use super::navigation::MENU;
use super::theme::*;
use super::*;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Clear, HighlightSpacing, Paragraph, Row, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Table, Wrap,
    },
};
use unicode_width::UnicodeWidthStr;

fn pane(title: impl Into<Line<'static>>, focused: bool) -> Block<'static> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(if focused { ACCENT } else { BORDER }))
        .style(Style::default().bg(PANEL).fg(FG))
}
fn label(cmd: Command, compact: bool) -> &'static str {
    match (cmd, compact) {
        (Command::Back, true) => "←",
        (Command::Back, false) => "← Back",
        (Command::Forward, true) => "→",
        (Command::Forward, false) => "→ Forward",
        (Command::Parent, true) => "↑ Up",
        (Command::Parent, false) => "↑ Up",
        (Command::Open, true) => "Open",
        (Command::Open, false) => "↵ Open",
        (Command::Find, true) => "Find",
        (Command::Find, false) => "/ Find",
        (Command::Map, true) => "Map",
        (Command::Map, false) => "g Map",
        (Command::Select, true) => "Mark",
        (Command::Select, false) => "Space Mark",
        (Command::Trash, true) => "Trash",
        (Command::Trash, false) => "Del Trash",
        (Command::Move, true) => "Move",
        (Command::Move, false) => "m Move",
        (Command::Clean, true) => "Clean",
        (Command::Clean, false) => "x Clean",
        (Command::Undo, true) => "Undo",
        (Command::Undo, false) => "u Undo",
        (Command::Rescan, true) => "Scan",
        (Command::Rescan, false) => "r Scan",
        (Command::Auto, true) => "Auto",
        (Command::Auto, false) => "a Auto",
        (Command::Help, true) => "?",
        (Command::Help, false) => "? Help",
        (Command::Quit, true) => "Quit",
        (Command::Quit, false) => "q Quit",
        _ => "Open",
    }
}
fn toolbar(width: u16, height: u16) -> (Vec<(Command, Rect)>, u16, bool) {
    let commands = [
        Command::Back,
        Command::Forward,
        Command::Parent,
        Command::Open,
        Command::Find,
        Command::Map,
        Command::Select,
        Command::Trash,
        Command::Move,
        Command::Clean,
        Command::Undo,
        Command::Rescan,
        Command::Auto,
        Command::Help,
        Command::Quit,
    ];
    let compact = width < 160 || height < 28;
    let mut x = 0;
    let mut y = 0;
    let mut result = Vec::new();
    for cmd in commands {
        if (height < 20 || width < 60)
            && !matches!(
                cmd,
                Command::Parent
                    | Command::Open
                    | Command::Trash
                    | Command::Clean
                    | Command::Undo
                    | Command::Help
            )
        {
            continue;
        }
        if height < 28
            && width >= 60
            && matches!(
                cmd,
                Command::Back | Command::Forward | Command::Open | Command::Quit
            )
        {
            continue;
        }
        let w = (label(cmd, compact).width() as u16 + 2).min(width);
        if x + w > width {
            x = 0;
            y += 1;
        }
        result.push((cmd, Rect::new(x, y, w, 1)));
        x += w + 1;
    }
    (result, y + 1, compact)
}

