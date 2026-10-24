mod editor;
mod navigation;
mod render;
mod state;
#[cfg(test)]
mod tests;

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
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
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
    Select,
    Trash,
    Move,
    Clean,
    Undo,
    Rescan,
    Auto,
    Help,
    Quit,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    View(View),
    Query,
    Row(usize),
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
    Help,
    Inspect,
    Preview(Box<Plan>, Editor),
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
}
enum Message {
    Scan(Result<PathBuf, String>),
    Search(u64, Result<query::SearchResult, String>),
    Plan(u64, Result<Plan, String>),
    Applied(Result<OperationUpdate, String>),
    Auto(Result<Option<(String, Vec<u32>)>, String>),
}

struct OperationUpdate {
    summary: String,
    hidden: Vec<u32>,
    restored: Option<u32>,
    blocked: Vec<ops::Blocked>,
}

pub struct App {
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
    busy: bool,
    applying: bool,
    quit_after_operation: bool,
    plan_generation: u64,
    searching: bool,
    search_error: Option<String>,
    matched: u64,
    search_ms: f64,
    activity: Vec<String>,
    coverage: Vec<String>,
    auto_checked: Instant,
    auto_busy: bool,
}

impl App {
    pub fn new(root: PathBuf, state: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel();
        let index = Index::open(&crate::index_path(&state))
            .ok()
            .filter(|i| i.path(0).is_ok_and(|p| p == root))
            .map(Arc::new);
        let mut app = Self {
            state,
            root,
            index,
            view: View::Space,
            folder: 0,
            ids: Vec::new(),
            table: TableState::default(),
            selected: HashSet::new(),
            anchor: None,
            hidden: HashSet::new(),
            needs_rescan: false,
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
            busy: false,
            applying: false,
            quit_after_operation: false,
            plan_generation: 0,
            searching: false,
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
        self.table
            .selected()
            .and_then(|row| self.ids.get(row))
            .copied()
    }
    fn chosen(&self) -> Vec<u32> {
        if self.selected.is_empty() {
            self.focused().into_iter().collect()
        } else {
            self.ids
                .iter()
                .copied()
                .filter(|id| self.selected.contains(id))
                .collect()
        }
    }
    fn mutable(&self) -> bool {
        !self.busy
            && !self.auto_busy
            && !self.searching
            && self.scan.is_none()
            && self.view != View::Activity
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
        self.restore_focus = None;
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
        if self.view == View::Find {
            self.restore_focus = focused;
            *self.table.offset_mut() = offset;
            self.search();
            return;
        }
        self.ids = if let Some(index) = &self.index {
            match self.view {
                View::Space => index
                    .children(self.folder, 1000)
                    .into_iter()
                    .filter(|e| !index.is_hidden(e.id, &self.hidden))
                    .map(|e| e.id)
                    .collect(),
                View::Clean => (1..index.count)
                    .filter_map(|id| index.entry(id))
                    .filter(|e| {
                        e.is_dir()
                            && e.allocated > 0
                            && !index.is_hidden(e.id, &self.hidden)
                            && ops::cache_candidate(index, *e)
                    })
                    .map(|e| e.id)
                    .collect(),
                _ => Vec::new(),
            }
        } else {
            Vec::new()
        };
        self.sort_rows();
        if self.view == View::Clean {
            self.ids.truncate(1000);
        }
        let row = focused
            .and_then(|id| self.ids.iter().position(|value| *value == id))
            .unwrap_or(0);
        self.select_row(row);
        *self.table.offset_mut() = offset.min(self.ids.len().saturating_sub(1));
    }

    fn sort_rows(&mut self) {
        if let Some(index) = &self.index {
            let sort = self.sort;
            self.ids.sort_unstable_by(|a, b| {
                let (a, b) = (index.entry(*a).unwrap(), index.entry(*b).unwrap());
                let names = || index.name(a).cmp(index.name(b)).then(a.id.cmp(&b.id));
                match sort {
                    Sort::Name => names(),
                    Sort::Size => b.allocated.cmp(&a.allocated).then_with(names),
                    Sort::Modified => b.modified.cmp(&a.modified).then_with(names),
                }
            });
        }
    }

    fn search(&mut self) {
        if self.view != View::Find {
            return;
        }
        let Some(index) = self.index.clone() else {
            return;
        };
        let n = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let (input, sort, hidden, generation, tx) = (
            self.query.text.clone(),
            self.sort,
            self.hidden.clone(),
            self.generation.clone(),
            self.tx.clone(),
        );
        self.searching = true;
        self.search_error = None;
        self.status = "Searching…".into();
        thread::spawn(move || {
            let result = query::Query::parse(&input)
                .map(|q| query::search(&index, &q, sort, 1000, &hidden, Some((&generation, n))))
                .map_err(|e| e.to_string());
            let _ = tx.send(Message::Search(n, result));
        });
    }

    fn query_changed(&mut self) {
        self.clear_selection();
        self.ids.clear();
        self.table = TableState::default();
        self.restore_focus = None;
        self.matched = 0;
        self.search();
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
                            self.hidden.clear();
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
                            self.restore_location(location);
                            let index = self.index.as_ref().unwrap();
                            self.status = format!(
                                "Indexed {} entries · {} restricted/missing",
                                index.count, index.errors
                            );
                            self.activity.push(self.status.clone());
                        }
                        Err(error) => {
                            self.status = error;
                            self.activity.push(self.status.clone());
                        }
                    }
                }
                Message::Search(n, result) => {
                    if n != self.generation.load(Ordering::Relaxed) || self.view != View::Find {
                        continue;
                    }
                    self.searching = false;
                    match result {
                        Ok(r) if !r.canceled => {
                            let focused = self.restore_focus.take().or_else(|| self.focused());
                            self.ids = r.ids;
                            self.matched = r.matched;
                            self.search_ms = r.elapsed_ms;
                            self.selected.retain(|id| self.ids.contains(id));
                            let row = focused
                                .and_then(|id| self.ids.iter().position(|value| *value == id))
                                .unwrap_or(0);
                            self.select_row(row);
                            self.status = if self.ids.is_empty() {
                                "No matches · edit the query or press Esc to browse".into()
                            } else {
                                "Ready".into()
                            };
                        }
                        Err(error) => {
                            self.ids.clear();
                            self.table.select(None);
                            self.matched = 0;
                            self.status = "Invalid query · edit the search field".into();
                            self.search_error = Some(error);
                        }
                        _ => {}
                    }
                }
                Message::Plan(n, result) => {
                    if n != self.plan_generation {
                        continue;
                    }
                    self.busy = false;
                    self.status = "Review the plan · no files changed".into();
                    match result {
                        Ok(plan) => {
                            self.open_modal(Modal::Preview(Box::new(plan), Editor::default()))
                        }
                        Err(error) => self.open_modal(Modal::Notice(error)),
                    }
                }
                Message::Applied(result) => {
                    self.busy = false;
                    self.applying = false;
                    match result {
                        Ok(update) => {
                            self.status = update.summary.clone();
                            self.activity.push(update.summary.clone());
                            if !update.hidden.is_empty() || update.restored.is_some() {
                                self.needs_rescan = true;
                            }
                            self.hidden.extend(update.hidden);
                            self.clear_selection();
                            if let Some(id) = update.restored {
                                self.hidden.remove(&id);
                            }
                            self.save_overlay();
                            let focused = self.focused();
                            let offset = self.table.offset();
                            self.refresh(focused, offset);
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
                            self.open_modal(Modal::Notice(error));
                        }
                    }
                }
                Message::Auto(result) => {
                    self.auto_busy = false;
                    match result {
                        Ok(Some((message, hidden))) => {
                            self.status = message.clone();
                            self.activity.push(message);
                            self.hidden.extend(hidden);
                            self.needs_rescan = true;
                            self.save_overlay();
                            let focused = self.focused();
                            let offset = self.table.offset();
                            self.refresh(focused, offset);
                        }
                        Ok(None) => {}
                        Err(error) => {
                            self.status = format!("Automatic check skipped: {error}");
                            self.activity.push(self.status.clone());
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
                        .filter(|entry| r.completed.contains(&entry.path))
                        .map(|entry| entry.id)
                        .collect();
                    OperationUpdate {
                        summary: message,
                        hidden,
                        restored: None,
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
            || self.auto_busy
            || self.scan.is_some()
            || self.modal.is_some()
            || self.searching
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
                        let message = format!(
                            "Automatic cleanup: {} · {} removed",
                            r.completed.len(),
                            bytes(r.allocated_removed)
                        );
                        let hidden = r
                            .completed
                            .iter()
                            .filter_map(|p| index.find_path(std::path::Path::new(p)))
                            .collect();
                        (message, hidden)
                    })
                })
                .map_err(|e| e.to_string());
            let _ = tx.send(Message::Auto(result));
        });
    }
}

pub fn run(root: PathBuf, state: PathBuf) -> Result<()> {
    let mut app = App::new(root, state);
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
    if app.index.is_none() {
        app.scan();
    }
    let result = (|| -> Result<()> {
        let mut dirty = true;
        let mut last_draw = Instant::now();
        loop {
            dirty |= app.messages();
            app.auto_tick();
            if terminate.load(Ordering::Relaxed) && app.quit() {
                break;
            }
            if app.quit_after_operation && !app.applying && !app.auto_busy {
                break;
            }
            let refresh = if app.scan.is_some() || app.busy || app.searching {
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
