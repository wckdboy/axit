use super::theme::*;
use super::*;
use crate::apps as manager;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::Line,
    widgets::{Paragraph, Row, Table, Wrap},
};

pub(super) fn query(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = render::pane(" Apps · / or Ctrl-F filters ", app.focus == Focus::Query);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let (text, cursor) = app.apps.filter.visible(inner.width);
    frame.render_widget(
        Paragraph::new(if text.is_empty() {
            "Name or application identifier"
        } else {
            &text
        })
        .style(Style::default().fg(if text.is_empty() { DIM } else { FG })),
        inner,
    );
    app.ui.query = inner;
    app.ui.hits.push((inner, Hit::Query));
    if app.focus == Focus::Query && app.modal.is_none() && inner.width > 0 {
        frame.set_cursor_position((inner.x + cursor, inner.y));
    }
}

pub(super) fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let columns = Layout::horizontal(if area.width >= 105 {
        vec![Constraint::Percentage(62), Constraint::Percentage(38)]
    } else {
        vec![Constraint::Percentage(100)]
    })
    .spacing(1)
    .split(area);
    let title = if app.apps.loading {
        " Apps · loading · navigation stays available ".into()
    } else if app.apps.show_leftovers {
        format!(
            " Data candidates · {} shown · review required ",
            app.ids.len()
        )
    } else {
        format!(" Installed apps · {} shown ", app.ids.len())
    };
    let block = render::pane(title, app.focus == Focus::List);
    let inner = block.inner(columns[0]);
    let header = usize::from(inner.height >= 3);
    let rows = app
        .ids
        .iter()
        .filter_map(|id| app.app_rows().get(*id as usize))
        .map(|a| {
            Row::new(vec!["▣".into(), a.name.clone(), a.source_label().into()])
                .style(Style::default().fg(if a.protected { DIM } else { FG }))
        });
    let mut table = Table::new(
        rows,
        [
            Constraint::Length(2),
            Constraint::Min(12),
            Constraint::Length(17),
        ],
    )
    .column_spacing(1)
    .block(block)
    .highlight_symbol("›")
    .row_highlight_style(Style::default().bg(SELECTED).fg(FG));
    if header > 0 {
        table = table.header(Row::new(["", "Name", "Source"]).style(Style::default().fg(DIM)));
    }
    frame.render_stateful_widget(table, columns[0], &mut app.table);
    let data = Rect::new(
        inner.x,
        inner.y + header as u16,
        inner.width,
        inner.height.saturating_sub(header as u16),
    );
    app.ui.list = data;
    app.ui.page = data.height.max(1) as usize;
    app.ui.hits.push((data, Hit::List));
    for offset in 0..data.height as usize {
        let row = app.table.offset() + offset;
        if row >= app.ids.len() {
            break;
        }
        app.ui.hits.push((
            Rect::new(data.x, data.y + offset as u16, data.width, 1),
            Hit::Row(row),
        ));
    }
    if app.ids.is_empty() {
        frame.render_widget(
            Paragraph::new(if app.apps.loading {
                "Reading app metadata…"
            } else {
                "No matching apps or data candidates."
            })
            .style(Style::default().fg(DIM))
            .wrap(Wrap { trim: false }),
            data,
        );
    }
    if columns.len() > 1 {
        let block = render::pane(" App details ", app.focus == Focus::Inspect);
        let inside = block.inner(columns[1]);
        frame.render_widget(block, columns[1]);
        let lines = if let Some(a) = app.app_target() {
            vec![
                Line::styled(
                    a.name.clone(),
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Line::raw(a.id.clone()),
                Line::raw(format!(
                    "{}{}",
                    a.source_label(),
                    a.version
                        .as_ref()
                        .map(|v| format!(" · {v}"))
                        .unwrap_or_default()
                )),
                Line::raw(""),
                Line::raw(a.path.as_deref().map(crate::text).unwrap_or_default()),
                Line::raw(""),
                Line::raw(
                    if matches!(a.source, manager::Source::Leftover) && !a.remembered {
                        "Unmatched data may belong to a service or shared app. Nothing is selected automatically."
                    } else {
                        "Personal data stays off by default. Files reviews exact and possible matches."
                    },
                ),
                Line::raw(""),
                Line::raw("i Install · Delete Remove · f Files · l Data"),
            ]
        } else {
            vec![Line::raw(
                "Choose an app to review its identity and related files.",
            )]
        };
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        app.ui.inspect_max = render::max_scroll(&paragraph, inside);
        app.inspect_scroll = app.inspect_scroll.min(app.ui.inspect_max);
        frame.render_widget(paragraph.scroll((app.inspect_scroll, 0)), inside);
        app.ui.inspector = inside;
        app.ui.hits.push((inside, Hit::Inspect));
    }
}

