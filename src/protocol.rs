use crate::{
    index::{Index, Progress},
    ops::{self, Action},
    query::{self, Query, Sort},
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    io::{self, BufRead, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
};

mod index_jobs;

type Output = Arc<Mutex<io::BufWriter<io::Stdout>>>;
struct Shared {
    state: PathBuf,
    root: PathBuf,
    index: RwLock<Option<Arc<Index>>>,
    progress: Arc<Mutex<Progress>>,
    scanning: AtomicBool,
    cancel: Arc<AtomicBool>,
    scan_thread: Mutex<Option<thread::JoinHandle<()>>>,
    job: Mutex<Option<index_jobs::JobStatus>>,
    stopping: AtomicBool,
    sessions: Mutex<HashMap<String, PathBuf>>,
    sequence: AtomicU64,
    writes: bool,
}

fn send(output: &Output, message: Value) {
    if let Ok(mut o) = output.lock() {
        let _ = serde_json::to_writer(&mut *o, &message);
        let _ = o.write_all(b"\n");
        let _ = o.flush();
    }
}
fn error(output: &Output, id: Value, code: i64, message: &str) {
    send(
        output,
        json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}}),
    );
}
fn respond(output: &Output, id: Value, result: Value) {
    send(output, json!({"jsonrpc":"2.0","id":id,"result":result}));
}

fn keys(value: &Value, allowed: &[&str]) -> Result<()> {
    let object = value.as_object().context("Arguments must be an object")?;
    ensure!(
        object.keys().all(|k| allowed.contains(&k.as_str())),
        "Unexpected argument"
    );
    Ok(())
}
fn integer(v: &Value, key: &str, default: u64, max: u64) -> Result<u64> {
    match v.get(key) {
        None => Ok(default),
        Some(n) => {
            let n = n.as_u64().context("Expected a nonnegative integer")?;
            ensure!(n <= max, "{key} is too large");
            Ok(n)
        }
    }
}
fn cursor(v: &Value) -> Result<Option<&str>> {
    v.get("cursor")
        .map(|value| value.as_str().context("cursor must be a string"))
        .transpose()
}
fn order(v: &Value, default: Sort) -> Result<Sort> {
    match v.get("sort") {
        None => Ok(default),
        Some(value) => match value.as_str() {
            Some("name") => Ok(Sort::Name),
            Some("size") => Ok(Sort::Size),
            Some("modified") => Ok(Sort::Modified),
            _ => bail!("Unknown sort"),
        },
    }
}
fn ids(v: &Value) -> Result<Vec<u32>> {
    let values = v["entry_ids"]
        .as_array()
        .context("entry_ids must be an array")?;
    ensure!(
        !values.is_empty() && values.len() <= 128,
        "Choose 1–128 entries"
    );
    values
        .iter()
        .map(|n| {
            n.as_u64()
                .filter(|n| *n < u32::MAX as u64)
                .map(|n| n as u32)
                .context("Invalid entry id")
        })
        .collect()
}

pub fn entry_json(index: &Index, id: u32) -> Result<Value> {
    let e = index.entry(id).context("Unknown entry id")?;
    Ok(
        json!({"id":id,"name":index.display_name(e),"path":crate::text(&index.path(id)?),"is_directory":e.is_dir(),"is_symlink":e.is_link(),
        "allocated_bytes":e.allocated,"logical_bytes":e.logical,"modified_unix":e.modified,"restricted":e.flags&crate::index::RESTRICTED!=0}),
    )
}