fn button(
    frame: &mut Frame,
    app: &mut App,
    rect: Rect,
    text: &str,
    hit: Hit,
    focused: bool,
    enabled: bool,
) {
    let active = enabled && (focused || app.hover == Some(hit) || app.pressed == Some(hit));
    let danger = matches!(hit, Hit::Command(Command::Trash | Command::Clean))
        || (hit == Hit::ModalConfirm
            && matches!(&app.modal,Some(Modal::Preview(plan)) if matches!(plan.action,Action::Trash|Action::Clean)));
    let accent = if danger { DANGER } else { ACCENT };
    let mut style = Style::default()
        .fg(if !enabled {
            BORDER
        } else if active {
            BG
        } else if danger {
            DANGER
        } else {
            FG
        })
        .bg(if active { accent } else { PANEL });
    if focused && app.focus == Focus::Nav && matches!(hit, Hit::View(_)) {
        style = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    }
    let text = if matches!(hit, Hit::ModalCancel | Hit::ModalConfirm) {
        format!("[ {text} ]")
    } else {
        format!(" {text} ")
    };
    frame.render_widget(Paragraph::new(text).style(style), rect);
    if enabled {
        app.ui.hits.push((rect, hit));
    }
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    app.ui = Ui::default();
    frame.render_widget(Block::default().style(Style::default().bg(BG).fg(FG)), area);
    if area.width < 48 || area.height < 14 {
        frame.render_widget(
            Paragraph::new("AXIT / WCKD.ai\nExpand to 48 × 14. Ctrl-Q quits.")
                .style(Style::default().fg(ACCENT)),
            area,
        );
        return;
    }
    let (buttons, footer_height, compact_toolbar) =
        toolbar(area.width.saturating_sub(2), area.height);
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Length(if app.view == View::Find { 3 } else { 2 }),
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(footer_height),
        ])
        .margin(1)
        .split(area);
    logo::draw(frame, layout[0]);
    let text_area = Rect::new(
        layout[0].x + 8,
        layout[0].y,
        layout[0].width.saturating_sub(8),
        1,
    );
    let mut brand = vec![
        Span::styled(
            "AXIT ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled("WCKD.ai", Style::default().fg(DIM)),
    ];
    let volume = crate::system::volume(&app.root).ok();
    if let Some(index) = &app.index
        && area.width >= 90
    {
        brand.push(Span::styled(
            format!("   {} indexed", index.count),
            Style::default().fg(DIM),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(brand)), text_area);
    if let Some(volume) = &volume {
        let available = volume.available.min(volume.total);
        frame.render_widget(
            Paragraph::new(format!(
                "{} available / {}",
                bytes(available),
                bytes(volume.total)
            ))
            .style(Style::default().fg(if available < 20 * 1024 * 1024 * 1024 {
                AMBER
            } else {
                INFO
            })),
            Rect::new(text_area.x, text_area.y + 1, text_area.width, 1),
        );
        let width = 20.min(text_area.width.saturating_sub(16));
        let used = (u128::from(width) * u128::from(volume.total.saturating_sub(available))
            / u128::from(volume.total.max(1))) as usize;
        let spans = vec![
            Span::styled("▰".repeat(used), Style::default().fg(BORDER)),
            Span::styled("▰".repeat(width as usize - used), Style::default().fg(MINT)),
            Span::styled(" used / available", Style::default().fg(DIM)),
        ];
        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(text_area.x, text_area.y + 2, text_area.width, 1),
        );
    }

    let mut x = layout[1].x;
    for (view, text) in [
        (View::Space, "1 Space"),
        (View::Find, "2 Find"),
        (View::Clean, "3 Clean"),
        (View::Activity, "4 Activity"),
    ] {
        let rect = Rect::new(x, layout[1].y, text.len() as u16 + 2, 1);
        button(
            frame,
            app,
            rect,
            text,
            Hit::View(view),
            app.view == view,
            true,
        );
        x = rect.right() + 1;
    }
    if layout[1].right().saturating_sub(x) >= 25 {
        frame.render_widget(
            Paragraph::new(if app.focus == Focus::Nav {
                "←→ sections · Enter opens"
            } else {
                "Ctrl-Tab next · F6 nav"
            })
            .style(Style::default().fg(DIM)),
            Rect::new(
                x + 1,
                layout[1].y,
                layout[1].right().saturating_sub(x + 1),
                1,
            ),
        );
    }
    if app.view == View::Find {
        let block = pane(
            if app.focus == Focus::Query {
                " Search · typing · Tab / Enter to results "
            } else {
                " Search · / or Ctrl-F to edit "
            },
            app.focus == Focus::Query,
        );
        let inner = block.inner(layout[2]);
        frame.render_widget(block, layout[2]);
        let (text, cursor) = app.query.visible(inner.width);
        frame.render_widget(
            Paragraph::new(if text.is_empty() {
                "Names, ext:pdf, size:>1gib…"
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
    } else {
        breadcrumbs(frame, app, layout[2]);
    }

    if app.view == View::Activity {
        let mut lines = vec![Line::styled("Operations", Style::default().fg(ACCENT))];
        if app.activity.is_empty() {
            lines.push(Line::raw("No operations in this session."));
        }
        lines.extend(
            app.activity
                .iter()
                .rev()
                .map(|s| Line::raw(crate::clean(s))),
        );
        if let Some(index) = &app.index {
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                format!(
                    "Coverage · {} restricted or missing locations",
                    index.errors
                ),
                Style::default().fg(AMBER),
            ));
            if index.errors > 0 {
                lines.push(Line::raw(
                    "Protected Mac locations require Full Disk Access for your terminal.",
                ));
            }
        }
        lines.extend(app.coverage.iter().map(|s| Line::raw(s.clone())));
        let block = pane(
            " Activity & coverage · ↑↓ / wheel to scroll ",
            app.focus == Focus::List,
        );
        let inner = block.inner(layout[3]);
        frame.render_widget(block, layout[3]);
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        app.ui.activity_max = max_scroll(&paragraph, inner);
        app.activity_scroll = app.activity_scroll.min(app.ui.activity_max);
        frame.render_widget(paragraph.scroll((app.activity_scroll, 0)), inner);
        app.ui.hits.push((inner, Hit::Activity));
        app.ui.page = inner.height as usize;
    } else if app.index.is_some() {
        let mut content = layout[3];
        if app.view == View::Space && app.show_map && content.height >= 12 {
            let height = if content.height >= 18 { 9 } else { 6 };
            let chart = Rect::new(content.x, content.y, content.width, height);
            usage::draw(frame, app, chart);
            content.y += height;
            content.height = content.height.saturating_sub(height);
        }
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(if area.width >= 110 {
                vec![Constraint::Percentage(66), Constraint::Percentage(34)]
            } else {
                vec![Constraint::Percentage(100)]
            })
            .spacing(1)
            .split(content);
        list(frame, app, columns[0]);
        if columns.len() > 1 {
            let block = pane(" Inspect · Tab to focus ", app.focus == Focus::Inspect);
            let inner = block.inner(columns[1]);
            frame.render_widget(block, columns[1]);
            let paragraph = Paragraph::new(inspect(app)).wrap(Wrap { trim: false });
            app.ui.inspect_max = max_scroll(&paragraph, inner);
            app.inspect_scroll = app.inspect_scroll.min(app.ui.inspect_max);
            frame.render_widget(paragraph.scroll((app.inspect_scroll, 0)), inner);
            app.ui.inspector = inner;
            app.ui.hits.push((inner, Hit::Inspect));
        } else if app.focus == Focus::Inspect {
            app.focus = Focus::List;
        }
    } else {
        let progress = app.progress.lock().map(|p| p.clone()).unwrap_or_default();
        frame.render_widget(Paragraph::new(format!("Building the first index…\n{} entries · {} restricted/missing\n\nMetadata only. Ctrl-Q cancels and quits.",progress.entries,progress.errors)).wrap(Wrap{trim:false}).block(pane(" Welcome to AXIT ",true)),layout[3]);
    }
    let status = if app.scan.is_some() {
        let p = app.progress.lock().map(|p| p.clone()).unwrap_or_default();
        format!(
            "Scanning: {} entries · {} inaccessible · {}",
            p.entries, p.errors, app.status
        )
    } else if app.needs_rescan {
        format!("{} · r refreshes indexed sizes", app.status)
    } else {
        app.status.clone()
    };
    let marked = if app.selected.is_empty() {
        String::new()
    } else {
        format!("{} marked · ", app.selected.len())
    };
    frame.render_widget(
        Paragraph::new(format!("{marked}{}", crate::clean(&status)))
            .style(Style::default().fg(if app.busy { ACCENT } else { DIM })),
        layout[4],
    );
    app.ui.commands = buttons.iter().map(|(cmd, _)| *cmd).collect();
    app.action_index = app
        .action_index
        .min(app.ui.commands.len().saturating_sub(1));
    if app.focus == Focus::Actions && !app.enabled(app.ui.commands[app.action_index]) {
        app.action_index = app
            .ui
            .commands
            .iter()
            .position(|cmd| app.enabled(*cmd))
            .unwrap_or(0);
    }
    for (i, (cmd, mut rect)) in buttons.into_iter().enumerate() {
        rect.x += layout[5].x;
        rect.y += layout[5].y;
        let enabled = app.enabled(cmd);
        button(
            frame,
            app,
            rect,
            label(cmd, compact_toolbar),
            Hit::Command(cmd),
            app.focus == Focus::Actions && app.action_index == i,
            enabled,
        );
    }
    if app.modal.is_some() {
        modal(frame, app, area);
    }
}

fn breadcrumbs(frame: &mut Frame, app: &mut App, area: Rect) {
    let title = format!(" Location · {} ", crate::text(&app.root));
    let block = pane(title, false).borders(Borders::TOP | Borders::LEFT | Borders::RIGHT);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(index) = app.index.clone() else {
        return;
    };
    let mut chain = Vec::new();
    let mut id = app.folder;
    while id != 0 {
        if let Some(entry) = index.entry(id) {
            chain.push((id, index.display_name(entry)));
            id = entry.parent;
        } else {
            break;
        }
    }
    chain.reverse();
    let root = Rect::new(inner.x, inner.y, 7.min(inner.width), 1);
    button(
        frame,
        app,
        root,
        "Scope",
        Hit::Crumb(0),
        app.folder == 0,
        true,
    );
    let mut x = root.right();
    let available = inner.right().saturating_sub(x);
    let mut start = 0;
    while start + 1 < chain.len()
        && chain[start..]
            .iter()
            .map(|(_, s)| s.width() + 5)
            .sum::<usize>()
            > available as usize
    {
        start += 1;
    }
    if start > 0 && x + 4 < inner.right() {
        frame.render_widget(
            Paragraph::new(" … / ").style(Style::default().fg(DIM)),
            Rect::new(x, inner.y, 5, 1),
        );
        x += 5;
    }
    for (id, name) in chain.into_iter().skip(start) {
        if x + 3 >= inner.right() {
            break;
        }
        frame.render_widget(
            Paragraph::new(" / ").style(Style::default().fg(DIM)),
            Rect::new(x, inner.y, 3, 1),
        );
        x += 3;
        let rect = Rect::new(
            x,
            inner.y,
            (name.width() as u16 + 2).min(inner.right() - x),
            1,
        );
        button(
            frame,
            app,
            rect,
            &name,
            Hit::Crumb(id),
            id == app.folder,
            true,
        );
        x = rect.right();
    }
}

fn list(frame: &mut Frame, app: &mut App, area: Rect) {
    let index = app.index.clone().unwrap();
    let title = if app.listing {
        " Loading indexed items… · navigation stays available ".to_string()
    } else {
        match app.view {
            View::Find => {
                if app.searching {
                    " Searching… ".into()
                } else {
                    format!(
                        " {} {} · {:.0} ms · {} listed ",
                        app.matched,
                        if app.matched == 1 { "match" } else { "matches" },
                        app.search_ms,
                        app.ids.len()
                    )
                }
            }
            View::Clean => format!(
                " Caches · {} of {} candidates · estimates ",
                app.ids.len(),
                app.folder_count
            ),
            _ => format!(
                " Files · {} listed{} ",
                app.ids.len(),
                if app.ids.len() == 1000 {
                    " · largest 1000"
                } else {
                    ""
                }
            ),
        }
    };
    let block = pane(title, app.focus == Focus::List);
    let inner = block.inner(area);
    let header = usize::from(inner.height >= 3);
    let data = Rect::new(
        inner.x,
        inner.y + header as u16,
        inner.width,
        inner.height.saturating_sub(header as u16),
    );
    app.ui.list = data;
    app.ui.page = data.height.max(1) as usize;
    app.ui.hits.push((inner, Hit::List));
    let rows = app
        .ids
        .iter()
        .filter_map(|id| index.entry(*id))
        .map(|entry| {
            let marker = if entry.flags & crate::index::RESTRICTED != 0 {
                "!"
            } else if entry.is_dir() {
                "▸"
            } else if entry.is_link() {
                "↗"
            } else {
                "·"
            };
            Row::new(vec![
                if app.selected.contains(&entry.id) {
                    "[✓]".into()
                } else {
                    "[ ]".into()
                },
                format!("{marker} {}", index.display_name(entry)),
                bytes(entry.allocated),
                age(entry.modified),
            ])
            .style(Style::default().fg(
                if entry.flags & crate::index::RESTRICTED != 0 {
                    AMBER
                } else {
                    FG
                },
            ))
        });
    let mut table = Table::new(
        rows,
        [
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(11),
            Constraint::Length(10),
        ],
    )
    .column_spacing(1)
    .block(block)
    .highlight_symbol("›")
    .highlight_spacing(HighlightSpacing::Always)
    .row_highlight_style(Style::default().bg(SELECTED).fg(FG));
    if header > 0 {
        let name = if app.sort == Sort::Name {
            "Name ↑"
        } else {
            "Name"
        };
        let disk = if app.sort == Sort::Size {
            "On disk ↓"
        } else {
            "On disk"
        };
        let date = if app.sort == Sort::Modified {
            "Modified ↓"
        } else {
            "Modified"
        };
        table = table.header(Row::new(["", name, disk, date]).style(Style::default().fg(DIM)));
        let name_width = inner.width.saturating_sub(28);
        let left = inner.x + 5;
        for (x, width, sort) in [
            (left, name_width, Sort::Name),
            (left + name_width + 1, 11, Sort::Size),
            (left + name_width + 13, 10, Sort::Modified),
        ] {
            app.ui
                .hits
                .push((Rect::new(x, inner.y, width, 1), Hit::Sort(sort)));
        }
    }
    frame.render_stateful_widget(table, area, &mut app.table);
    for visible in 0..data.height as usize {
        let row = app.table.offset() + visible;
        if row >= app.ids.len() {
            break;
        }
        let rect = Rect::new(data.x, data.y + visible as u16, data.width, 1);
        app.ui.hits.push((rect, Hit::Row(row)));
        app.ui.hits.push((
            Rect::new(data.x + 1, rect.y, 3.min(data.width.saturating_sub(1)), 1),
            Hit::Check(row),
        ));
    }
    if app.ids.len() > data.height as usize && data.height > 1 {
        let mut state = ScrollbarState::new(app.ids.len().saturating_sub(data.height as usize))
            .position(app.table.offset());
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .thumb_style(Style::default().fg(ACCENT))
                .track_style(Style::default().fg(BORDER)),
            Rect::new(area.x, data.y, area.width, data.height),
            &mut state,
        );
        app.ui.hits.push((
            Rect::new(area.right() - 1, data.y, 1, data.height),
            Hit::Scrollbar,
        ));
    }
    if app.ids.is_empty() {
        let message = if let Some(error) = &app.search_error
            && app.view == View::Find
        {
            format!("Search could not run:\n\n{error}")
        } else if app.searching {
            "Searching…".into()
        } else if app.listing {
            "Reading indexed items… Navigation stays available.".into()
        } else if app.view == View::Find {
            "No matching files. Edit the query or press Esc.".into()
        } else if app.view == View::Clean {
            "No generated cache candidates in this index.".into()
        } else if index
            .entry(app.folder)
            .is_some_and(|e| e.flags & crate::index::RESTRICTED != 0)
        {
            "Access restricted. Indexed contents are incomplete; see 4 Activity.".into()
        } else {
            "This folder has no indexed children.".into()
        };
        frame.render_widget(
            Paragraph::new(message)
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(DIM)),
            data,
        );
    }
}