pub(super) fn modal(frame: &mut Frame, app: &mut App, area: Rect) -> bool {
    if let Some(Modal::AppFiles(review)) = &app.modal {
        let review = review.clone();
        let parts = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(2),
            Constraint::Length(3),
        ])
        .split(area);
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(
                    review.app.name.clone(),
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Line::raw("Space chooses · Enter reviews · Tab buttons"),
                Line::styled(
                    "Personal data and uncertain matches need your selection.",
                    Style::default().fg(DIM),
                ),
            ]),
            parts[0],
        );
        let mut lines = vec![];
        for (i, file) in review.rows.iter().enumerate() {
            let path = app
                .apps
                .context
                .as_ref()
                .and_then(|ctx| file.path.strip_prefix(&ctx.home).ok())
                .map(|p| format!("~/{}", crate::text(p)))
                .unwrap_or_else(|| crate::text(&file.path));
            let color = if !file.selectable {
                BORDER
            } else if app.modal_focus == 2 && i == review.cursor {
                ACCENT
            } else {
                FG
            };
            let checkbox = if !file.selectable {
                "[-]"
            } else if review.selected.contains(&i) {
                "[x]"
            } else {
                "[ ]"
            };
            lines.push(Line::styled(
                format!(
                    "{} {checkbox} {}",
                    if i == review.cursor { "›" } else { " " },
                    path
                ),
                Style::default().fg(color),
            ));
            lines.push(Line::styled(
                format!(
                    "      {}{}",
                    if file.contains_user_data {
                        "Personal data · "
                    } else {
                        ""
                    },
                    file.reason
                ),
                Style::default().fg(DIM),
            ));
        }
        if lines.is_empty() {
            lines.push(Line::raw("No related files found in the known locations."));
        }
        app.ui.modal_max = (lines.len() as u16).saturating_sub(parts[1].height);
        app.modal_scroll = app.modal_scroll.min(app.ui.modal_max);
        if app.modal_focus == 2 {
            let cursor = (review.cursor * 2) as u16;
            if cursor < app.modal_scroll {
                app.modal_scroll = cursor;
            }
            if cursor >= app.modal_scroll + parts[1].height {
                app.modal_scroll = cursor.saturating_sub(parts[1].height.saturating_sub(1));
            }
        }
        frame.render_widget(
            Paragraph::new(lines).scroll((app.modal_scroll, 0)),
            parts[1],
        );
        app.ui.hits.push((parts[1], Hit::ModalBody));
        for i in 0..review.rows.len() {
            let start = (i * 2) as i32 - i32::from(app.modal_scroll);
            let end = start + 2;
            if end > 0 && start < i32::from(parts[1].height) {
                app.ui.hits.push((
                    Rect::new(
                        parts[1].x,
                        parts[1].y + start.max(0) as u16,
                        parts[1].width,
                        (end.min(i32::from(parts[1].height)) - start.max(0)) as u16,
                    ),
                    Hit::AppFile(i),
                ));
            }
        }
        frame.render_widget(
            Paragraph::new(review.error.clone().unwrap_or_else(|| {
                format!(
                    "{} selected · files move to Trash after final review",
                    review.selected.len()
                )
            }))
            .style(Style::default().fg(if review.error.is_some() { DANGER } else { DIM }))
            .wrap(Wrap { trim: false }),
            parts[2],
        );
        render::modal_buttons(
            frame,
            app,
            parts[3],
            "Review selected",
            !review.selected.is_empty(),
        );
        return true;
    }
    if let Some(Modal::AppInstall(form)) = &mut app.modal {
        let parts = Layout::vertical([
            Constraint::Length(2),
            Constraint::Length(3),
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(3),
        ])
        .split(area);
        let provider = form.providers[form.provider];
        frame.render_widget(
            Paragraph::new(format!("Source: {} · F2 changes source", provider.label()))
                .style(Style::default().fg(ACCENT)),
            parts[0],
        );
        app.ui.hits.push((parts[0], Hit::AppProvider));
        let block = render::pane(" Application package ", app.modal_focus == 2);
        let inner = block.inner(parts[1]);
        frame.render_widget(block, parts[1]);
        let (text, cursor) = form.target.visible(inner.width);
        frame.render_widget(
            Paragraph::new(if text.is_empty() {
                if provider == manager::Provider::Brew {
                    "firefox"
                } else {
                    "org.mozilla.firefox"
                }
            } else {
                &text
            })
            .style(Style::default().fg(if text.is_empty() { DIM } else { FG })),
            inner,
        );
        app.ui.modal_input = inner;
        app.ui.hits.push((inner, Hit::ModalInput));
        if app.modal_focus == 2 && inner.width > 0 {
            frame.set_cursor_position((inner.x + cursor, inner.y));
        }
        frame.render_widget(
            Paragraph::new(form.error.clone().unwrap_or_default())
                .style(Style::default().fg(DANGER))
                .wrap(Wrap { trim: false }),
            parts[2],
        );
        frame.render_widget(Paragraph::new("Review the exact native command next. The package manager keeps its own prompts and verification.").style(Style::default().fg(DIM)).wrap(Wrap{trim:false}),parts[3]);
        let enabled = !form.target.text.trim().is_empty();
        render::modal_buttons(frame, app, parts[4], "Review install", enabled);
        return true;
    }
    if let Some(Modal::Managed(plan)) = &app.modal {
        let plan = plan.clone();
        let parts = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(3),
        ])
        .split(area);
        frame.render_widget(
            Paragraph::new(format!(
                "{} {}?",
                if plan.uninstall {
                    "Uninstall"
                } else {
                    "Install"
                },
                plan.target
            ))
            .style(Style::default().fg(if plan.uninstall { DANGER } else { ACCENT }))
            .wrap(Wrap { trim: false }),
            parts[0],
        );
        frame.render_widget(
            Paragraph::new(vec![
                Line::raw(plan.command.clone()),
                Line::raw(""),
                Line::raw(plan.note.clone()),
                Line::raw(""),
                Line::styled(
                    "The terminal temporarily returns to the native manager.",
                    Style::default().fg(DIM),
                ),
            ])
            .wrap(Wrap { trim: false }),
            parts[1],
        );
        render::modal_buttons(
            frame,
            app,
            parts[2],
            if plan.uninstall {
                "Uninstall"
            } else {
                "Install"
            },
            true,
        );
        return true;
    }
    false
}