impl Shared {
    fn index(&self) -> Result<Arc<Index>> {
        self.index
            .read()
            .map_err(|_| anyhow::anyhow!("Index lock unavailable"))?
            .clone()
            .context("No index yet. Call axit_scan, then poll axit_status")
    }
    fn tool(self: &Arc<Self>, name: &str, args: &Value, generation: &AtomicU64) -> Result<Value> {
        let legacy = name
            .strip_prefix("scry_")
            .map(|suffix| format!("axit_{suffix}"));
        let name = legacy.as_deref().unwrap_or(name);
        match name {
            "axit_apps" => {
                keys(args, &["query", "limit"])?;
                let query = match args.get("query") {
                    Some(v) => v.as_str().context("query must be a string")?,
                    None => "",
                };
                ensure!(query.len() <= 4096, "Query is too long");
                let limit = integer(args, "limit", 100, 500)? as usize;
                ensure!(limit > 0, "limit must be at least 1");
                app_inventory(&self.root, query, limit)
            }
            "axit_app_files" | "axit_app_data_plan" => {
                keys(
                    args,
                    if name == "axit_app_files" {
                        &["app_key"]
                    } else {
                        &["app_key", "paths"]
                    },
                )?;
                let context = crate::apps::Context::system()?.constrained(&self.root)?;
                let catalog = crate::apps::inventory(&context);
                let key = args["app_key"]
                    .as_str()
                    .context("app_key must be a string")?;
                ensure!(key.len() <= 4096, "Application key is too long");
                let app = crate::apps::resolve(&catalog, key)?;
                let files = crate::apps::review_files(&context, &catalog, app);
                if name == "axit_app_files" {
                    return Ok(
                        json!({"key":app.key(),"application":app,"files":files,"note":"Scoped metadata only. Native install/uninstall uses axit apps in an interactive terminal."}),
                    );
                }
                let paths = args["paths"].as_array().context("paths must be an array")?;
                ensure!(
                    !paths.is_empty() && paths.len() <= 128,
                    "Choose 1–128 known related-data paths"
                );
                let paths=paths.iter().map(|p| {
                    let path=PathBuf::from(p.as_str().context("Each path must be a string")?);
                    ensure!(files.iter().any(|f|f.path==path&&f.selectable&&!matches!(f.kind,crate::apps::RelatedKind::Application|crate::apps::RelatedKind::Launcher)),"Choose a selectable related-data path from axit_app_files; app removal uses the interactive manager");
                    Ok(path)
                }).collect::<Result<Vec<_>>>()?;
                let index = self.index().ok();
                Ok(serde_json::to_value(ops::plan_app_trash(
                    &self.root,
                    &paths,
                    &self.state,
                    index.as_deref(),
                    Some(app),
                )?)?)
            }
            "axit_status" => {
                keys(args, &[])?;
                Ok(self.status())
            }
            "axit_scan" => {
                keys(args, &[])?;
                self.scan(None)
            }
            "axit_refresh" => {
                keys(args, &["paths"])?;
                let paths = args["paths"].as_array().context("paths must be an array")?;
                ensure!(
                    !paths.is_empty() && paths.len() <= 64,
                    "Choose 1–64 refresh paths"
                );
                let paths = paths
                    .iter()
                    .map(|p| {
                        let p = p.as_str().context("Each path must be a string")?;
                        ensure!(p.len() <= 4096, "Refresh path is too long");
                        Ok(PathBuf::from(p))
                    })
                    .collect::<Result<Vec<_>>>()?;
                self.refresh_index(&paths, &self.root, None)
            }
            "axit_cancel_index" => {
                keys(args, &[])?;
                self.cancel_index(None)
            }
            "axit_search" => {
                keys(args, &["query", "limit", "sort", "cursor"])?;
                let input = args["query"].as_str().context("query must be a string")?;
                let query = Query::parse(input)?;
                let limit = integer(args, "limit", 100, 500)? as usize;
                ensure!(limit > 0, "limit must be at least 1");
                let sort = order(args, Sort::Name)?;
                let index = self.index()?;
                let result = query::search_page(
                    &index,
                    &query,
                    sort,
                    crate::page::Request {
                        limit,
                        cursor: cursor(args)?,
                    },
                    &HashSet::new(),
                    Some((generation, 0)),
                )?;
                let entries = result
                    .ids
                    .iter()
                    .map(|id| entry_json(&index, *id))
                    .collect::<Result<Vec<_>>>()?;
                Ok(
                    json!({"matched":result.matched,"elapsed_ms":result.elapsed_ms,"canceled":result.canceled,"page":result.paging,"entries":entries}),
                )
            }
            "axit_browse" => {
                keys(args, &["entry_id", "limit", "sort", "cursor"])?;
                let id = integer(args, "entry_id", 0, u32::MAX as u64 - 1)? as u32;
                let limit = integer(args, "limit", 100, 500)? as usize;
                ensure!(limit > 0, "limit must be at least 1");
                let index = self.index()?;
                ensure!(
                    index.entry(id).is_some_and(|e| e.is_dir()),
                    "Choose a directory"
                );
                let result = index.children_page(
                    id,
                    order(args, Sort::Size)?,
                    crate::page::Request {
                        limit,
                        cursor: cursor(args)?,
                    },
                    &HashSet::new(),
                    Some((generation, 0)),
                )?;
                Ok(
                    json!({"folder":entry_json(&index,id)?,"matched":result.matched,"page":result.paging,"canceled":result.canceled,"children":result.ids.iter().map(|id|entry_json(&index,*id)).collect::<Result<Vec<_>>>()?,"note":"Child folders include their contents; parent and child totals overlap."}),
                )
            }
            "axit_cleanup_plan" | "axit_trash_plan" | "axit_move_plan" => {
                keys(
                    args,
                    if name == "axit_move_plan" {
                        &["entry_ids", "destination"]
                    } else if name == "axit_cleanup_plan" {
                        &["entry_ids", "minimum_age_hours"]
                    } else {
                        &["entry_ids"]
                    },
                )?;
                let ids = ids(args)?;
                let index = self.index()?;
                let action = if name == "axit_cleanup_plan" {
                    Action::Clean
                } else if name == "axit_move_plan" {
                    Action::Move {
                        destination: args["destination"]
                            .as_str()
                            .context("destination must be an absolute folder path")?
                            .into(),
                    }
                } else {
                    Action::Trash
                };
                let age = integer(args, "minimum_age_hours", 24, 87600)? as i64 * 3600;
                ensure!(age >= 3600, "Cleanup age must be at least one hour");
                Ok(serde_json::to_value(ops::plan(
                    &index,
                    &ids,
                    action,
                    &self.state,
                    age,
                )?)?)
            }
            "axit_apply_plan" => {
                keys(args, &["plan_id", "confirm"])?;
                ensure!(
                    self.writes,
                    "Mutations are disabled. Review/apply in the TUI, or explicitly start this server with --allow-mutations"
                );
                let plan = ops::load_plan(
                    &self.state,
                    args["plan_id"].as_str().context("Missing plan_id")?,
                )?;
                ensure!(
                    ops::decode(&plan.root)? == self.root,
                    "Plan belongs to a different scope"
                );
                Ok(serde_json::to_value(ops::apply(
                    &self.state,
                    &plan,
                    args["confirm"].as_str().context("Missing confirmation")?,
                )?)?)
            }
            "axit_undo" => {
                keys(args, &[])?;
                ensure!(self.writes, "Mutations are disabled");
                ops::undo_in(&self.state, Some(&self.root))
            }
            _ => bail!("Unknown tool: {name}"),
        }
    }
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str], write: bool) -> Value {
    json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false},
        "annotations":{"readOnlyHint":!write,"destructiveHint":write,"idempotentHint":!write,"openWorldHint":false}})
}