fn age(modified: i64) -> String {
    if modified <= 0 {
        return "unknown".into();
    }
    let secs = (crate::unix_now() - modified).max(0);
    if secs < 60 {
        "now".into()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}
fn inspect(app: &App) -> Vec<Line<'static>> {
    let Some(index) = &app.index else {
        return vec![];
    };
    let Some(entry) = app.focused().and_then(|id| index.entry(id)) else {
        return vec![Line::styled(
            "Choose an item to inspect.",
            Style::default().fg(DIM),
        )];
    };
    let path = index.path(entry.id).unwrap_or_default();
    let mut lines = vec![
        Line::styled(
            index.display_name(entry),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
        Line::styled(crate::text(&path), Style::default().fg(DIM)),
        Line::raw(""),
        Line::raw(format!("On disk   {}", bytes(entry.allocated))),
        Line::raw(if entry.is_dir() {
            "Type      Folder".into()
        } else {
            format!("File size {}", bytes(entry.logical))
        }),
        Line::raw(format!("Modified  {}", age(entry.modified))),
        Line::raw(""),
    ];
    if let Some(label) = ops::cache_label(&path) {
        lines.push(Line::styled(label, Style::default().fg(MINT)));
        lines.push(Line::raw(
            "Clean checks ownership, activity and protected contents before offering a plan.",
        ));
    } else {
        lines.push(Line::raw("Trash and moves are reversible through Undo."));
    }
    if entry.flags & crate::index::ALIAS != 0 {
        lines.push(Line::styled(
            "Hardlink · blocks counted elsewhere.",
            Style::default().fg(DIM),
        ));
    }
    if entry.flags & crate::index::RESTRICTED != 0 {
        lines.push(Line::styled(
            "Access restricted · indexed size is incomplete.",
            Style::default().fg(AMBER),
        ));
    }
    if app.needs_rescan {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Indexed sizes may be stale. Press r to refresh.",
            Style::default().fg(AMBER),
        ));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "p Show containing folder",
        Style::default().fg(DIM),
    ));
    if index.errors > 0 {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            format!("{} unavailable locations · 4 Activity", index.errors),
            Style::default().fg(AMBER),
        ));
    }
    lines
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width.saturating_sub(2));
    let h = height.min(area.height.saturating_sub(2));
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}
fn max_scroll(paragraph: &Paragraph, area: Rect) -> u16 {
    paragraph
        .line_count(area.width)
        .saturating_sub(area.height as usize)
        .min(u16::MAX as usize) as u16
}
fn scrollable(frame: &mut Frame, app: &mut App, lines: Vec<Line<'static>>, area: Rect) {
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    app.ui.modal_max = max_scroll(&paragraph, area);
    app.modal_scroll = app.modal_scroll.min(app.ui.modal_max);
    frame.render_widget(paragraph.scroll((app.modal_scroll, 0)), area);
    app.ui.hits.push((area, Hit::ModalBody));
}

