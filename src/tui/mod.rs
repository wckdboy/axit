mod applications;
mod editor;
mod logo;
mod navigation;
mod pages;
mod render;
mod rows;
mod state;
#[cfg(test)]
mod tests;
mod theme;
mod usage;

use crate::{
    bytes,
    index::{Index, Progress},
    ops::{self, Action, Plan},
    query::{self, Sort},
};
use anyhow::Result;
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event,
    },
    execute,
};
use editor::Editor;
use ratatui::{layout::Rect, widgets::TableState};
pub use render::draw;
use std::{
    collections::HashSet,
    io,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum View {
    Space,
    Find,
    Clean,
    Activity,
    Apps,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Nav,
    Query,
    List,
    Inspect,
    Actions,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    Back,
    Forward,
    Parent,
    Root,
    Open,
    Reveal,
    Find,
    Map,
    Select,
    Trash,
    Move,
    Clean,
    Undo,
    Rescan,
    Auto,
    Help,
    Quit,
    Install,
    Uninstall,
    AppFiles,
    Leftovers,
    RefreshApps,
    PreviousPage,
    NextPage,
    FirstPage,
    LastPage,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    View(View),
    Query,
    Row(usize),
    Tile(u32),
    Check(usize),
    Sort(Sort),
    Command(Command),
    Crumb(u32),
    List,
    Inspect,
    Activity,
    Scrollbar,
    ModalBody,
    ModalInput,
    ModalCancel,
    ModalConfirm,
    Menu(usize),
    AppFile(usize),
    AppProvider,
}

#[derive(Default)]
struct Ui {
    hits: Vec<(Rect, Hit)>,
    list: Rect,
    inspector: Rect,
    page: usize,
    modal_max: u16,
    inspect_max: u16,
    activity_max: u16,
    modal_input: Rect,
    query: Rect,
    commands: Vec<Command>,
}
impl Ui {
    fn hit(&self, column: u16, row: u16) -> Option<Hit> {
        self.hits
            .iter()
            .rev()
            .find(|(r, _)| column >= r.x && column < r.right() && row >= r.y && row < r.bottom())
            .map(|(_, h)| *h)
    }
}

enum Modal {
    AppFiles(applications::Files),
    AppInstall(applications::Install),
    Managed(Box<crate::apps::ManagedPlan>),
    Help,
    Inspect,
    Preview(Box<Plan>),
    Destination(Editor),
    Auto,
    Notice(String),
    Menu(usize),
}
#[derive(Clone)]
struct Location {
    view: View,
    folder: PathBuf,
    focused: Option<PathBuf>,
    query: Editor,
    sort: Sort,
    offset: usize,
    page_cursor: Option<String>,
    stamp: Option<[u64; 5]>,
}
enum Message {
    Apps(u64, applications::Loaded),
    AppFiles(u64, applications::Files),
    Scan(Result<PathBuf, String>),
    Search(u64, Result<query::SearchResult, String>),
    Listing(
        u64,
        std::result::Result<Option<rows::Data>, String>,
        Option<u32>,
        usize,
    ),
    Plan(u64, Result<Plan, String>),
    Applied(Result<OperationUpdate, String>),
    Auto(Result<Option<OperationUpdate>, String>),
}

struct OperationUpdate {
    mutated: bool,
    summary: String,
    hidden: Vec<u32>,
    restored: Option<u32>,
    reindex: bool,
    uncertain: bool,
    blocked: Vec<ops::Blocked>,
}

pub struct App {
    pages: pages::State,
    apps: applications::State,
    pending_native: Option<crate::apps::ManagedPlan>,
    state: PathBuf,
    root: PathBuf,
    index: Option<Arc<Index>>,
    view: View,
    folder: u32,
    ids: Vec<u32>,
    table: TableState,
    selected: HashSet<u32>,
    anchor: Option<usize>,
    hidden: HashSet<u32>,
    needs_rescan: bool,
    folder_count: u64,
    folder_bytes: u64,
    show_map: bool,
    rows_cache: Arc<Mutex<rows::Cache>>,
    rows_revision: u64,
    listing: bool,
    list_inflight: bool,
    pending_listing: Option<(Option<u32>, usize)>,
    query: Editor,
    focus: Focus,
    action_index: usize,
    sort: Sort,
    status: String,
    modal: Option<Modal>,
    modal_focus: u8,
    modal_scroll: u16,
    inspect_scroll: u16,
    activity_scroll: u16,
    ui: Ui,
    hover: Option<Hit>,
    pressed: Option<Hit>,
    drag_scrollbar: bool,
    last_click: Option<(u32, Instant)>,
    drag_anchor: Option<usize>,
    back: Vec<Location>,
    forward: Vec<Location>,
    restore_focus: Option<u32>,
    progress: Arc<Mutex<Progress>>,
    cancel: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    tx: Sender<Message>,
    rx: Receiver<Message>,
    scan: Option<thread::JoinHandle<()>>,
    index_error: Option<String>,
    busy: bool,
    applying: bool,
    quit_after_operation: bool,
    plan_generation: u64,
    plan_inflight: bool,
    searching: bool,
    search_inflight: bool,
    search_pending: bool,
    search_error: Option<String>,
    matched: u64,
    search_ms: f64,
    activity: Vec<String>,
    coverage: Vec<String>,
    auto_checked: Instant,
    auto_busy: bool,
}

impl App {
    fn record(&mut self, text: String) {
        self.activity.push(text);
        if self.activity.len() > 256 {
            self.activity.drain(..self.activity.len() - 256);
        }
    }
    pub fn new(root: PathBuf, state: PathBuf) -> Self {
        Self::new_in(root, state, View::Space, None)
    }
    fn new_in(
        root: PathBuf,
        state: PathBuf,
        view: View,
        context: Option<crate::apps::Context>,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let stored = crate::index_path(&state);
        let opened = Index::open(&stored);
        let index_error = opened
            .as_ref()
            .err()
            .filter(|_| stored.symlink_metadata().is_ok())
            .map(|error| format!("Index unavailable · r Scan to repair · {error:#}"));
        let index = opened
            .ok()
            .filter(|i| i.path(0).is_ok_and(|p| p == root))
            .map(Arc::new);
        let mut app = Self {
            pages: Default::default(),
            apps: applications::State::new(context),
            pending_native: None,
            state,
            root,
            index,
            view,
            folder: 0,
            ids: Vec::new(),
            table: TableState::default(),
            selected: HashSet::new(),
            anchor: None,
            hidden: HashSet::new(),
            needs_rescan: false,
            folder_count: 0,
            folder_bytes: 0,
            show_map: true,
            rows_cache: Arc::new(Mutex::new(rows::Cache::default())),
            rows_revision: 0,
            listing: false,
            list_inflight: false,
            pending_listing: None,
            query: Editor::default(),
            focus: Focus::List,
            action_index: 0,
            sort: Sort::Size,
            status: "Ready · ? Help".into(),
            modal: None,
            modal_focus: 0,
            modal_scroll: 0,
            inspect_scroll: 0,
            activity_scroll: 0,
            ui: Ui::default(),
            hover: None,
            pressed: None,
            drag_scrollbar: false,
            last_click: None,
            drag_anchor: None,
            back: Vec::new(),
            forward: Vec::new(),
            restore_focus: None,
            progress: Arc::new(Mutex::new(Progress::default())),
            cancel: Arc::new(AtomicBool::new(false)),
            generation: Arc::new(AtomicU64::new(0)),
            tx,
            rx,
            scan: None,
            index_error,
            busy: false,
            applying: false,
            quit_after_operation: false,
            plan_generation: 0,
            plan_inflight: false,
            searching: false,
            search_inflight: false,
            search_pending: false,
            search_error: None,
            matched: 0,
            search_ms: 0.,
            activity: Vec::new(),
            coverage: Vec::new(),
            auto_checked: Instant::now(),
            auto_busy: false,
        };
        app.load_overlay();
        app.load_coverage();
        app.refresh(None, 0);
        if let Some(error) = &app.index_error {
            app.status = error.clone();
        }
        app
    }

    fn load_coverage(&mut self) {
        self.coverage = std::fs::read(self.state.join("latest-scan.json"))
            .ok()
            .and_then(|data| serde_json::from_slice::<serde_json::Value>(&data).ok())
            .and_then(|v| v["errors"].as_array().cloned())
            .unwrap_or_default()
            .iter()
            .take(1000)
            .filter_map(|e| e.as_str())
            .map(crate::clean)
            .collect();
    }

    fn focused(&self) -> Option<u32> {
        if self.view == View::Apps {
            return None;
        }
        self.table
            .selected()
            .and_then(|row| self.ids.get(row))
            .copied()
    }
    fn chosen(&self) -> Vec<u32> {
        if self.selected.is_empty() {
            self.focused().into_iter().collect()
        } else {
            let mut ids: Vec<_> = self.selected.iter().copied().collect();
            ids.sort_unstable();
            ids
        }
    }
    fn mutable(&self) -> bool {
        !self.busy
            && !self.plan_inflight
            && !self.auto_busy
            && !self.searching
            && !self.listing
            && self.scan.is_none()
            && !matches!(self.view, View::Activity | View::Apps)
            && !self.chosen().is_empty()
    }
    fn cancel_plan(&mut self) {
        self.plan_generation = self.plan_generation.wrapping_add(1);
        if self.busy && !self.applying {
            self.busy = false;
        }
    }
    fn invalidate_search(&mut self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.searching = false;
        self.search_pending = false;
        self.restore_focus = None;
        self.listing = false;
        self.pending_listing = None;
    }
    fn clear_selection(&mut self) {
        self.selected.clear();
        self.anchor = None;
        self.last_click = None;
        self.drag_anchor = None;
    }
    fn select_row(&mut self, row: usize) {
        self.table.select(if self.ids.is_empty() {
            None
        } else {
            Some(row.min(self.ids.len() - 1))
        });
        self.inspect_scroll = 0;
    }

    fn refresh(&mut self, focused: Option<u32>, offset: usize) {
        self.invalidate_search();
        self.clear_selection();
        self.table = TableState::default();
        if self.view == View::Apps {
            self.apps_filter(None);
            if self.apps.catalog.apps.is_empty() && !self.apps.inflight {
                self.apps_reload();
            }
            return;
        }
        if self.view == View::Find {
            self.restore_focus = focused;
            *self.table.offset_mut() = offset;
            self.search();
            return;
        }
        self.ids.clear();
        self.folder_count = 0;
        self.folder_bytes = 0;
        self.listing(focused, offset);
    }

    fn search(&mut self) {
        if self.view != View::Find {
            return;
        }
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.searching = self.index.is_some();
        self.search_error = None;
        self.status = "Searching…".into();
        self.search_pending = self.index.is_some();
        self.start_search();
    }

    fn start_search(&mut self) {
        if self.search_inflight || !self.search_pending || self.view != View::Find {
            return;
        }
        let Some(index) = self.index.clone() else {
            return;
        };
        self.search_pending = false;
        self.search_inflight = true;
        let locate = self.restore_focus;
        if let Some(data) = self.cached_page(&index, locate) {
            self.accept_search(data);
            return;
        }
        let n = self.generation.load(Ordering::Relaxed);
        let (input, sort, cursor, hidden, generation, tx) = (
            self.query.text.clone(),
            self.sort,
            self.pages.cursor.clone(),
            self.hidden.clone(),
            self.generation.clone(),
            self.tx.clone(),
        );
        self.searching = true;
        self.search_error = None;
        self.status = "Searching…".into();
        thread::spawn(move || {
            let result = query::Query::parse(&input)
                .and_then(|q| {
                    let mut page = query::search_page(
                        &index,
                        &q,
                        sort,
                        crate::page::Request {
                            limit: 1000,
                            cursor: cursor.as_deref(),
                        },
                        &hidden,
                        Some((&generation, n)),
                    )?;
                    if !page.canceled
                        && cursor.is_none()
                        && let Some(id) = locate
                        && !page.ids.contains(&id)
                    {
                        let at = q.cursor_at(&index, sort, &hidden, id);
                        if let Ok(found) = query::search_page(
                            &index,
                            &q,
                            sort,
                            crate::page::Request {
                                limit: 1000,
                                cursor: Some(&at),
                            },
                            &hidden,
                            Some((&generation, n)),
                        ) {
                            page = found;
                        }
                    }
                    Ok(page)
                })
                .map_err(|e| e.to_string());
            let _ = tx.send(Message::Search(n, result));
        });
    }

    fn query_changed(&mut self) {
        self.pages = Default::default();
        self.clear_selection();
        self.ids.clear();
        self.table = TableState::default();
        self.restore_focus = None;
        self.matched = 0;
        self.search();
    }

    fn scan_if_missing(&mut self) {
        if self.index.is_none() && self.scan.is_none() {
            if let Some(error) = &self.index_error {
                self.status = error.clone();
            } else {
                self.scan();
            }
        }
    }

    fn scan(&mut self) {
        if self.scan.is_some() {
            self.status = "A scan is already running".into();
            return;
        }
        if self.applying || self.auto_busy {
            self.status = "Finish the file operation before rescanning".into();
            return;
        }
        self.cancel_plan();
        self.cancel.store(false, Ordering::Relaxed);
        if let Ok(mut p) = self.progress.lock() {
            *p = Progress::default();
        }
        let (root, state, progress, cancel, tx) = (
            self.root.clone(),
            self.state.clone(),
            self.progress.clone(),
            self.cancel.clone(),
            self.tx.clone(),
        );
        self.status = "Scanning in the background · browsing stays available".into();
        self.scan = Some(thread::spawn(move || {
            let result =
                crate::index::scan(&root, &state, progress, cancel).map_err(|e| format!("{e:#}"));
            let _ = tx.send(Message::Scan(result));
        }));
    }

    fn preview(&mut self, action: Action) {
        if !self.mutable() {
            self.status = if self.searching || self.scan.is_some() {
                "Wait for the current search or scan before previewing file changes"
            } else if self.busy || self.auto_busy {
                "A file operation is already being checked"
            } else {
                "Choose an item first"
            }
            .into();
            return;
        }
        let Some(index) = self.index.clone() else {
            return;
        };
        let ids = self.chosen();
        self.cancel_plan();
        let n = self.plan_generation;
        self.busy = true;
        self.plan_inflight = true;
        self.status =
            "Checking selected paths, active files and protected data… · Esc cancels".into();
        let (state, tx) = (self.state.clone(), self.tx.clone());
        thread::spawn(move || {
            let result =
                ops::plan(&index, &ids, action, &state, 3600).map_err(|e| format!("{e:#}"));
            let _ = tx.send(Message::Plan(n, result));
        });
    }

    fn open_modal(&mut self, modal: Modal) {
        self.modal = Some(modal);
        self.modal_scroll = 0;
        self.modal_focus = 0;
        self.ui.hits.clear();
        self.last_click = None;
        self.pressed = None;
        self.drag_anchor = None;
        self.drag_scrollbar = false;
    }

    fn messages(&mut self) -> bool {
        let mut changed = false;
        while let Ok(message) = self.rx.try_recv() {
            changed = true;
            match message {
                Message::Apps(ticket, data) => self.receive_apps(ticket, data),
                Message::AppFiles(ticket, review) => {
                    self.plan_inflight = false;
                    if ticket == self.plan_generation {
                        self.busy = false;
                        self.open_modal(Modal::AppFiles(review));
                        self.modal_focus = 2;
                        self.status = "Choose files · Enter reviews · Esc cancels".into();
                    }
                }
                Message::Listing(ticket, data, focused, offset) => {
                    self.list_inflight = false;
                    if ticket == self.generation.load(Ordering::Relaxed)
                        && matches!(self.view, View::Space | View::Clean)
                    {
                        self.receive_listing(data, focused, offset);
                    }
                    self.start_listing();
                }
                Message::Scan(result) => {
                    if let Some(handle) = self.scan.take() {
                        let _ = handle.join();
                    }
                    match result
                        .and_then(|p| Index::open(&p).map(Arc::new).map_err(|e| e.to_string()))
                    {
                        Ok(index) => {
                            let location = self.location();
                            self.index = Some(index);
                            self.index_error = None;
                            self.hidden.clear();
                            self.invalidate_rows();
                            self.needs_rescan = false;
                            self.save_overlay();
                            self.cancel_plan();
                            if matches!(
                                self.modal,
                                Some(Modal::Preview(..) | Modal::Destination(_) | Modal::Menu(_))
                            ) {
                                self.modal = None;
                            }
                            self.load_coverage();
                            let focus = self.focus;
                            self.restore_location(location);
                            self.focus = focus;
                            let index = self.index.as_ref().unwrap();
                            self.status = format!(
                                "Indexed {} entries · {} restricted/missing",
                                index.live_count(),
                                index.errors
                            );
                            if let Ok(progress) = self.progress.lock()
                                && progress.message.starts_with("Scan complete · ")
                            {
                                self.status.push_str(" · ");
                                self.status.push_str(&progress.message);
                            }
                            self.record(self.status.clone());
                        }
                        Err(error) => {
                            self.status = error;
                            self.record(self.status.clone());
                        }
                    }
                }
                Message::Search(n, result) => {
                    self.search_inflight = false;
                    if n != self.generation.load(Ordering::Relaxed) || self.view != View::Find {
                        self.start_search();
                        continue;
                    }
                    self.searching = false;
                    match result {
                        Ok(r) if !r.canceled => {
                            if let Some(index) = &self.index {
                                self.cache_page(index, self.restore_focus, r.clone());
                            }
                            self.accept_search(r);
                        }
                        Err(error) => {
                            self.page_failed(error);
                        }
                        _ => {}
                    }
                    self.start_search();
                }
                Message::Plan(n, result) => {
                    self.plan_inflight = false;
                    if n != self.plan_generation {
                        continue;
                    }
                    self.busy = false;
                    self.status = "Review the plan · no files changed".into();
                    match result {
                        Ok(plan) => self.open_modal(Modal::Preview(Box::new(plan))),
                        Err(error) => self.open_modal(Modal::Notice(error)),
                    }
                }
                Message::Applied(result) => {
                    self.busy = false;
                    self.applying = false;
                    match result {
                        Ok(update) => {
                            self.status = update.summary.clone();
                            self.record(update.summary.clone());
                            if update.mutated
                                || update.uncertain
                                || update.reindex
                                || !update.hidden.is_empty()
                                || update.restored.is_some()
                            {
                                self.needs_rescan = true;
                            }
                            self.hidden.extend(update.hidden);
                            self.invalidate_rows();
                            self.clear_selection();
                            if let Some(id) = update.restored {
                                self.hidden.remove(&id);
                            }
                            self.save_overlay();
                            let focused = self.surviving_focus();
                            let offset = self.table.offset();
                            self.refresh(focused, offset);
                            if self.view == View::Apps {
                                self.apps_reload();
                            }
                            if update.reindex && self.view != View::Apps {
                                self.scan();
                                self.status = format!("{} · refreshing index", update.summary);
                            }
                            if !update.blocked.is_empty() {
                                let details = update
                                    .blocked
                                    .iter()
                                    .map(|b| format!("{}\n{}", b.path, b.reason))
                                    .collect::<Vec<_>>()
                                    .join("\n\n");
                                self.open_modal(Modal::Notice(format!(
                                    "{}\n\n{details}",
                                    update.summary
                                )));
                            }
                        }
                        Err(error) => {
                            self.status = error.clone();
                            // A filesystem change can succeed before journal persistence fails.
                            self.needs_rescan = true;
                            self.invalidate_rows();
                            self.save_overlay();
                            self.open_modal(Modal::Notice(format!(
                                "{error}\n\nSome paths may have changed. Press r to refresh the index."
                            )));
                        }
                    }
                }
                Message::Auto(result) => {
                    self.auto_busy = false;
                    match result {
                        Ok(Some(update)) => {
                            self.status = update.summary.clone();
                            self.record(update.summary);
                            if update.mutated || !update.hidden.is_empty() || update.uncertain {
                                self.hidden.extend(update.hidden);
                                self.invalidate_rows();
                                self.needs_rescan = true;
                                self.save_overlay();
                                let focused = self.surviving_focus();
                                let offset = self.table.offset();
                                self.refresh(focused, offset);
                            }
                        }
                        Ok(None) => {}
                        Err(error) => {
                            self.status = format!("Automatic cleanup stopped: {error}");
                            self.invalidate_rows();
                            self.needs_rescan = true;
                            self.save_overlay();
                            self.record(self.status.clone());
                        }
                    }
                }
            }
        }
        changed
    }

    fn apply(&mut self, plan: Plan, confirmation: String) {
        let (state, tx) = (self.state.clone(), self.tx.clone());
        self.busy = true;
        self.applying = true;
        self.modal = None;
        self.ui.hits.clear();
        self.status = "Applying reviewed plan…".into();
        thread::spawn(move || {
            let result = ops::apply(&state, &plan, &confirmation)
                .map(|r| {
                    let message = format!(
                        "{} completed · {} blocked · {}",
                        r.completed.len(),
                        r.blocked.len(),
                        r.note
                    );
                    let hidden = plan
                        .entries
                        .iter()
                        .filter(|entry| entry.id != u32::MAX && r.completed.contains(&entry.path))
                        .map(|entry| entry.id)
                        .collect();
                    OperationUpdate {
                        mutated: !r.completed.is_empty(),
                        summary: message,
                        hidden,
                        restored: None,
                        reindex: false,
                        uncertain: r.needs_reconcile,
                        blocked: r.blocked,
                    }
                })
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Message::Applied(result));
        });
    }

    fn auto_tick(&mut self) {
        if self.auto_checked.elapsed() < Duration::from_secs(60)
            || self.busy
            || self.plan_inflight
            || self.auto_busy
            || self.scan.is_some()
            || self.modal.is_some()
            || self.searching
            || self.listing
        {
            return;
        }
        self.auto_checked = Instant::now();
        let Some(index) = self.index.clone() else {
            return;
        };
        let (state, tx) = (self.state.clone(), self.tx.clone());
        if !ops::auto_policy(&state).is_ok_and(|p| p.enabled) {
            return;
        }
        self.auto_busy = true;
        thread::spawn(move || {
            let result = ops::auto_run(&state, &index)
                .map(|r| {
                    r.map(|r| {
                        let mut message = format!(
                            "Automatic cleanup: {} · {} removed",
                            r.completed.len(),
                            bytes(r.allocated_removed)
                        );
                        if r.needs_reconcile {
                            message.push_str(&format!(" · {}", r.note));
                        }
                        for blocked in &r.blocked {
                            message.push_str(&format!(" · {}: {}", blocked.path, blocked.reason));
                        }
                        let hidden = r
                            .completed
                            .iter()
                            .filter_map(|p| index.find_path(std::path::Path::new(p)))
                            .collect();
                        OperationUpdate {
                            mutated: !r.completed.is_empty(),
                            summary: message,
                            hidden,
                            restored: None,
                            reindex: false,
                            uncertain: r.needs_reconcile,
                            blocked: r.blocked,
                        }
                    })
                })
                .map_err(|e| e.to_string());
            let _ = tx.send(Message::Auto(result));
        });
    }
}

