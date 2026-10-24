use crate::{
    bytes,
    index::{Index, Progress},
    ops::{self, Action, Plan},
    query::{self, Sort},
};
use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Gauge, Paragraph, Row, Table, TableState, Wrap},
};
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

const ACCENT: Color = Color::Rgb(180, 158, 255);
const MINT: Color = Color::Rgb(125, 223, 190);
const DIM: Color = Color::Rgb(137, 145, 166);
const FG: Color = Color::Rgb(224, 228, 240);
const BG: Color = Color::Rgb(17, 20, 29);
const PANEL: Color = Color::Rgb(24, 28, 39);
const AMBER: Color = Color::Rgb(240, 193, 121);

#[derive(Clone, Copy, PartialEq)]
enum View {
    Space,
    Find,
    Clean,
    Activity,
}
enum Modal {
    Help,
    Preview(Box<Plan>, String),
    Destination(String),
    Auto,
    Notice(String),
}
enum Message {
    Scan(Result<PathBuf, String>),
    Search(u64, Result<query::SearchResult, String>),
    Plan(Result<Plan, String>),
    Applied(Result<(String, Vec<u32>), String>),
    Auto(Result<Option<(String, Vec<u32>)>, String>),
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
    hidden: HashSet<u32>,
    query: String,
    editing: bool,
    sort: Sort,
    status: String,
    modal: Option<Modal>,
    progress: Arc<Mutex<Progress>>,
    cancel: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    tx: Sender<Message>,
    rx: Receiver<Message>,
    scan: Option<thread::JoinHandle<()>>,
    busy: bool,
    matched: u64,
    search_ms: f64,
    activity: Vec<String>,
    auto_checked: Instant,
    auto_busy: bool,
    preview_scroll: u16,
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
            hidden: HashSet::new(),
            query: String::new(),
            editing: false,
            sort: Sort::Name,
            status: "Ready".into(),
            modal: None,
            progress: Arc::new(Mutex::new(Progress::default())),
            cancel: Arc::new(AtomicBool::new(false)),
            generation: Arc::new(AtomicU64::new(0)),
            tx,
            rx,
            scan: None,
            busy: false,
            matched: 0,
            search_ms: 0.,
            activity: Vec::new(),
            auto_checked: Instant::now(),
            auto_busy: false,
            preview_scroll: 0,
        };
        if let Some(index) = &app.index
            && let Ok(root) = index.path(0)
        {
            app.root = root;
        }
        app.refresh();
        app
    }

    fn refresh(&mut self) {
        self.table.select(Some(0));
        let Some(index) = &self.index else {
            return;
        };
        self.ids = match self.view {
            View::Space => index
                .children(self.folder, 1000)
                .into_iter()
                .map(|e| e.id)
                .filter(|id| !index.is_hidden(*id, &self.hidden))
                .collect(),
            View::Clean => {
                let mut candidates = Vec::new();
                for id in 1..index.count {
                    let Some(e) = index.entry(id) else {
                        continue;
                    };
                    if !e.is_dir() || e.allocated == 0 || self.hidden.contains(&id) {
                        continue;
                    }
                    let name = index.name(e);
                    if ![
                        b"target".as_slice(),
                        b"incremental",
                        b".cxx",
                        b".gradle",
                        b"intermediates",
                        b"Intermediates.noindex",
                        b"Index.noindex",
                        b"ModuleCache.noindex",
                        b"SDKStatCaches.noindex",
                        b"SDKExplicitPrecompiledModules",
                    ]
                    .contains(&name)
                    {
                        continue;
                    }
                    if index.path(id).is_ok_and(|p| ops::cache_label(&p).is_some()) {
                        candidates.push(e);
                    }
                }
                candidates.sort_unstable_by_key(|e| std::cmp::Reverse(e.allocated));
                candidates.truncate(1000);
                candidates.into_iter().map(|e| e.id).collect()
            }
            View::Find => {
                self.search();
                return;
            }
            View::Activity => Vec::new(),
        };
    }

    fn scan(&mut self) {
        if self.scan.is_some() {
            self.status = "A scan is already running".into();
            return;
        }
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
        self.status = "Scanning in the background. Search stays available.".into();
        self.scan = Some(thread::spawn(move || {
            let result =
                crate::index::scan(&root, &state, progress, cancel).map_err(|e| format!("{e:#}"));
            let _ = tx.send(Message::Scan(result));
        }));
    }

    fn search(&mut self) {
        let Some(index) = self.index.clone() else {
            return;
        };
        let n = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let (input, sort, hidden, generation, tx) = (
            self.query.clone(),
            self.sort,
            self.hidden.clone(),
            self.generation.clone(),
            self.tx.clone(),
        );
        thread::spawn(move || {
            let result = query::Query::parse(&input)
                .map(|q| query::search(&index, &q, sort, 1000, &hidden, Some((&generation, n))))
                .map_err(|e| e.to_string());
            let _ = tx.send(Message::Search(n, result));
        });
    }

    fn chosen(&self) -> Vec<u32> {
        if self.selected.is_empty() {
            self.table
                .selected()
                .and_then(|i| self.ids.get(i))
                .copied()
                .into_iter()
                .collect()
        } else {
            self.selected.iter().copied().collect()
        }
    }

    fn preview(&mut self, action: Action) {
        if self.busy {
            return;
        }
        let Some(index) = self.index.clone() else {
            return;
        };
        let ids = self.chosen();
        if ids.is_empty() {
            return;
        }
        self.busy = true;
        self.status = "Checking identity, active files, protected data and cache contents…".into();
        let (state, tx) = (self.state.clone(), self.tx.clone());
        thread::spawn(move || {
            let result =
                ops::plan(&index, &ids, action, &state, 3600).map_err(|e| format!("{e:#}"));
            let _ = tx.send(Message::Plan(result));
        });
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
                            self.generation.fetch_add(1, Ordering::Relaxed);
                            self.status = format!(
                                "Indexed {} entries · {} restricted/missing locations",
                                index.count, index.errors
                            );
                            self.activity.push(self.status.clone());
                            self.index = Some(index);
                            self.folder = 0;
                            self.hidden.clear();
                            self.selected.clear();
                            self.refresh();
                        }
                        Err(error) => {
                            self.status = error;
                            self.activity.push(self.status.clone());
                        }
                    }
                }
                Message::Search(n, result) => {
                    if n != self.generation.load(Ordering::Relaxed) {
                        continue;
                    }
                    match result {
                        Ok(r) if !r.canceled => {
                            self.ids = r.ids;
                            self.matched = r.matched;
                            self.search_ms = r.elapsed_ms;
                            self.table.select(Some(0));
                            self.status = "Ready".into();
                        }
                        Err(error) => self.status = error,
                        _ => {}
                    }
                }
                Message::Plan(result) => {
                    self.busy = false;
                    self.status = "Preview ready · no files changed".into();
                    self.preview_scroll = 0;
                    match result {
                        Ok(p) => self.modal = Some(Modal::Preview(Box::new(p), String::new())),
                        Err(e) => self.modal = Some(Modal::Notice(e)),
                    }
                }
                Message::Applied(result) => {
                    self.busy = false;
                    self.auto_busy = false;
                    match result {
                        Ok((message, hidden)) => {
                            self.status = message.clone();
                            self.activity.push(message);
                            self.hidden.extend(hidden);
                            self.selected.clear();
                            self.generation.fetch_add(1, Ordering::Relaxed);
                            self.refresh();
                            self.scan();
                        }
                        Err(error) => {
                            self.status = error.clone();
                            self.modal = Some(Modal::Notice(error));
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
                            self.generation.fetch_add(1, Ordering::Relaxed);
                            self.refresh();
                            self.scan();
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

    fn auto_tick(&mut self) {
        if self.auto_checked.elapsed() < Duration::from_secs(60)
            || self.busy
            || self.auto_busy
            || self.scan.is_some()
        {
            return;
        }
        self.auto_checked = Instant::now();
        let Some(index) = self.index.clone() else {
            return;
        };
        let state = self.state.clone();
        let tx = self.tx.clone();
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

    fn apply(&mut self, plan: Plan, confirmation: String) {
        let (state, tx) = (self.state.clone(), self.tx.clone());
        self.busy = true;
        self.modal = None;
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
                    (message, hidden)
                })
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Message::Applied(result));
        });
    }

    fn key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c' | 'q'))
        {
            return true;
        }
        if let Some(modal) = self.modal.take() {
            match modal {
                Modal::Preview(plan, mut input) => {
                    let phrase = if plan.action == Action::Clean {
                        "CLEAN"
                    } else if plan.action == Action::Trash {
                        "TRASH"
                    } else {
                        "MOVE"
                    };
                    match key.code {
                        KeyCode::Esc => {
                            self.status = "Preview canceled · no files changed".into();
                        }
                        KeyCode::Up => {
                            self.preview_scroll = self.preview_scroll.saturating_sub(1);
                            self.modal = Some(Modal::Preview(plan, input));
                        }
                        KeyCode::Down => {
                            self.preview_scroll = self.preview_scroll.saturating_add(1);
                            self.modal = Some(Modal::Preview(plan, input));
                        }
                        KeyCode::PageUp => {
                            self.preview_scroll = self.preview_scroll.saturating_sub(10);
                            self.modal = Some(Modal::Preview(plan, input));
                        }
                        KeyCode::PageDown => {
                            self.preview_scroll = self.preview_scroll.saturating_add(10);
                            self.modal = Some(Modal::Preview(plan, input));
                        }
                        KeyCode::Backspace => {
                            input.pop();
                            self.modal = Some(Modal::Preview(plan, input));
                        }
                        KeyCode::Char(c) if input.len() < 12 => {
                            input.push(c);
                            self.modal = Some(Modal::Preview(plan, input));
                        }
                        KeyCode::Enter if input == phrase => self.apply(*plan, input),
                        _ => self.modal = Some(Modal::Preview(plan, input)),
                    }
                }
                Modal::Destination(mut input) => match key.code {
                    KeyCode::Esc => {}
                    KeyCode::Backspace => {
                        input.pop();
                        self.modal = Some(Modal::Destination(input));
                    }
                    KeyCode::Char(c) if input.len() < 4096 => {
                        input.push(c);
                        self.modal = Some(Modal::Destination(input));
                    }
                    KeyCode::Enter => {
                        let p = if input.starts_with("~/") {
                            crate::home()
                                .map(|p| p.join(&input[2..]))
                                .unwrap_or_default()
                        } else {
                            PathBuf::from(&input)
                        };
                        self.preview(Action::Move {
                            destination: p.to_string_lossy().into_owned(),
                        });
                    }
                    _ => self.modal = Some(Modal::Destination(input)),
                },
                Modal::Auto => match key.code {
                    KeyCode::Enter => {
                        let result = (|| -> Result<String> {
                            let mut policy = ops::auto_policy(&self.state)?;
                            policy.enabled = !policy.enabled;
                            if policy.enabled {
                                let index = self.index.as_ref().context("Scan a project first")?;
                                policy.roots =
                                    vec![index.path(self.folder)?.to_string_lossy().into_owned()];
                            }
                            ops::save_auto(&self.state, &policy)?;
                            Ok(if policy.enabled {
                                "Automatic cleanup enabled for this folder".into()
                            } else {
                                "Automatic cleanup disabled".into()
                            })
                        })();
                        self.modal = Some(Modal::Notice(result.unwrap_or_else(|e| e.to_string())));
                    }
                    KeyCode::Esc => {}
                    _ => self.modal = Some(Modal::Auto),
                },
                Modal::Help | Modal::Notice(_) => {}
            }
            return false;
        }
        if self.editing {
            match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Tab => self.editing = false,
                KeyCode::Backspace => {
                    self.query.pop();
                    self.search();
                }
                KeyCode::Char(c) if self.query.len() < 4096 => {
                    self.query.push(c);
                    self.search();
                }
                KeyCode::Down => {
                    self.editing = false;
                    self.table.select(Some(0));
                }
                _ => {}
            }
            return false;
        }
        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('1') => {
                self.view = View::Space;
                self.refresh();
            }
            KeyCode::Char('2' | '/') => {
                self.view = View::Find;
                self.editing = true;
                self.refresh();
            }
            KeyCode::Char('3') => {
                self.view = View::Clean;
                self.refresh();
            }
            KeyCode::Char('4') => {
                self.view = View::Activity;
                self.refresh();
            }
            KeyCode::Char('?') => self.modal = Some(Modal::Help),
            KeyCode::Char('r') => self.scan(),
            KeyCode::Char('a') => self.modal = Some(Modal::Auto),
            KeyCode::Char('x') => self.preview(Action::Clean),
            KeyCode::Char('t') => self.preview(Action::Trash),
            KeyCode::Char('m') => self.modal = Some(Modal::Destination(String::new())),
            KeyCode::Char('u') => {
                if !self.busy {
                    self.busy = true;
                    let (state, tx) = (self.state.clone(), self.tx.clone());
                    thread::spawn(move || {
                        let r = ops::undo(&state)
                            .map(|v| (v.to_string(), Vec::new()))
                            .map_err(|e| e.to_string());
                        let _ = tx.send(Message::Applied(r));
                    });
                }
            }
            KeyCode::Char('s') => {
                self.sort = match self.sort {
                    Sort::Name => Sort::Size,
                    Sort::Size => Sort::Modified,
                    Sort::Modified => Sort::Name,
                };
                if self.view == View::Find {
                    self.search();
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let i = self.table.selected().unwrap_or(0);
                self.table
                    .select(Some((i + 1).min(self.ids.len().saturating_sub(1))));
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.table
                    .select(Some(self.table.selected().unwrap_or(0).saturating_sub(1)));
            }
            KeyCode::PageDown => {
                self.table.select(Some(
                    (self.table.selected().unwrap_or(0) + 15).min(self.ids.len().saturating_sub(1)),
                ));
            }
            KeyCode::PageUp => {
                self.table
                    .select(Some(self.table.selected().unwrap_or(0).saturating_sub(15)));
            }
            KeyCode::Char(' ') => {
                if let Some(id) = self.table.selected().and_then(|i| self.ids.get(i))
                    && !self.selected.remove(id)
                {
                    self.selected.insert(*id);
                }
            }
            KeyCode::Esc => self.selected.clear(),
            KeyCode::Backspace | KeyCode::Left => {
                if let Some(index) = &self.index
                    && self.folder != 0
                {
                    self.folder = index.entry(self.folder).map(|e| e.parent).unwrap_or(0);
                    self.view = View::Space;
                    self.refresh();
                }
            }
            KeyCode::Enter | KeyCode::Right => {
                if let Some(index) = &self.index
                    && let Some(e) = self
                        .table
                        .selected()
                        .and_then(|i| self.ids.get(i))
                        .and_then(|id| index.entry(*id))
                    && e.is_dir()
                    && !e.is_link()
                {
                    self.folder = e.id;
                    self.view = View::Space;
                    self.refresh();
                }
            }
            KeyCode::Tab if self.view == View::Find => self.editing = true,
            _ => {}
        }
        false
    }
}