fn modal(frame: &mut Frame, app: &mut App, area: Rect) {
    // Only explicit section navigation remains available behind a dialog.
    app.ui.hits.retain(|(_, hit)| matches!(hit, Hit::View(_)));
    let container = if area.height >= 20 {
        Rect::new(area.x, area.y + 5, area.width, area.height - 5)
    } else {
        area
    };
    let box_area = if matches!(app.modal, Some(Modal::Menu(_))) {
        centered(container, 40, MENU.len() as u16 + 3)
    } else if let Some(Modal::Preview(plan)) = &app.modal {
        let rows = (plan.entries.len() + plan.blocked.len()).min(7) as u16;
        centered(container, 78, (10 + rows * 2).clamp(12, 24))
    } else if matches!(app.modal, Some(Modal::Destination(_) | Modal::Auto)) {
        centered(container, 76, 19)
    } else {
        centered(container, 92, 32)
    };
    frame.render_widget(Clear, box_area);
    let title = match &app.modal {
        Some(Modal::Preview(..)) => " Review plan ",
        Some(Modal::Destination(_)) => " Move to folder ",
        Some(Modal::Auto) => " Automatic cleanup ",
        Some(Modal::Help) => " AXIT / keyboard & mouse ",
        Some(Modal::Inspect) => " File details ",
        Some(Modal::Menu(_)) => " Actions ",
        _ => " AXIT ",
    };
    let mut block = pane(title, true);
    if matches!(&app.modal,Some(Modal::Preview(plan)) if plan.action == Action::Clean) {
        block = block.border_style(Style::default().fg(DANGER));
    }
    let inside = block.inner(box_area);
    frame.render_widget(block, box_area);
    if let Some(Modal::Menu(selected)) = &app.modal {
        let selected = *selected;
        for (i, (cmd, text)) in MENU.iter().enumerate() {
            let rect = Rect::new(inside.x, inside.y + i as u16, inside.width, 1);
            if rect.y >= inside.bottom().saturating_sub(1) {
                break;
            }
            button(
                frame,
                app,
                rect,
                text,
                Hit::Menu(i),
                selected == i,
                app.enabled(*cmd),
            );
        }
        let rect = Rect::new(
            inside.x,
            inside.bottom().saturating_sub(1),
            10.min(inside.width),
            1,
        );
        button(frame, app, rect, "Cancel", Hit::ModalCancel, false, true);
        return;
    }
    if matches!(app.modal, Some(Modal::Preview(..))) {
        preview(frame, app, inside);
        return;
    }
    if matches!(app.modal, Some(Modal::Destination(_))) {
        let parts = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inside);
        frame.render_widget(Paragraph::new("Existing destination folder, inside the indexed scope.\nRelative paths start in the current folder; ~/ is supported.").wrap(Wrap{trim:false}),parts[0]);
        let field = pane(" Destination · Ctrl-U clears ", app.modal_focus == 2);
        let input_area = field.inner(parts[1]);
        frame.render_widget(field, parts[1]);
        if let Some(Modal::Destination(input)) = &mut app.modal {
            let (text, cursor) = input.visible(input_area.width);
            frame.render_widget(Paragraph::new(text), input_area);
            if app.modal_focus == 2 {
                frame.set_cursor_position((input_area.x + cursor, input_area.y));
            }
        }
        app.ui.modal_input = input_area;
        app.ui.hits.push((input_area, Hit::ModalInput));
        frame.render_widget(Paragraph::new("Enter previews the move. Tab chooses a button. Nothing moves before the reviewed plan is confirmed.").wrap(Wrap{trim:false}).style(Style::default().fg(DIM)),parts[2]);
        modal_buttons(frame, app, parts[3], "Preview move", true);
        return;
    }
    let (lines, confirmation) = match &app.modal {
        Some(Modal::Help) => (
            vec![
                "1 Space · 2 Find · 3 Clean · 4 Activity · Ctrl-Tab cycles views",
                "Ctrl-Tab / Ctrl-PgDn Next section · Ctrl-Shift-Tab / Ctrl-PgUp Previous",
                "Alt-1…4 Switch anywhere, including dialogs · F6 Focus navigation",
                "Navigation: ←→ choose a section · Enter / Down opens it",
                "Space map: g shows/hides · click selects · double-click opens",
                "/ or Ctrl-F Search · Tab / Shift-Tab cycles navigation and content",
                "↑↓ / j k Move · Home / End First / last · PgUp / PgDn One page",
                "Enter / → Open folder or inspect file · Backspace / ← Parent",
                "Alt-← / Alt-→ History · Alt-↑ Parent · Alt-Home Scope root",
                "Space Toggle mark · Shift-↑↓ Range · Ctrl-A Mark listed items",
                "Esc Clear marks / leave search · p Show containing folder",
                "Delete / t Trash preview · m Move · x Cache-cleanup preview",
                "u Undo last move · s Sort · click column headings to sort",
                "r Refresh index · a Automatic cleanup · q / Ctrl-Q Quit",
                "",
                "Click a row to focus; double-click to open or inspect.",
                "Click [ ] to mark; Ctrl-click toggles; Shift-click selects a range.",
                "Drag across rows to select. Right-click opens item actions.",
                "Wheel / touchpad scrolls the list, inspector or open dialog.",
                "Click breadcrumbs, tabs and action buttons to navigate.",
                "Dialogs: Tab chooses buttons; Enter activates; Esc cancels.",
                "Confirmation: Cancel starts focused · Tab / ←→ chooses a button.",
                "Enter activates the chosen button · Ctrl-Tab leaves without applying.",
                "",
                "Search editing: ←→, Home/End, Ctrl-A/E, Ctrl-U clear, Ctrl-K cut tail.",
                "Filters: ext:pdf size:>1gib older:30d type:dir path:project",
                "Quoted words, *.zip wildcards, !excluded and re:expressions.",
                "",
                "Sizes are indexed estimates; folder totals overlap. Refresh when ready.",
                "Whole Rust targets and release profiles are protected from Clean.",
                "Automatic cleanup stays off unless explicitly enabled.",
                "Hold Shift for your terminal's normal text selection / context menu.",
            ]
            .into_iter()
            .map(|s| Line::raw(s.to_string()))
            .collect(),
            None,
        ),
        Some(Modal::Inspect) => (inspect(app), None),
        Some(Modal::Notice(text)) => (vec![Line::raw(crate::clean(text))], None),
        Some(Modal::Auto) => {
            let policy = ops::auto_policy(&app.state).unwrap_or_default();
            let path = app
                .index
                .as_ref()
                .and_then(|i| i.path(app.folder).ok())
                .unwrap_or(app.root.clone());
            let mut lines = vec![
                Line::styled(
                    format!(
                        "Currently {}",
                        if policy.enabled { "ENABLED" } else { "OFF" }
                    ),
                    Style::default().fg(ACCENT),
                ),
                Line::raw(""),
                Line::raw(format!("Folder: {}", crate::text(&path))),
                Line::raw(""),
                Line::raw(format!("Minimum age: {} days", policy.minimum_age_days)),
                Line::raw(format!(
                    "At most {} per run · only below {} free",
                    bytes(policy.max_bytes),
                    bytes(policy.below_free_bytes)
                )),
                Line::raw(format!(
                    "Cooldown: {} hours · while AXIT is running",
                    policy.cooldown_secs / 3600
                )),
                Line::raw(""),
                Line::raw(
                    "Enabling authorizes permanent cleanup of verified generated caches in this folder. Entire home/disk roots are rejected.",
                ),
            ];
            if policy.enabled {
                lines.push(Line::raw(format!(
                    "Active roots: {}",
                    policy.roots.join(", ")
                )));
            }
            (
                lines,
                Some(if policy.enabled {
                    "Disable cleanup"
                } else {
                    "Enable for folder"
                }),
            )
        }
        _ => (vec![], None),
    };
    let parts = Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).split(inside);
    scrollable(frame, app, lines, parts[0]);
    frame.render_widget(
        Paragraph::new("↑↓ / PgUp / PgDn / wheel · Esc closes").style(Style::default().fg(DIM)),
        Rect::new(parts[1].x, parts[1].y, parts[1].width, 1),
    );
    modal_buttons(
        frame,
        app,
        Rect::new(parts[1].x, parts[1].y + 1, parts[1].width, 1),
        confirmation.unwrap_or("Close"),
        confirmation.is_some(),
    );
}