pub(super) struct State {
    pub context: Option<manager::Context>,
    pub catalog: manager::Catalog,
    pub leftovers: Vec<manager::Application>,
    pub filter: Editor,
    pub show_leftovers: bool,
    pub loading: bool,
    pub inflight: bool,
    pub pending: bool,
    pub generation: u64,
}
impl State {
    pub fn new(context: Option<manager::Context>) -> Self {
        Self {
            context: context.or_else(|| manager::Context::system().ok()),
            catalog: Default::default(),
            leftovers: vec![],
            filter: Default::default(),
            show_leftovers: false,
            loading: false,
            inflight: false,
            pending: false,
            generation: 0,
        }
    }
}
pub(super) struct Loaded {
    pub catalog: manager::Catalog,
    pub leftovers: Vec<manager::Application>,
}
#[derive(Clone)]
pub(super) struct Files {
    pub app: manager::Application,
    pub rows: Vec<manager::RelatedFile>,
    pub selected: HashSet<usize>,
    pub cursor: usize,
    pub uninstall: bool,
    pub error: Option<String>,
}
pub(super) struct Install {
    pub target: Editor,
    pub providers: Vec<manager::Provider>,
    pub provider: usize,
    pub error: Option<String>,
}

impl App {
    pub(super) fn app_rows(&self) -> &[manager::Application] {
        if self.apps.show_leftovers {
            &self.apps.leftovers
        } else {
            &self.apps.catalog.apps
        }
    }
    pub(super) fn app_target(&self) -> Option<&manager::Application> {
        self.table
            .selected()
            .and_then(|row| self.ids.get(row))
            .and_then(|id| self.app_rows().get(*id as usize))
    }
    pub(super) fn apps_filter(&mut self, focused: Option<String>) {
        let focused = focused.or_else(|| self.app_target().map(manager::Application::key));
        let terms: Vec<_> = self
            .apps
            .filter
            .text
            .to_lowercase()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        self.ids = self
            .app_rows()
            .iter()
            .enumerate()
            .filter(|(_, a)| {
                let text = format!("{} {} {}", a.name, a.id, a.source_label()).to_lowercase();
                terms.iter().all(|term| text.contains(term))
            })
            .map(|(i, _)| i as u32)
            .collect();
        self.selected.clear();
        let row = focused
            .and_then(|key| {
                self.ids
                    .iter()
                    .position(|id| self.app_rows()[*id as usize].key() == key)
            })
            .unwrap_or(0);
        self.select_row(row);
    }
    pub(super) fn apps_reload(&mut self) {
        self.apps.generation = self.apps.generation.wrapping_add(1);
        self.apps.pending = true;
        self.apps.loading = true;
        self.start_apps();
    }
    pub(super) fn start_apps(&mut self) {
        if self.apps.inflight || !self.apps.pending {
            return;
        }
        let Some(context) = self.apps.context.clone() else {
            self.apps.pending = false;
            self.apps.loading = false;
            self.status = "Cannot locate application folders".into();
            return;
        };
        self.apps.inflight = true;
        self.apps.pending = false;
        let (ticket, state, tx) = (self.apps.generation, self.state.clone(), self.tx.clone());
        thread::spawn(move || {
            let mut catalog = manager::inventory(&context);
            let leftovers = match manager::leftovers(&context, &catalog, &state) {
                Ok(data) => data,
                Err(error) => {
                    catalog.warnings.push(format!("App history: {error}"));
                    vec![]
                }
            };
            let _ = tx.send(Message::Apps(ticket, Loaded { catalog, leftovers }));
        });
    }
    pub(super) fn receive_apps(&mut self, ticket: u64, loaded: Loaded) {
        self.apps.inflight = false;
        if ticket == self.apps.generation {
            let focused = self.app_target().map(manager::Application::key);
            self.apps.catalog = loaded.catalog;
            self.apps.leftovers = loaded.leftovers;
            self.apps.loading = false;
            for warning in self.apps.catalog.warnings.clone() {
                self.record(format!("Apps: {warning}"));
            }
            if self.view == View::Apps {
                self.apps_filter(focused);
            }
        }
        self.start_apps();
    }
    pub(super) fn apps_enabled(&self, cmd: Command) -> Option<bool> {
        let ready = !self.busy
            && !self.applying
            && !self.auto_busy
            && self.scan.is_none()
            && !self.plan_inflight;
        match cmd {
            Command::Parent | Command::Reveal | Command::Select | Command::Auto | Command::Map
                if self.view == View::Apps =>
            {
                Some(false)
            }
            Command::Install => Some(self.view == View::Apps && ready),
            Command::AppFiles | Command::Uninstall => Some(
                self.view == View::Apps
                    && ready
                    && !self.apps.loading
                    && self
                        .app_target()
                        .is_some_and(|a| !a.protected || cmd == Command::AppFiles),
            ),
            Command::Leftovers | Command::RefreshApps => Some(self.view == View::Apps && ready),
            Command::Open if self.view == View::Apps => {
                Some(ready && !self.apps.loading && self.app_target().is_some())
            }
            _ => None,
        }
    }
    pub(super) fn app_command(&mut self, cmd: Command) -> bool {
        match cmd {
            Command::RefreshApps | Command::Rescan if self.view == View::Apps => self.apps_reload(),
            Command::Leftovers => {
                self.apps.show_leftovers = !self.apps.show_leftovers;
                self.apps_filter(None);
            }
            Command::Install => {
                let providers = self
                    .apps
                    .context
                    .as_ref()
                    .map(|c| c.tools.available())
                    .unwrap_or_default();
                if providers.is_empty() {
                    self.open_modal(Modal::Notice("Install Homebrew on macOS or Flatpak on Linux to use native app installation.".into()));
                } else {
                    self.open_modal(Modal::AppInstall(Install {
                        target: Default::default(),
                        providers,
                        provider: 0,
                        error: None,
                    }));
                    self.modal_focus = 2;
                }
            }
            Command::Uninstall if self.view == View::Apps => {
                let Some(app) = self.app_target().cloned() else {
                    return true;
                };
                if matches!(
                    app.source,
                    manager::Source::Brew { .. } | manager::Source::Flatpak { .. }
                ) {
                    let result = self
                        .apps
                        .context
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("App context unavailable"))
                        .and_then(|ctx| manager::uninstall_plan(&ctx.tools, &app));
                    match result {
                        Ok(plan) => self.open_modal(Modal::Managed(Box::new(plan))),
                        Err(error) => self.open_modal(Modal::Notice(error.to_string())),
                    }
                } else {
                    self.app_files(!matches!(app.source, manager::Source::Leftover));
                }
            }
            Command::Open | Command::AppFiles if self.view == View::Apps => self.app_files(false),
            _ => return false,
        }
        true
    }
    fn app_files(&mut self, uninstall: bool) {
        let (Some(app), Some(context)) = (self.app_target().cloned(), self.apps.context.clone())
        else {
            return;
        };
        if uninstall && !matches!(app.source, manager::Source::Bundle) {
            self.open_modal(Modal::Notice("This launcher belongs to another installer. Use its native package manager; Files reviews related data only.".into()));
            return;
        }
        self.cancel_plan();
        let ticket = self.plan_generation;
        self.busy = true;
        self.plan_inflight = true;
        self.status = "Finding related files… · Esc cancels".into();
        let catalog = self.apps.catalog.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let rows = manager::review_files(&context, &catalog, &app);
            let selected = rows
                .iter()
                .enumerate()
                .filter(|(_, file)| manager::default_file(&app, file, uninstall))
                .map(|(i, _)| i)
                .collect();
            let _ = tx.send(Message::AppFiles(
                ticket,
                Files {
                    app,
                    rows,
                    selected,
                    cursor: 0,
                    uninstall,
                    error: None,
                },
            ));
        });
    }
    pub(super) fn app_confirm(&mut self) -> bool {
        if matches!(self.modal, Some(Modal::Managed(_))) {
            if let Some(Modal::Managed(plan)) = self.modal.take() {
                self.pending_native = Some(*plan);
                self.applying = true;
                self.busy = true;
                self.ui.hits.clear();
            }
            return true;
        }
        if let Some(Modal::AppInstall(form)) = &mut self.modal {
            let result = self
                .apps
                .context
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("App context unavailable"))
                .and_then(|ctx| {
                    manager::install_plan(
                        &ctx.tools,
                        form.providers[form.provider],
                        &form.target.text,
                    )
                });
            match result {
                Ok(plan) => self.open_modal(Modal::Managed(Box::new(plan))),
                Err(error) => {
                    if let Some(Modal::AppInstall(form)) = &mut self.modal {
                        form.error = Some(error.to_string());
                        self.modal_focus = 2;
                    }
                }
            }
            return true;
        }
        if let Some(Modal::AppFiles(review)) = &mut self.modal {
            let paths: Vec<_> = review
                .selected
                .iter()
                .filter_map(|i| review.rows.get(*i))
                .filter(|f| f.selectable)
                .map(|f| f.path.clone())
                .collect();
            if paths.is_empty() {
                review.error = Some("Select at least one item".into());
                return true;
            }
            if review.uninstall
                && !review.selected.iter().any(|i| {
                    review
                        .rows
                        .get(*i)
                        .is_some_and(|f| f.kind == manager::RelatedKind::Application)
                })
            {
                review.error = Some(
                    "Select the app bundle to uninstall it; Files reviews data separately".into(),
                );
                return true;
            }
            let subject = review.app.clone();
            self.modal = None;
            self.cancel_plan();
            let ticket = self.plan_generation;
            self.busy = true;
            self.plan_inflight = true;
            self.status = "Verifying selected files… · Esc cancels".into();
            let (state, index, tx) = (self.state.clone(), self.index.clone(), self.tx.clone());
            thread::spawn(move || {
                let result = ops::plan_app_trash(
                    std::path::Path::new("/"),
                    &paths,
                    &state,
                    index.as_deref(),
                    Some(&subject),
                )
                .map_err(|e| format!("{e:#}"));
                let _ = tx.send(Message::Plan(ticket, result));
            });
            return true;
        }
        false
    }
    pub(super) fn app_modal_key(&mut self, key: KeyEvent) -> bool {
        if let Some(Modal::AppFiles(review)) = &mut self.modal {
            if key.code == KeyCode::Tab || key.code == KeyCode::BackTab {
                let reverse =
                    key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT);
                self.modal_focus = (self.modal_focus + if reverse { 2 } else { 1 }) % 3;
                return true;
            }
            if self.modal_focus == 2 {
                match key.code {
                    KeyCode::Up => review.cursor = review.cursor.saturating_sub(1),
                    KeyCode::Down => {
                        review.cursor = (review.cursor + 1).min(review.rows.len().saturating_sub(1))
                    }
                    KeyCode::PageUp => review.cursor = review.cursor.saturating_sub(8),
                    KeyCode::PageDown => {
                        review.cursor = (review.cursor + 8).min(review.rows.len().saturating_sub(1))
                    }
                    KeyCode::Home => review.cursor = 0,
                    KeyCode::End => review.cursor = review.rows.len().saturating_sub(1),
                    KeyCode::Char(' ') => {
                        if review.rows.get(review.cursor).is_some_and(|f| f.selectable)
                            && !review.selected.remove(&review.cursor)
                        {
                            review.selected.insert(review.cursor);
                        }
                    }
                    KeyCode::Enter => {
                        self.app_confirm();
                    }
                    _ => {}
                }
                self.modal_scroll = self.modal_scroll.min(self.ui.modal_max);
                return true;
            }
        }
        if let Some(Modal::AppInstall(form)) = &mut self.modal {
            if key.code == KeyCode::F(2) {
                form.provider = (form.provider + 1) % form.providers.len();
                form.error = None;
                return true;
            }
            if self.modal_focus == 2 && !matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
                if key.code == KeyCode::Enter {
                    self.app_confirm();
                } else {
                    form.target.key(key);
                    form.error = None;
                }
                return true;
            }
        }
        false
    }
    pub(super) fn app_key(&mut self, key: KeyEvent) -> bool {
        if self.view != View::Apps {
            return false;
        }
        if self.focus == Focus::Query {
            match key.code {
                KeyCode::Up => self.focus = Focus::Nav,
                KeyCode::Down | KeyCode::Enter | KeyCode::Esc => self.focus = Focus::List,
                _ => {
                    if self.apps.filter.key(key) {
                        self.apps_filter(None);
                    }
                }
            }
            return true;
        }
        if matches!(self.focus, Focus::Nav | Focus::Actions) {
            return false;
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            return false;
        }
        match key.code {
            KeyCode::Char('/') => self.focus = Focus::Query,
            KeyCode::Char('i') => {
                self.command(Command::Install);
            }
            KeyCode::Char('f') => {
                self.command(Command::AppFiles);
            }
            KeyCode::Char('r') => {
                self.command(Command::RefreshApps);
            }
            KeyCode::Char('l') => {
                self.command(Command::Leftovers);
            }
            KeyCode::Delete | KeyCode::Char('x') => {
                self.command(Command::Uninstall);
            }
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {}
            KeyCode::Char('s' | 'p' | 'm' | 't' | 'a' | 'g' | ' ') => {}
            _ => return false,
        }
        true
    }
}