pub fn run(root: PathBuf, state: PathBuf) -> Result<()> {
    run_in(root, state, View::Space, None)
}

pub fn run_apps(root: PathBuf, state: PathBuf) -> Result<()> {
    run_in(root, state, View::Apps, None)
}

/// Starts the Apps view with explicit inventory roots and native tools.
pub fn run_apps_with_context(
    root: PathBuf,
    state: PathBuf,
    context: crate::apps::Context,
) -> Result<()> {
    run_in(root, state, View::Apps, Some(context))
}

fn run_in(
    root: PathBuf,
    state: PathBuf,
    view: View,
    context: Option<crate::apps::Context>,
) -> Result<()> {
    let mut app = App::new_in(root, state, view, context);
    let mut terminal = ratatui::init();
    struct Restore(Vec<signal_hook::SigId>);
    impl Drop for Restore {
        fn drop(&mut self) {
            for id in self.0.drain(..) {
                signal_hook::low_level::unregister(id);
            }
            let _ = execute!(io::stdout(), DisableMouseCapture, DisableBracketedPaste);
            ratatui::restore();
        }
    }
    let mut restore = Restore(Vec::new());
    let terminate = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        restore
            .0
            .push(signal_hook::flag::register(signal, terminate.clone())?);
    }
    execute!(io::stdout(), EnableMouseCapture, EnableBracketedPaste)?;
    if app.index.is_none() && view != View::Apps {
        app.scan_if_missing();
    }
    let result = (|| -> Result<()> {
        let mut dirty = true;
        let mut last_draw = Instant::now();
        loop {
            dirty |= app.messages();
            if let Some(plan) = app.pending_native.take() {
                execute!(io::stdout(), DisableMouseCapture, DisableBracketedPaste)?;
                ratatui::restore();
                println!("\n{}\n", plan.command);
                let result = plan.run(&app.state, plan.phrase());
                let message = match result {
                    Ok(()) => format!(
                        "{} completed for {}",
                        if plan.uninstall {
                            "Uninstall"
                        } else {
                            "Install"
                        },
                        plan.target
                    ),
                    Err(error) => format!("{error:#}"),
                };
                println!("\n{message}");
                if !terminate.load(Ordering::Relaxed) {
                    println!("\nPress Enter to return to Axit.");
                    while !terminate.load(Ordering::Relaxed) {
                        if event::poll(Duration::from_millis(50))?
                            && matches!(event::read()?,Event::Key(key) if key.code==crossterm::event::KeyCode::Enter)
                        {
                            break;
                        }
                    }
                }
                crossterm::terminal::enable_raw_mode()?;
                execute!(
                    io::stdout(),
                    crossterm::terminal::EnterAlternateScreen,
                    EnableMouseCapture,
                    EnableBracketedPaste
                )?;
                // Fullscreen resize clears both buffers without querying cursor position.
                // Some terminals and PTYs do not answer cursor-position reports.
                let (width, height) = crossterm::terminal::size()?;
                terminal.resize(Rect::new(0, 0, width, height))?;
                app.busy = false;
                app.applying = false;
                app.status = message.clone();
                app.record(message);
                app.needs_rescan = true;
                app.invalidate_rows();
                app.save_overlay();
                app.apps_reload();
                dirty = true;
            }
            app.auto_tick();
            if terminate.load(Ordering::Relaxed) && app.quit() {
                break;
            }
            if app.quit_after_operation && !app.applying && !app.auto_busy {
                break;
            }
            let refresh = if app.scan.is_some()
                || app.busy
                || app.searching
                || app.listing
                || app.apps.loading
            {
                Duration::from_millis(200)
            } else {
                Duration::from_secs(2)
            };
            if dirty || last_draw.elapsed() >= refresh {
                terminal.draw(|f| draw(f, &mut app))?;
                last_draw = Instant::now();
                dirty = false;
            }
            if event::poll(Duration::from_millis(50))? {
                // Drain a bounded input burst before drawing, so touchpad inertia stays responsive.
                for _ in 0..32 {
                    let e = event::read()?;
                    dirty |= !matches!(
                        e,
                        Event::Mouse(crossterm::event::MouseEvent {
                            kind: crossterm::event::MouseEventKind::Moved,
                            ..
                        })
                    );
                    let hover = app.hover;
                    if app.handle_event(e) {
                        return Ok(());
                    }
                    dirty |= hover != app.hover;
                    if !event::poll(Duration::ZERO)? {
                        break;
                    }
                }
            }
        }
        Ok(())
    })();
    app.cancel.store(true, Ordering::Relaxed);
    app.invalidate_search();
    drop(restore);
    if let Some(handle) = app.scan.take() {
        let _ = handle.join();
    }
    result
}