pub fn tools(writes: bool) -> Vec<Value> {
    let list =
        json!({"type":"array","items":{"type":"integer","minimum":0},"minItems":1,"maxItems":128});
    let mut tools = vec![
        tool(
            "axit_apps",
            "List bounded application metadata inside the server's fixed filesystem scope. No native commands or launcher Exec fields run.",
            json!({"query":{"type":"string","maxLength":4096},"limit":{"type":"integer","minimum":1,"maximum":500}}),
            &[],
            false,
        ),
        tool(
            "axit_app_files",
            "Inspect known related files inside the fixed scope. User data and uncertain matches are identified; native uninstall remains interactive.",
            json!({"app_key":{"type":"string","maxLength":4096}}),
            &["app_key"],
            false,
        ),
        tool(
            "axit_app_data_plan",
            "Preview reversible Trash of explicitly selected related-data paths returned by axit_app_files. App bundles and launchers use the interactive manager. Does not move files.",
            json!({"app_key":{"type":"string","maxLength":4096},"paths":{"type":"array","items":{"type":"string","maxLength":4096},"minItems":1,"maxItems":128}}),
            &["app_key", "paths"],
            false,
        ),
        tool(
            "axit_status",
            "Inspect index progress, permission gaps and filesystem free space.",
            json!({}),
            &[],
            false,
        ),
        tool(
            "axit_scan",
            "Start a read-only background scan of the server's fixed scope. Poll axit_status.",
            json!({}),
            &[],
            false,
        ),
        tool(
            "axit_refresh",
            "Update selected metadata paths in the fixed root without rebuilding the base index. Relative paths use that root. Background job; poll axit_status for completion/report. No user files change.",
            json!({"paths":{"type":"array","items":{"type":"string","minLength":1,"maxLength":4096},"minItems":1,"maxItems":64}}),
            &["paths"],
            false,
        ),
        tool(
            "axit_cancel_index",
            "Request cancellation of the active metadata scan or refresh. Poll axit_status; cancellation before publication preserves the previous snapshot.",
            json!({}),
            &[],
            false,
        ),
        tool(
            "axit_search",
            "Find indexed filenames. Supports quoted words, globs, ext:, size:, older:, type:, path:, !term and re:.",
            json!({"query":{"type":"string","maxLength":4096},"limit":{"type":"integer","minimum":1,"maximum":500},"sort":{"enum":["name","size","modified"]},"cursor":{"type":"string","maxLength":178}}),
            &["query"],
            false,
        ),
        tool(
            "axit_browse",
            "List the largest children of a directory; allocated folder sizes overlap.",
            json!({"entry_id":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":500},"sort":{"enum":["name","size","modified"]},"cursor":{"type":"string","maxLength":178}}),
            &[],
            false,
        ),
        tool(
            "axit_cleanup_plan",
            "Preview verified disposable-cache cleanup. Does not delete files. Active files, source, archives, keys and models are protected.",
            json!({"entry_ids":list.clone(),"minimum_age_hours":{"type":"integer","minimum":1,"maximum":87600}}),
            &["entry_ids"],
            false,
        ),
        tool(
            "axit_trash_plan",
            "Preview moving selected entries to Trash. Does not move files or release disk space.",
            json!({"entry_ids":list.clone()}),
            &["entry_ids"],
            false,
        ),
        tool(
            "axit_move_plan",
            "Preview organizing selected entries into an existing destination folder inside the indexed scope. Never overwrites.",
            json!({"entry_ids":list,"destination":{"type":"string"}}),
            &["entry_ids", "destination"],
            false,
        ),
    ];
    if writes {
        tools.push(tool("axit_apply_plan","Apply a user-reviewed, unexpired plan. Requires the exact confirmation phrase CLEAN, TRASH or MOVE. Permanent cache cleanup cannot be undone.",json!({"plan_id":{"type":"string"},"confirm":{"enum":["CLEAN","TRASH","MOVE"]}}),&["plan_id","confirm"],true));
        tools.push(tool(
            "axit_undo",
            "Undo the last reversible move without overwriting an existing destination.",
            json!({}),
            &[],
            true,
        ));
    }
    tools
}

fn app_inventory(root: &Path, query: &str, limit: usize) -> Result<Value> {
    let context = crate::apps::Context::system()?.constrained(root)?;
    let catalog = crate::apps::inventory(&context);
    let query = query.to_lowercase();
    let apps: Vec<_> = catalog
        .apps
        .iter()
        .filter(|a| {
            format!("{} {}", a.name, a.id)
                .to_lowercase()
                .contains(&query)
        })
        .collect();
    Ok(
        json!({"matched":apps.len(),"apps":apps.iter().take(limit).map(|a|json!({"key":a.key(),"application":a})).collect::<Vec<_>>(),"warnings":catalog.warnings,"scope":crate::text(root),"note":"Bounded scoped metadata. Native package installation/uninstallation is interactive in axit apps."}),
    )
}

fn acp_prompt(
    shared: &Arc<Shared>,
    session: &str,
    params: &Value,
    cancel: &AtomicU64,
) -> Result<Value> {
    let cwd = shared
        .sessions
        .lock()
        .map_err(|_| anyhow::anyhow!("Session lock unavailable"))?
        .get(session)
        .cloned()
        .context("Unknown session")?;
    let content = params["prompt"]
        .as_array()
        .context("prompt must contain text blocks")?;
    ensure!(content.len() <= 100, "Too many prompt blocks");
    let mut text = String::new();
    for block in content {
        ensure!(
            block["type"] == "text",
            "This adapter accepts text commands only"
        );
        text.push_str(block["text"].as_str().context("Missing prompt text")?);
        text.push(' ');
    }
    ensure!(text.len() <= 4096, "Prompt is too long");
    let input = text.trim();
    if input == "/status" {
        return Ok(shared.status_for(Some(session)));
    }
    if input == "/scan" {
        return shared.scan(Some(session));
    }
    if input == "/cancel-index" {
        return shared.cancel_index(Some(session));
    }
    if let Some(path) = input
        .strip_prefix("/refresh")
        .filter(|p| p.is_empty() || p.starts_with(' '))
    {
        let path = if path.trim().is_empty() {
            PathBuf::from(".")
        } else {
            PathBuf::from(path.trim())
        };
        return shared.refresh_index(&[path], &cwd, Some(session));
    }
    if input == "/apps" || input.starts_with("/apps ") {
        return app_inventory(&cwd, input.strip_prefix("/apps").unwrap_or("").trim(), 100);
    }
    if let Some(key) = input.strip_prefix("/app ") {
        let context = crate::apps::Context::system()?.constrained(&cwd)?;
        let catalog = crate::apps::inventory(&context);
        let app = crate::apps::resolve(&catalog, key.trim())?;
        return Ok(
            json!({"key":app.key(),"application":app,"files":crate::apps::review_files(&context,&catalog,app)}),
        );
    }
    let index = shared.index()?;
    let scope = index
        .find_path(&cwd)
        .context("Session folder is not in this index; scan it first")?;
    if let Some(arguments) = input.strip_prefix("/plan ") {
        let ids = arguments
            .split_whitespace()
            .map(str::parse::<u32>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ensure!(
            ids.iter().all(|id| index.is_descendant(*id, scope)),
            "Entry is outside the session scope"
        );
        return Ok(serde_json::to_value(ops::plan(
            &index,
            &ids,
            Action::Clean,
            &shared.state,
            86400,
        )?)?);
    }
    let (input, sort) = if let Some(rest) = input
        .strip_prefix("/largest")
        .filter(|rest| rest.is_empty() || rest.starts_with(' '))
    {
        (rest.trim(), Sort::Size)
    } else {
        (input.strip_prefix("/search ").unwrap_or(input), Sort::Name)
    };
    let (input, cursor) = if let Some(rest) = input.strip_prefix("--cursor ") {
        let (token, query) = rest.split_once(' ').unwrap_or((rest, ""));
        (query.trim(), Some(token))
    } else {
        (input, None)
    };
    ensure!(
        sort != Sort::Size || input.is_empty(),
        "Use /largest [--cursor TOKEN]"
    );
    let mut query = Query::parse(input)?;
    query.set_scope(scope);
    let result = query::search_page(
        &index,
        &query,
        sort,
        crate::page::Request { limit: 100, cursor },
        &HashSet::new(),
        Some((cancel, 0)),
    )?;
    Ok(
        json!({"matched":result.matched,"elapsed_ms":result.elapsed_ms,"canceled":result.canceled,"page":result.paging,"entries":result.ids.iter().map(|id|entry_json(&index,*id)).collect::<Result<Vec<_>>>()?,
        "commands":"/search [--cursor TOKEN] QUERY · /largest [--cursor TOKEN] · /status · /scan · /refresh [PATH] · /cancel-index · /apps [QUERY] · /app KEY · /plan ID… · deterministic metadata adapter; no model or file contents required"}),
    )
}

/// Local newline-delimited JSON-RPC. stdout is reserved for protocol messages.
pub fn serve(state: PathBuf, root: PathBuf, acp: bool, writes: bool) -> Result<()> {
    let root = root.canonicalize()?;
    let index = Index::open(&crate::index_path(&state))
        .ok()
        .filter(|i| i.path(0).is_ok_and(|p| p == root))
        .map(Arc::new);
    let shared = Arc::new(Shared {
        state,
        root,
        index: RwLock::new(index),
        progress: Arc::new(Mutex::new(Progress::default())),
        scanning: AtomicBool::new(false),
        cancel: Arc::new(AtomicBool::new(false)),
        scan_thread: Mutex::new(None),
        job: Mutex::new(None),
        stopping: AtomicBool::new(false),
        sessions: Mutex::new(HashMap::new()),
        sequence: AtomicU64::new(0),
        writes,
    });
    let output = Arc::new(Mutex::new(io::BufWriter::new(io::stdout())));
    let jobs = Arc::new(Mutex::new(HashMap::<String, Arc<AtomicU64>>::new()));
    let active = Arc::new(AtomicUsize::new(0));
    let mut initialized = false;
    let mut workers = Vec::new();
    let stdin = io::stdin();
    let mut input = stdin.lock();
    loop {
        let mut line = Vec::new();
        let n = (&mut input)
            .take(1024 * 1024 + 1)
            .read_until(b'\n', &mut line)?;
        if n == 0 {
            break;
        }
        if line.len() > 1024 * 1024 {
            if line.last() != Some(&b'\n') {
                let mut discard = Vec::new();
                while {
                    discard.clear();
                    let n = (&mut input).take(4096).read_until(b'\n', &mut discard)?;
                    n > 0 && discard.last() != Some(&b'\n')
                } {}
            }
            error(&output, Value::Null, -32600, "Message exceeds 1 MiB");
            continue;
        }
        let request: Value = match serde_json::from_slice(&line) {
            Ok(v) => v,
            Err(_) => {
                error(&output, Value::Null, -32700, "Invalid JSON");
                continue;
            }
        };
        let id = request.get("id").cloned();
        let method = request["method"].as_str().unwrap_or("").to_string();
        if request["jsonrpc"] != "2.0"
            || method.is_empty()
            || id
                .as_ref()
                .is_some_and(|v| !v.is_string() && !v.is_i64() && !v.is_u64())
        {
            error(&output, Value::Null, -32600, "Invalid JSON-RPC request");
            continue;
        }
        let params = request.get("params").cloned().unwrap_or(json!({}));
        if id.is_none() {
            if method == "notifications/cancelled"
                && let Some(key) = params.get("requestId").map(Value::to_string)
                && let Ok(j) = jobs.lock()
                && let Some(cancel) = j.get(&key)
            {
                cancel.store(1, Ordering::Relaxed);
            }
            if method == "session/cancel"
                && let Some(session) = params["sessionId"].as_str()
                && let Ok(j) = jobs.lock()
                && let Some(cancel) = j.get(&format!("acp:{session}"))
            {
                cancel.store(1, Ordering::Relaxed);
            }
            if acp
                && method == "session/cancel"
                && let Some(session) = params["sessionId"].as_str()
            {
                let _ = shared.cancel_index(Some(session));
            }
            continue;
        }
        let id = id.unwrap_or(Value::Null);
        if method == "initialize" {
            if initialized {
                error(&output, id, -32600, "Already initialized");
                continue;
            }
            if !params.is_object()
                || (acp && !params["protocolVersion"].is_u64())
                || (!acp && !params["protocolVersion"].is_string())
            {
                error(&output, id, -32602, "Missing or invalid protocolVersion");
                continue;
            }
            initialized = true;
            let result = if acp {
                json!({"protocolVersion":1,"agentCapabilities":{"loadSession":false,"promptCapabilities":{"image":false,"audio":false,"embeddedContext":false},"mcpCapabilities":{"http":false,"sse":false}},"agentInfo":{"name":"axit","title":"AXIT / WCKD.ai","version":crate::VERSION},"authMethods":[]})
            } else {
                let version = params["protocolVersion"]
                    .as_str()
                    .filter(|s| ["2025-11-25", "2025-06-18", "2024-11-05"].contains(s))
                    .unwrap_or("2025-11-25");
                json!({"protocolVersion":version,"capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"axit","version":crate::VERSION},"instructions":"Metadata-only filesystem search and storage analysis. Scans report permission gaps. Mutations are disabled by default; review plans in the TUI."})
            };
            respond(&output, id, result);
            continue;
        }
        if !initialized {
            error(&output, id, -32002, "Initialize first");
            continue;
        }
        if method == "ping" {
            respond(&output, id, json!({}));
            continue;
        }
        if !acp && method == "tools/list" {
            respond(&output, id, json!({"tools":tools(writes)}));
            continue;
        }
        if acp && method == "authenticate" {
            respond(&output, id, json!({}));
            continue;
        }
        if acp && method == "session/new" {
            let result = (|| -> Result<String> {
                let cwd = Path::new(params["cwd"].as_str().context("Missing absolute cwd")?);
                ensure!(cwd.is_absolute(), "cwd must be absolute");
                let cwd = crate::scoped_alias(&cwd.canonicalize()?, &shared.root)?;
                ensure!(
                    cwd.starts_with(&shared.root),
                    "Session cwd is outside server scope"
                );
                ensure!(
                    params["mcpServers"].as_array().is_none_or(|v| v.is_empty()),
                    "AXIT does not start external MCP servers"
                );
                let session = format!(
                    "axit-{}-{}",
                    crate::unix_now(),
                    shared.sequence.fetch_add(1, Ordering::Relaxed)
                );
                shared
                    .sessions
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Session lock unavailable"))?
                    .insert(session.clone(), cwd);
                Ok(session)
            })();
            match result {
                Ok(session) => {
                    respond(&output, id, json!({"sessionId":session}));
                    send(
                        &output,
                        json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":session,"update":{"sessionUpdate":"available_commands_update","availableCommands":[{"name":"apps","description":"List scoped application metadata","input":{"hint":"optional name"}},{"name":"app","description":"Inspect scoped related files","input":{"hint":"app key"}},{"name":"search","description":"Find indexed filenames","input":{"hint":"[--cursor TOKEN] query"}},{"name":"largest","description":"Show largest entries","input":{"hint":"[--cursor TOKEN]"}},{"name":"status","description":"Index, disk space and coverage"},{"name":"scan","description":"Rebuild the fixed server scan scope"},{"name":"refresh","description":"Update this session folder or one path within it","input":{"hint":"optional relative path; spaces are literal"}},{"name":"cancel-index","description":"Cancel this session’s active scan or refresh"},{"name":"plan","description":"Preview safe cache cleanup","input":{"hint":"entry ids"}}]}}}),
                    );
                }
                Err(e) => error(&output, id, -32602, &e.to_string()),
            }
            continue;
        }
        if (acp && method != "session/prompt") || (!acp && method != "tools/call") {
            error(&output, id, -32601, "Method not found");
            continue;
        }
        if active.load(Ordering::Acquire) >= 4 {
            error(
                &output,
                id,
                -32000,
                "At most four requests may run concurrently",
            );
            continue;
        }
        let key = if acp {
            format!("acp:{}", params["sessionId"].as_str().unwrap_or(""))
        } else {
            id.to_string()
        };
        let cancel = Arc::new(AtomicU64::new(0));
        if let Ok(mut registry) = jobs.lock() {
            if registry.contains_key(&key) {
                error(
                    &output,
                    id,
                    -32000,
                    "A request with this id/session is already running",
                );
                continue;
            }
            registry.insert(key.clone(), cancel.clone());
        }
        active.fetch_add(1, Ordering::AcqRel);
        let (shared, output, jobs, active) =
            (shared.clone(), output.clone(), jobs.clone(), active.clone());
        workers.push(thread::spawn(move||{
            if acp {
                let session=params["sessionId"].as_str().unwrap_or("");
                match acp_prompt(&shared,session,&params,&cancel){
                    Ok(value)=>{send(&output,json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":session,"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":serde_json::to_string_pretty(&value).unwrap_or_default()}}}}));respond(&output,id,json!({"stopReason":if cancel.load(Ordering::Relaxed)!=0{"cancelled"}else{"end_turn"}}));}
                    Err(e)=>error(&output,id,-32602,&format!("{e:#}")),
                }
            }else{
                let name=params["name"].as_str().unwrap_or("");let args=params.get("arguments").cloned().unwrap_or(json!({}));
                match shared.tool(name,&args,&cancel){
                    Ok(value)=>respond(&output,id,json!({"content":[{"type":"text","text":value.to_string()}],"structuredContent":value,"isError":false})),
                    Err(e)=>respond(&output,id,json!({"content":[{"type":"text","text":format!("{e:#}")}],"isError":true})),
                }
            }
            if let Ok(mut registry)=jobs.lock(){registry.remove(&key);}active.fetch_sub(1,Ordering::Release);
        }));
        let mut i = 0;
        while i < workers.len() {
            if workers[i].is_finished() {
                let handle = workers.swap_remove(i);
                let _ = handle.join();
            } else {
                i += 1;
            }
        }
    }
    shared.stopping.store(true, Ordering::Release);
    shared.cancel.store(true, Ordering::Relaxed);
    if let Ok(registry) = jobs.lock() {
        for cancel in registry.values() {
            cancel.store(1, Ordering::Relaxed);
        }
    }
    for worker in workers {
        let _ = worker.join();
    }
    if let Some(handle) = shared
        .scan_thread
        .lock()
        .map_err(|_| anyhow::anyhow!("Scan lock unavailable"))?
        .take()
    {
        let _ = handle.join();
    }
    Ok(())
}
