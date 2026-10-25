use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand, ValueEnum};
use std::{
    collections::HashSet,
    io::{self, IsTerminal},
    path::PathBuf,
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::{Duration, Instant},
};
use wckd_axit::{
    self as axit,
    index::{Index, Progress},
    ops::{self, Action},
    query::{self, Query, Sort},
};

#[derive(Parser)]
#[command(
    name = "axit",
    version,
    about = "See what is hiding on your disk. / WCKD.ai",
    long_about = "A fast, local terminal file finder and storage browser. Scans read metadata only. File operations require a reviewed plan; automatic cleanup is off by default."
)]
struct Cli {
    #[arg(
        long,
        global = true,
        value_name = "PATH",
        help = "Scan scope. Defaults to the last index root, then your home"
    )]
    root: Option<PathBuf>,
    #[arg(
        long,
        global = true,
        value_name = "PATH",
        help = "Private state/index directory"
    )]
    state_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Clone, ValueEnum)]
enum Order {
    Name,
    Size,
    Modified,
}
impl From<Order> for Sort {
    fn from(v: Order) -> Self {
        match v {
            Order::Name => Self::Name,
            Order::Size => Self::Size,
            Order::Modified => Self::Modified,
        }
    }
}
#[derive(Clone, ValueEnum)]
enum Operation {
    Clean,
    Trash,
    Move,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage applications, related files and reviewed native installation.
    Apps {
        #[command(subcommand)]
        action: Option<AppCommand>,
    },
    /// Open the keyboard-first storage browser (also the default).
    Tui,
    /// Build an immutable, bounded metadata index. Previous index survives failure.
    Scan,
    /// Search indexed names: ext:pdf size:>1gib older:30d type:dir path:project.
    Search {
        query: String,
        #[arg(long, default_value = "100")]
        limit: usize,
        #[arg(long, value_enum, default_value = "name")]
        sort: Order,
        #[arg(long)]
        json: bool,
    },
    /// Browse the largest children of an indexed directory id.
    Browse {
        #[arg(default_value = "0")]
        id: u32,
        #[arg(long, default_value = "100")]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Disk usage, index size, scope, permission failures and automatic-cleanup policy.
    Status,
    /// Inspect physical volume layout and scan restrictions. Does not request privilege.
    Doctor,
    /// Preview a file operation. No selected files are changed.
    Plan {
        #[arg(required=true,num_args=1..)]
        ids: Vec<u32>,
        #[arg(long, value_enum, default_value = "clean")]
        action: Operation,
        #[arg(long)]
        to: Option<PathBuf>,
        #[arg(long, default_value = "24")]
        minimum_age_hours: u32,
    },
    /// Apply an unexpired preview. Requires CLEAN, TRASH or MOVE exactly.
    Apply {
        plan_id: String,
        #[arg(long)]
        confirm: String,
    },
    /// Undo the last reversible move; never overwrites an existing file.
    Undo,
    /// Configure opt-in low-space cleanup, or run a single automatic check.
    Auto {
        #[arg(long, conflicts_with = "disable")]
        enable: bool,
        #[arg(long)]
        disable: bool,
        #[arg(long, value_name = "PROJECT")]
        allow: Vec<PathBuf>,
        #[arg(long)]
        run: bool,
        #[arg(long)]
        watch: bool,
    },
    /// Serve MCP tools over stdio. Filesystem mutations are disabled by default.
    Mcp {
        #[arg(long)]
        allow_mutations: bool,
    },
    /// Serve a deterministic Agent Client Protocol v1 command adapter over stdio.
    Acp,
}

#[derive(Subcommand)]
enum AppCommand {
    /// List installed application identities and stable selection keys.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Inspect exact and possible related files; never executes a launcher.
    Inspect {
        app: String,
        #[arg(long)]
        leftover: bool,
    },
    /// List remembered and unmatched data candidates; absence is not proof of disuse.
    Leftovers {
        #[arg(long)]
        json: bool,
    },
    /// Preview installation; --confirm INSTALL runs the native manager interactively.
    Install {
        package: String,
        #[arg(long)]
        provider: Option<axit::apps::Provider>,
        #[arg(long)]
        confirm: Option<String>,
    },
    /// Preview removal. Native managers keep user data; standalone bundles use reversible Trash.
    Uninstall {
        app: String,
        #[arg(long)]
        include_data: bool,
        #[arg(long)]
        include_review: bool,
        #[arg(long)]
        confirm: Option<String>,
    },
    /// Preview related-data Trash, optionally from leftover candidates.
    TrashData {
        app: String,
        #[arg(long)]
        leftover: bool,
        #[arg(long)]
        include_data: bool,
        #[arg(long)]
        include_review: bool,
        #[arg(long)]
        confirm: Option<String>,
    },
}

fn apps_cli(action: Option<AppCommand>, root: Option<PathBuf>, state: PathBuf) -> Result<()> {
    use axit::apps;
    let Some(action) = action else {
        ensure!(
            io::stdout().is_terminal() && io::stdin().is_terminal(),
            "Apps needs an interactive terminal; use apps list/inspect for scripts"
        );
        let root = root
            .or_else(|| {
                current(&state)
                    .ok()
                    .and_then(|i| i.path(0).ok())
                    .filter(|p| p.is_dir())
            })
            .map_or_else(axit::home, Ok)?
            .canonicalize()
            .context("Root folder is unavailable")?;
        return axit::tui::run_apps(root, state);
    };
    let context = apps::Context::system()?;
    if let AppCommand::Install {
        package,
        provider,
        confirm,
    } = action
    {
        let provider = provider
            .or_else(|| context.tools.available().first().copied())
            .context("Install Homebrew on macOS or Flatpak on Linux first")?;
        let plan = apps::install_plan(&context.tools, provider, &package)?;
        return if let Some(confirm) = confirm {
            plan.run(&state, &confirm)
        } else {
            output(plan)
        };
    }
    let catalog = apps::inventory(&context);
    match action {
        AppCommand::List { json } | AppCommand::Leftovers { json } => {
            let list = if matches!(action, AppCommand::Leftovers { .. }) {
                apps::Catalog {
                    apps: apps::leftovers(&context, &catalog, &state)?,
                    warnings: catalog.warnings.clone(),
                }
            } else {
                catalog
            };
            if json {
                output(
                    serde_json::json!({"apps":list.apps.iter().map(|a|serde_json::json!({"key":a.key(),"application":a})).collect::<Vec<_>>(),"warnings":list.warnings}),
                )
            } else {
                for app in list.apps {
                    println!("{}\t{}\t{}", app.key(), app.source_label(), app.name);
                }
                for warning in list.warnings {
                    eprintln!("{warning}");
                }
                Ok(())
            }
        }
        AppCommand::Inspect { ref app, leftover } => {
            let data = if leftover {
                apps::Catalog {
                    apps: apps::leftovers(&context, &catalog, &state)?,
                    warnings: catalog.warnings.clone(),
                }
            } else {
                catalog.clone()
            };
            let app = apps::resolve(&data, app)?;
            output(
                serde_json::json!({"key":app.key(),"application":app,"files":apps::review_files(&context,&catalog,app),"warnings":catalog.warnings}),
            )
        }
        AppCommand::Uninstall { .. } | AppCommand::TrashData { .. } => {
            let (query, leftover, include_data, include_review, confirm, uninstall) = match action {
                AppCommand::Uninstall {
                    app,
                    include_data,
                    include_review,
                    confirm,
                } => (app, false, include_data, include_review, confirm, true),
                AppCommand::TrashData {
                    app,
                    leftover,
                    include_data,
                    include_review,
                    confirm,
                } => (app, leftover, include_data, include_review, confirm, false),
                _ => unreachable!(),
            };
            let data = if leftover {
                apps::Catalog {
                    apps: apps::leftovers(&context, &catalog, &state)?,
                    warnings: catalog.warnings.clone(),
                }
            } else {
                catalog.clone()
            };
            let app = apps::resolve(&data, &query)?;
            if uninstall
                && matches!(
                    app.source,
                    apps::Source::Brew { .. } | apps::Source::Flatpak { .. }
                )
            {
                ensure!(
                    !include_data && !include_review,
                    "Native uninstall keeps user data; review apps leftovers / trash-data afterward"
                );
                let plan = apps::uninstall_plan(&context.tools, app)?;
                return if let Some(confirm) = confirm {
                    plan.run(&state, &confirm)
                } else {
                    output(plan)
                };
            }
            ensure!(
                !uninstall || matches!(app.source, apps::Source::Bundle) && !app.protected,
                "Use this application's native installer; Files can review related data"
            );
            let files = apps::review_files(&context, &catalog, app);
            let paths: Vec<_> = files
                .iter()
                .filter(|f| {
                    f.selectable
                        && (if f.kind == apps::RelatedKind::Application {
                            uninstall
                        } else {
                            (apps::default_file(app, f, false) || include_data || include_review)
                                && (!f.contains_user_data || include_data)
                                && (f.confidence == apps::Confidence::Exact || include_review)
                        })
                })
                .map(|f| f.path.clone())
                .collect();
            ensure!(
                !paths.is_empty(),
                "No selected files; inspect the app and opt into personal data or uncertain matches explicitly"
            );
            let index = current(&state).ok();
            let plan = ops::plan_app_trash(
                std::path::Path::new("/"),
                &paths,
                &state,
                index.as_ref(),
                Some(app),
            )?;
            if let Some(confirm) = confirm {
                output(ops::apply(&state, &plan, &confirm)?)
            } else {
                output(plan)
            }
        }
        AppCommand::Install { .. } => unreachable!(),
    }
}

fn output(value: impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
fn current(state: &std::path::Path) -> Result<Index> {
    Index::open(&axit::index_path(state))
}

fn main() {
    if let Err(error) = run() {
        eprintln!("axit: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let state = if let Some(path) = cli.state_dir {
        std::fs::create_dir_all(&path)?;
        path.canonicalize()?
    } else {
        axit::state_dir()?
    };
    if matches!(cli.command, Some(Commands::Apps { .. })) {
        let Some(Commands::Apps { action }) = cli.command else {
            unreachable!()
        };
        return apps_cli(action, cli.root, state);
    }
    let root = cli
        .root
        .or_else(|| current(&state).ok().and_then(|i| i.path(0).ok()))
        .map_or_else(axit::home, Ok)?
        .canonicalize()
        .context("Root folder is unavailable")?;
    match cli.command.unwrap_or(Commands::Tui) {
        Commands::Apps { action } => apps_cli(action, Some(root), state),
        Commands::Tui => {
            ensure!(
                io::stdout().is_terminal() && io::stdin().is_terminal(),
                "The TUI needs an interactive terminal. Use axit scan/search/status for scripts"
            );
            axit::tui::run(root, state)
        }
        Commands::Scan => {
            let progress = Arc::new(Mutex::new(Progress::default()));
            let cancel = Arc::new(AtomicBool::new(false));
            signal_hook::flag::register(signal_hook::consts::SIGINT, cancel.clone())?;
            signal_hook::flag::register(signal_hook::consts::SIGTERM, cancel.clone())?;
            let started = Instant::now();
            eprintln!("AXIT · scanning {} · metadata only", axit::text(&root));
            let path = axit::index::scan(&root, &state, progress, cancel)?;
            let index = Index::open(&path)?;
            output(
                serde_json::json!({"root":axit::text(&root),"entries":index.count,"allocated_bytes":index.entry(0).map(|e|e.allocated),"restricted_or_missing":index.errors,"elapsed_seconds":started.elapsed().as_secs_f64(),"index_bytes":index.disk_bytes(),"index":axit::text(&path),"coverage":"Other mounts and symlinks are not followed. Permission failures appear in latest-scan.json."}),
            )
        }
        Commands::Search {
            query,
            limit,
            sort,
            json,
        } => {
            let index = current(&state)?;
            let q = Query::parse(&query)?;
            let result = query::search(&index, &q, sort.into(), limit, &HashSet::new(), None);
            if json {
                output(
                    serde_json::json!({"matched":result.matched,"elapsed_ms":result.elapsed_ms,"entries":result.ids.iter().map(|id|axit::protocol::entry_json(&index,*id)).collect::<Result<Vec<_>>>()?}),
                )
            } else {
                for id in result.ids {
                    let e = index.entry(id).context("Missing entry")?;
                    println!(
                        "{:>9}  {:>12}  {}",
                        id,
                        axit::bytes(e.allocated),
                        axit::text(&index.path(id)?)
                    );
                }
                eprintln!("{} matches · {:.1} ms", result.matched, result.elapsed_ms);
                Ok(())
            }
        }
        Commands::Browse { id, limit, json } => {
            let index = current(&state)?;
            ensure!(
                index.entry(id).is_some_and(|e| e.is_dir()),
                "Choose a directory id"
            );
            let children = index.children(id, limit.clamp(1, 1000));
            if json {
                output(
                    children
                        .iter()
                        .map(|e| axit::protocol::entry_json(&index, e.id))
                        .collect::<Result<Vec<_>>>()?,
                )
            } else {
                println!("{}", axit::text(&index.path(id)?));
                for e in children {
                    println!(
                        "{:>9}  {:>12}  {}{}",
                        e.id,
                        axit::bytes(e.allocated),
                        if e.is_dir() { "▸ " } else { "  " },
                        index.display_name(e)
                    );
                }
                Ok(())
            }
        }
        Commands::Status => {
            let index = current(&state).ok();
            output(
                serde_json::json!({"name":"AXIT / WCKD.ai","version":axit::VERSION,"root":axit::text(&root),"volume":axit::system::volume(&root)?,"entries":index.as_ref().map(|i|i.count),"index_bytes":index.as_ref().map(|i|i.disk_bytes()),"restricted_or_missing":index.as_ref().map(|i|i.errors),"automatic_cleanup":ops::auto_policy(&state)?}),
            )
        }
        Commands::Doctor => {
            let restrictions = std::fs::read(state.join("latest-scan.json"))
                .ok()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
            output(
                serde_json::json!({"version":axit::VERSION,"root":axit::text(&root),"state":axit::text(&state),"volume":axit::system::volume(&root)?,"layout":axit::system::disk_layout().ok(),"last_scan":restrictions,"active_file_checks":match axit::system::OpenFiles::capture(){Ok(o)=>format!("Available · {} current-user paths",o.paths.len()),Err(e)=>e.to_string()},"macos_access":"For a complete Mac scan, grant Full Disk Access to your terminal in System Settings → Privacy & Security. Administrator access alone does not bypass privacy protection. AXIT never requests or stores passwords."}),
            )
        }
        Commands::Plan {
            ids,
            action,
            to,
            minimum_age_hours,
        } => {
            let action = match action {
                Operation::Clean => Action::Clean,
                Operation::Trash => Action::Trash,
                Operation::Move => Action::Move {
                    destination: to
                        .context("Move requires --to PATH")?
                        .to_str()
                        .context("Destination must be UTF-8")?
                        .into(),
                },
            };
            output(ops::plan(
                &current(&state)?,
                &ids,
                action,
                &state,
                minimum_age_hours as i64 * 3600,
            )?)
        }
        Commands::Apply { plan_id, confirm } => output(ops::apply(
            &state,
            &ops::load_plan(&state, &plan_id)?,
            &confirm,
        )?),
        Commands::Undo => output(ops::undo(&state)?),
        Commands::Auto {
            enable,
            disable,
            allow,
            run,
            watch,
        } => {
            let mut policy = ops::auto_policy(&state)?;
            if enable || disable || !allow.is_empty() {
                if enable {
                    policy.enabled = true;
                }
                if disable {
                    policy.enabled = false;
                }
                if !allow.is_empty() {
                    policy.roots = allow
                        .iter()
                        .map(|p| p.canonicalize())
                        .collect::<std::io::Result<Vec<_>>>()?
                        .iter()
                        .map(|p| {
                            p.to_str()
                                .map(String::from)
                                .context("Policy roots must be UTF-8")
                        })
                        .collect::<Result<_>>()?;
                }
                ops::save_auto(&state, &policy)?;
            }
            if watch {
                eprintln!("AXIT automatic checks · every five minutes · Ctrl-C to stop");
                loop {
                    match ops::auto_run(&state, &current(&state)?) {
                        Ok(Some(r)) => output(r)?,
                        Ok(None) => {}
                        Err(e) => eprintln!("axit auto: {e:#}"),
                    }
                    std::thread::sleep(Duration::from_secs(300));
                }
            }
            if run {
                output(ops::auto_run(&state, &current(&state)?)?)
            } else {
                output(policy)
            }
        }
        Commands::Mcp { allow_mutations } => {
            axit::protocol::serve(state, root, false, allow_mutations)
        }
        Commands::Acp => axit::protocol::serve(state, root, true, false),
    }
}