fn preview(frame: &mut Frame, app: &mut App, inside: Rect) {
    let Some(Modal::Preview(plan)) = &app.modal else {
        return;
    };
    let count = plan.entries.len();
    let noun = if count == 1 { "item" } else { "items" };
    let (question, consequence, confirm) = match &plan.action {
        Action::Trash => (
            format!("Move {count} {noun} to Trash?"),
            "Undo can restore them. Empty Trash later to free space.".to_string(),
            "Move to Trash",
        ),
        Action::Clean => (
            format!("Permanently delete {count} caches?"),
            "Verified generated files only. This cannot be undone.".to_string(),
            "Delete permanently",
        ),
        Action::Move { destination } => (
            format!("Move {count} {noun}?"),
            format!(
                "To {} · existing files are never replaced.",
                crate::clean(destination)
            ),
            "Move files",
        ),
    };
    let question = if count == 0 {
        "Nothing eligible to change".into()
    } else {
        question
    };
    let mut lines = Vec::new();
    for entry in &plan.entries {
        lines.push(Line::styled(entry.path.clone(), Style::default().fg(FG)));
        lines.push(Line::styled(
            format!("  {} · {}", bytes(entry.bytes), entry.reason),
            Style::default().fg(DIM),
        ));
    }
    for blocked in &plan.blocked {
        lines.push(Line::styled(
            blocked.path.clone(),
            Style::default().fg(AMBER),
        ));
        lines.push(Line::styled(
            format!("  Skipped · {}", blocked.reason),
            Style::default().fg(DIM),
        ));
    }
    let summary = format!(
        "{} on disk · {} skipped",
        bytes(plan.total_bytes),
        plan.blocked.len()
    );
    let danger = plan.action == Action::Clean;
    let parts = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(3),
    ])
    .split(inside);
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                question,
                Style::default()
                    .fg(if danger { DANGER } else { FG })
                    .add_modifier(Modifier::BOLD),
            ),
            Line::styled(summary, Style::default().fg(DIM)),
        ]),
        parts[0],
    );
    frame.render_widget(
        Paragraph::new(consequence)
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(if danger { DANGER } else { DIM })),
        parts[1],
    );
    scrollable(frame, app, lines, parts[2]);
    let footer = parts[3];
    modal_buttons(
        frame,
        app,
        Rect::new(footer.x, footer.y + 1, footer.width, 1),
        confirm,
        count > 0,
    );
    frame.render_widget(
        Paragraph::new("Tab / ←→ buttons · Enter choose · Esc cancel")
            .style(Style::default().fg(DIM)),
        Rect::new(footer.x, footer.y + 2, footer.width, 1),
    );
}

fn modal_buttons(frame: &mut Frame, app: &mut App, area: Rect, confirm: &str, enabled: bool) {
    let confirm_width = if enabled {
        (confirm.width() as u16 + 4).min(area.width.saturating_sub(12))
    } else {
        0
    };
    let cancel_width = if enabled { 10 } else { 9 }.min(area.width);
    let total = cancel_width + if enabled { confirm_width + 2 } else { 0 };
    let cancel = Rect::new(area.right().saturating_sub(total), area.y, cancel_width, 1);
    button(
        frame,
        app,
        cancel,
        if enabled { "Cancel" } else { "Close" },
        Hit::ModalCancel,
        app.modal_focus == 0,
        true,
    );
    if enabled {
        let rect = Rect::new(
            area.right().saturating_sub(confirm_width),
            area.y,
            confirm_width,
            1,
        );
        button(
            frame,
            app,
            rect,
            confirm,
            Hit::ModalConfirm,
            app.modal_focus == 1,
            true,
        );
    }
}