fn pane(title: impl Into<Line<'static>>) -> Block<'static> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(54, 61, 78)))
        .style(Style::default().bg(PANEL))
}
fn key(label: &str, description: &str) -> Span<'static> {
    Span::styled(format!(" {label} {description} "), Style::default().fg(DIM))
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::default().bg(BG).fg(FG)), area);
    if area.width < 48 || area.height < 14 {
        frame.render_widget(
            Paragraph::new("SCRY / WCKD.ai\nExpand to at least 48 × 14.  Ctrl-Q quits.")
                .style(Style::default().fg(ACCENT)),
            area,
        );
        return;
    }
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(2),
            Constraint::Length(1),
        ])
        .margin(1)
        .split(area);
    let mut brand = vec![
        Span::styled(
            " ▰ SCRY ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled("WCKD.ai", Style::default().fg(DIM)),
        Span::raw("   see what’s hiding."),
    ];
    if let Some(index) = &app.index {
        brand.push(Span::styled(
            format!(
                "    {} entries · {} index",
                index.count,
                bytes(index.disk_bytes())
            ),
            Style::default().fg(DIM),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(brand)), layout[0]);
    let tabs = [
        (View::Space, "1 Space"),
        (View::Find, "2 Find"),
        (View::Clean, "3 Clean"),
        (View::Activity, "4 Activity"),
    ]
    .map(|(v, label)| {
        Span::styled(
            format!(" {label} "),
            Style::default()
                .fg(if app.view == v { BG } else { DIM })
                .bg(if app.view == v { ACCENT } else { BG })
                .add_modifier(Modifier::BOLD),
        )
    });
    frame.render_widget(Paragraph::new(Line::from(tabs.to_vec())), layout[1]);
    if app.view == View::Find {
        let title = if app.editing {
            " Find · typing · Tab to results "
        } else {
            " Find · / to edit "
        };
        frame.render_widget(
            Paragraph::new(app.query.as_str()).block(
                pane(title).border_style(Style::default().fg(if app.editing {
                    ACCENT
                } else {
                    DIM
                })),
            ),
            layout[2],
        );
        if app.editing {
            let x = layout[2].x
                + 1
                + app
                    .query
                    .chars()
                    .count()
                    .min(layout[2].width.saturating_sub(3) as usize) as u16;
            frame.set_cursor_position((x, layout[2].y + 1));
        }
    } else {
        let root = app
            .index
            .as_ref()
            .and_then(|i| i.path(0).ok())
            .unwrap_or(app.root.clone());
        if let Ok(volume) = crate::system::volume(&root) {
            let ratio = (1. - volume.available as f64 / volume.total.max(1) as f64).clamp(0., 1.);
            frame.render_widget(
                Gauge::default()
                    .ratio(ratio)
                    .label(format!(
                        "{} free / {} shared volume",
                        bytes(volume.available),
                        bytes(volume.total)
                    ))
                    .gauge_style(
                        Style::default()
                            .fg(if ratio > 0.95 { AMBER } else { MINT })
                            .bg(PANEL),
                    )
                    .block(pane(" Disk ")),
                layout[2],
            );
        }
    }
    if app.view == View::Activity {
        let mut lines = vec![Line::styled(
            "Local operations only. No telemetry. No file contents indexed.",
            Style::default().fg(DIM),
        )];
        lines.extend(
            app.activity
                .iter()
                .rev()
                .take(40)
                .map(|s| Line::raw(s.clone())),
        );
        let report = app.state.join("latest-scan.json");
        if let Ok(data) = std::fs::read(&report)
            && let Ok(v) = serde_json::from_slice::<serde_json::Value>(&data)
            && let Some(errors) = v["errors"].as_array()
            && !errors.is_empty()
        {
            lines.push(Line::styled(
                "Restricted / missing locations",
                Style::default().fg(AMBER),
            ));
            lines.extend(
                errors
                    .iter()
                    .take(20)
                    .filter_map(|e| e.as_str())
                    .map(|s| Line::raw(crate::clean(s))),
            );
        }
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(pane(" Activity / coverage ")),
            layout[3],
        );
    } else if let Some(index) = &app.index {
        let columns = if area.width >= 110 {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(66), Constraint::Percentage(34)])
                .spacing(1)
                .split(layout[3])
        } else {
            Layout::default()
                .constraints([Constraint::Percentage(100)])
                .split(layout[3])
        };
        let title = match app.view {
            View::Space => format!(
                " {} ",
                index
                    .path(app.folder)
                    .map(|p| crate::text(&p))
                    .unwrap_or_default()
            ),
            View::Find => format!(
                " {} matches · {:.0} ms · {:?} ",
                app.matched, app.search_ms, app.sort
            ),
            View::Clean => " Build caches · verify before cleaning · nested totals overlap ".into(),
            View::Activity => String::new(),
        };
        let rows = app.ids.iter().filter_map(|id| index.entry(*id)).map(|e| {
            let mark = if app.selected.contains(&e.id) {
                "●"
            } else if e.is_dir() {
                "▸"
            } else if e.is_link() {
                "↗"
            } else {
                "·"
            };
            let kind = if e.flags & crate::index::RESTRICTED != 0 {
                "Restricted"
            } else if e.is_dir() {
                "Folder"
            } else if e.is_link() {
                "Link"
            } else {
                "File"
            };
            Row::new(vec![
                format!("{mark} {}", index.display_name(e)),
                bytes(e.allocated),
                kind.to_string(),
            ])
            .style(
                Style::default().fg(if e.flags & crate::index::RESTRICTED != 0 {
                    AMBER
                } else {
                    FG
                }),
            )
        });
        let table = Table::new(
            rows,
            [
                Constraint::Min(14),
                Constraint::Length(12),
                Constraint::Length(11),
            ],
        )
        .header(
            Row::new(["Name", "On disk", "Type"])
                .style(Style::default().fg(DIM))
                .bottom_margin(1),
        )
        .block(pane(title))
        .row_highlight_style(Style::default().bg(Color::Rgb(49, 43, 72)).fg(FG))
        .highlight_symbol("▌");
        frame.render_stateful_widget(table, columns[0], &mut app.table);
        if columns.len() > 1 {
            let mut lines = Vec::new();
            if let Some(e) = app
                .table
                .selected()
                .and_then(|i| app.ids.get(i))
                .and_then(|id| index.entry(*id))
            {
                let path = index.path(e.id).unwrap_or_default();
                lines.extend([
                    Line::styled(
                        index.display_name(e),
                        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                    ),
                    Line::raw(""),
                    Line::raw(crate::text(&path)),
                    Line::raw(""),
                    Line::raw(format!("On disk   {}", bytes(e.allocated))),
                    Line::raw(format!("File size {}", bytes(e.logical))),
                    Line::raw(format!(
                        "Modified  {} days ago",
                        (crate::unix_now() - e.modified).max(0) / 86400
                    )),
                    Line::raw(""),
                ]);
                if let Some(label) = ops::cache_label(&path) {
                    lines.push(Line::styled(label, Style::default().fg(MINT)));
                    lines.push(Line::raw("Candidate only. Active files, age, Git and protected contents are checked in the preview."));
                } else {
                    lines.push(Line::styled(
                        "Keep / review personally",
                        Style::default().fg(DIM),
                    ));
                    lines.push(Line::raw("Not eligible for automatic cleanup. Trash and moves require a reviewed plan."));
                }
                if e.flags & crate::index::ALIAS != 0 {
                    lines.push(Line::raw("Hardlink: shared inode counted elsewhere."));
                }
            }
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                format!("{} selected", app.selected.len()),
                Style::default().fg(DIM),
            ));
            lines.push(Line::raw("t Trash · m Move · x Verify cache"));
            lines.push(Line::raw("u Undo last move"));
            if index.errors > 0 {
                lines.push(Line::raw(""));
                lines.push(Line::styled(
                    format!("{} restricted/missing locations", index.errors),
                    Style::default().fg(AMBER),
                ));
                lines.push(Line::raw("Coverage is incomplete. See Activity. macOS may require Full Disk Access for your terminal."));
            }
            frame.render_widget(
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .block(pane(" Inspect ")),
                columns[1],
            );
        }
    } else {
        let p = app.progress.lock().map(|p| p.clone()).unwrap_or_default();
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled("Your disk, made legible.", Style::default().fg(ACCENT)),
                Line::raw(""),
                Line::raw(format!("Building the first index · {} entries", p.entries)),
                Line::raw(format!("{} restricted or missing locations", p.errors)),
                Line::raw(""),
                Line::raw("Only file metadata is read. File contents stay untouched."),
                Line::raw(
                    "The index is limited to 1 GiB. Symlinks and other mounts are not followed.",
                ),
            ])
            .block(pane(" First scan "))
            .wrap(Wrap { trim: false }),
            layout[3],
        );
    }
    let status = if app.scan.is_some() {
        let p = app.progress.lock().map(|p| p.clone()).unwrap_or_default();
        format!(
            "Scanning · {} entries · {} restricted/missing · {}",
            p.entries, p.errors, app.status
        )
    } else {
        app.status.clone()
    };
    frame.render_widget(
        Paragraph::new(crate::clean(&status))
            .style(Style::default().fg(if app.busy { ACCENT } else { DIM }))
            .wrap(Wrap { trim: true }),
        layout[4],
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            key("/", "find"),
            key("↵", "open folder"),
            key("Space", "select"),
            key("←", "up"),
            key("x", "clean"),
            key("a", "auto"),
            key("r", "rescan"),
            key("?", "help"),
            key("q", "quit"),
        ])),
        layout[5],
    );
    if let Some(modal) = &app.modal {
        draw_modal(frame, area, app, modal);
    }
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    Rect::new(
        area.x + (area.width - width.min(area.width)) / 2,
        area.y + (area.height - height.min(area.height)) / 2,
        width.min(area.width),
        height.min(area.height),
    )
}

fn draw_modal(frame: &mut Frame, area: Rect, app: &App, modal: &Modal) {
    let box_area = centered(area, 86, area.height.saturating_sub(4).min(28));
    frame.render_widget(Clear, box_area);
    if let Modal::Preview(plan, input) = modal {
        let block = pane(" Review plan ").border_style(Style::default().fg(ACCENT));
        let inside = block.inner(box_area);
        frame.render_widget(block, box_area);
        let parts = Layout::default()
            .constraints([
                Constraint::Length(3),
                Constraint::Min(1),
                Constraint::Length(5),
            ])
            .split(inside);
        let (action, phrase) = if plan.action == Action::Clean {
            ("Permanent cache cleanup", "CLEAN")
        } else if plan.action == Action::Trash {
            ("Move to Trash", "TRASH")
        } else {
            ("Move files", "MOVE")
        };
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(
                    format!(
                        "{action} · {} eligible · {} blocked",
                        plan.entries.len(),
                        plan.blocked.len()
                    ),
                    Style::default().fg(ACCENT),
                ),
                Line::raw(format!(
                    "{} allocated · expires after 10 minutes",
                    bytes(plan.total_bytes)
                )),
                Line::styled("↑/↓ scroll through every path", Style::default().fg(DIM)),
            ]),
            parts[0],
        );
        let mut lines = Vec::new();
        for e in &plan.entries {
            lines.push(Line::styled(
                format!("{} · {}", bytes(e.bytes), e.reason),
                Style::default().fg(MINT),
            ));
            lines.push(Line::raw(e.path.clone()));
            lines.push(Line::raw(""));
        }
        for e in &plan.blocked {
            lines.push(Line::styled(
                format!("BLOCKED · {}", e.reason),
                Style::default().fg(AMBER),
            ));
            lines.push(Line::raw(e.path.clone()));
            lines.push(Line::raw(""));
        }
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((app.preview_scroll, 0)),
            parts[1],
        );
        let consequence = if plan.action == Action::Clean {
            "Permanently removes verified caches. Future builds may take longer."
        } else if plan.action == Action::Trash {
            "Reversible. Disk space is released when Trash is emptied."
        } else {
            "Reversible. Existing destination files are never overwritten."
        };
        let prompt = if plan.entries.is_empty() {
            "No eligible entries. Esc closes this preview.".to_string()
        } else {
            format!("Type {phrase}, then Enter. Esc cancels.")
        };
        frame.render_widget(
            Paragraph::new(vec![
                Line::raw(""),
                Line::raw(consequence),
                Line::styled(prompt, Style::default().fg(ACCENT)),
                Line::raw(format!("> {input}")),
            ])
            .wrap(Wrap { trim: false }),
            parts[2],
        );
        return;
    }
    let (title,lines)=match modal {
        Modal::Help=>(" SCRY / keyboard ",vec!["1 Space   2 Find   3 Clean   4 Activity".into(),"/ Search · Tab switches between query and results".into(),"↑↓ or j/k Navigate · Enter opens a folder · Backspace goes up".into(),"Space Select · Esc clears selection".into(),"t Preview Trash · m Preview move · x Verify disposable cache".into(),"u Undo move/Trash · a Automatic cleanup settings · r Rescan".into(),"s Cycle search sort · q Quit · Ctrl-Q always quits".into(),"".into(),"Filters: ext:pdf size:>1gib older:30d type:dir path:niobium".into(),"Quoted words, *.zip wildcards, !excluded and re:expressions.".into(),"".into(),"Clean removes verified caches permanently. Trash does not free space until emptied.".into(),"Automatic cleanup is OFF unless enabled for an explicit folder.".into(),"File sizes may overlap through folders, hardlinks or APFS clones.".into(),"Any key closes help.".into()]),
        Modal::Notice(message)=>(" SCRY ",vec![message.clone(),"".into(),"Any key to close.".into()]),
        Modal::Destination(input)=>(" Move selected items ",vec!["Existing destination folder, inside the indexed scope:".into(),"".into(),input.clone(),"".into(),"Enter previews. Existing files are never overwritten. Esc cancels.".into()]),
        Modal::Auto=>{
            let policy=ops::auto_policy(&app.state).unwrap_or_default();
            let path=app.index.as_ref().and_then(|i|i.path(app.folder).ok()).unwrap_or(app.root.clone());
            (" Automatic space saving ",vec![format!("Currently {}",if policy.enabled{"ENABLED"}else{"OFF"}),"".into(),format!("Folder: {}",crate::text(&path)),"".into(),"Verified generated caches only · minimum age 7 days".into(),"At most 2 GiB per run · only below 20 GiB free".into(),"At most once a day · only while SCRY is running".into(),"Source, models, releases, keys and active builds are protected.".into(),"".into(),"Enabling authorizes permanent removal of eligible caches in this folder.".into(),"Entire home/disk roots are not allowed.".into(),"".into(),format!("Enter to {} · Esc cancels",if policy.enabled{"disable"}else{"enable for this folder"})])
        }
        Modal::Preview(plan,input)=>{
            let action=if plan.action==Action::Clean{"PERMANENT CACHE CLEANUP"}else if plan.action==Action::Trash{"MOVE TO TRASH"}else{"MOVE"};
            let phrase=if plan.action==Action::Clean{"CLEAN"}else if plan.action==Action::Trash{"TRASH"}else{"MOVE"};
            let mut lines=vec![format!("{action} · {} eligible · {} blocked",plan.entries.len(),plan.blocked.len()),format!("{} allocated · expires after 10 minutes",bytes(plan.total_bytes)),"".into()];
            for e in plan.entries.iter().take(6){lines.push(format!("{}  {}",bytes(e.bytes),e.path));}
            for e in plan.blocked.iter().take(5){lines.push(format!("BLOCKED {} · {}",e.path,e.reason));}
            lines.push("".into());
            if plan.action==Action::Clean{lines.push("Caches are deleted permanently. Future builds may take longer.".into());}
            else if plan.action==Action::Trash{lines.push("Trash is reversible and does not immediately free space.".into());}
            else{lines.push("Move is reversible. Existing destinations are never overwritten.".into());}
            lines.extend([format!("Type {phrase}, then Enter. Esc cancels."),format!("> {input}")]);
            (" Review plan ",lines)
        }
    };
    frame.render_widget(
        Paragraph::new(lines.into_iter().map(Line::raw).collect::<Vec<_>>())
            .style(Style::default().fg(FG))
            .wrap(Wrap { trim: false })
            .block(pane(title).border_style(Style::default().fg(ACCENT))),
        box_area,
    );
}

pub fn run(root: PathBuf, state: PathBuf) -> Result<()> {
    let mut app = App::new(root, state);
    let mut terminal = ratatui::init();
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            ratatui::restore();
        }
    }
    let _restore = Restore;
    if app.index.is_none() {
        app.scan();
    }
    let result = (|| -> Result<()> {
        let mut dirty = true;
        let mut last_draw = Instant::now();
        loop {
            dirty |= app.messages();
            app.auto_tick();
            let refresh = if app.scan.is_some() {
                Duration::from_millis(200)
            } else {
                Duration::from_secs(2)
            };
            if dirty || last_draw.elapsed() >= refresh {
                terminal.draw(|f| draw(f, &mut app))?;
                last_draw = Instant::now();
                dirty = false;
            }
            if event::poll(Duration::from_millis(150))? {
                dirty = true;
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        if app.key(key) {
                            break;
                        }
                    }
                    Event::Paste(s) => match &mut app.modal {
                        Some(Modal::Destination(input)) => {
                            input.extend(s.chars().filter(|c| !c.is_control()).take(4096))
                        }
                        _ if app.editing => {
                            app.query
                                .extend(s.chars().filter(|c| !c.is_control()).take(4096));
                            app.search();
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
        }
        Ok(())
    })();
    app.cancel.store(true, Ordering::Relaxed);
    app.generation.fetch_add(1, Ordering::Relaxed);
    if let Some(handle) = app.scan.take() {
        let _ = handle.join();
    }
    result
}
