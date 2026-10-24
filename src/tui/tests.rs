use super::*;
use crate::index;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};
use std::fs;

fn fixture(files: usize) -> (tempfile::TempDir, App) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("files");
    let state = temp.path().join("state");
    fs::create_dir_all(root.join("folder/nested")).unwrap();
    fs::create_dir_all(root.join("destination")).unwrap();
    fs::create_dir(&state).unwrap();
    for n in 0..files {
        fs::write(root.join(format!("file-{n:03}.txt")), b"fixture").unwrap();
    }
    fs::write(root.join("folder/inside.txt"), b"inside").unwrap();
    let root = root.canonicalize().unwrap();
    let state = state.canonicalize().unwrap();
    index::scan(
        &root,
        &state,
        Arc::new(Mutex::new(Progress::default())),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    (temp, App::new(root, state))
}
fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> bool {
    app.handle_event(Event::Key(KeyEvent::new(code, modifiers)))
}
fn plain(app: &mut App, code: KeyCode) -> bool {
    key(app, code, KeyModifiers::NONE)
}
fn render(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| draw(f, app)).unwrap();
    let b = terminal.backend().buffer();
    (0..height)
        .map(|y| (0..width).map(|x| b[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}
fn point(app: &App, hit: Hit) -> (u16, u16) {
    let rect = app
        .ui
        .hits
        .iter()
        .rev()
        .find(|(_, h)| *h == hit)
        .expect("visible hit target")
        .0;
    (rect.x + rect.width / 2, rect.y)
}
fn mouse(app: &mut App, kind: MouseEventKind, point: (u16, u16), modifiers: KeyModifiers) {
    app.handle_event(Event::Mouse(MouseEvent {
        kind,
        column: point.0,
        row: point.1,
        modifiers,
    }));
}
fn click(app: &mut App, hit: Hit) {
    let p = point(app, hit);
    mouse(
        app,
        MouseEventKind::Down(MouseButton::Left),
        p,
        KeyModifiers::NONE,
    );
    mouse(
        app,
        MouseEventKind::Up(MouseButton::Left),
        p,
        KeyModifiers::NONE,
    );
}
fn id(app: &App, relative: &str) -> u32 {
    app.index
        .as_ref()
        .unwrap()
        .find_path(&app.root.join(relative))
        .unwrap()
}
fn focus(app: &mut App, relative: &str) -> u32 {
    let id = id(app, relative);
    let row = app.ids.iter().position(|value| *value == id).unwrap();
    app.select_row(row);
    id
}
fn settle(app: &mut App) {
    for _ in 0..300 {
        app.messages();
        if !app.busy && !app.searching && app.scan.is_none() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("UI job did not complete: {}", app.status);
}

#[test]
fn folder_navigation_clears_marks_and_parent_restores_the_child_cursor() {
    let (_temp, mut app) = fixture(4);
    let folder = focus(&mut app, "folder");
    plain(&mut app, KeyCode::Char(' '));
    assert_eq!(app.selected.len(), 1);
    plain(&mut app, KeyCode::Enter);
    assert_eq!(app.folder, folder);
    assert!(app.selected.is_empty());
    plain(&mut app, KeyCode::Backspace);
    assert_eq!(app.folder, 0);
    assert_eq!(app.focused(), Some(folder));
    key(&mut app, KeyCode::Left, KeyModifiers::ALT);
    assert_eq!(app.folder, folder);
    key(&mut app, KeyCode::Right, KeyModifiers::ALT);
    assert_eq!(app.folder, 0);
    assert_eq!(app.focused(), Some(folder));
}

#[test]
fn late_search_results_cannot_replace_a_folder_view() {
    let (_temp, mut app) = fixture(4);
    plain(&mut app, KeyCode::Char('/'));
    let generation = app.generation.load(Ordering::Relaxed);
    plain(&mut app, KeyCode::Esc);
    plain(&mut app, KeyCode::Char('1'));
    let expected = app.ids.clone();
    app.tx
        .send(Message::Search(
            generation,
            Ok(query::SearchResult {
                ids: vec![],
                matched: 0,
                elapsed_ms: 0.,
                canceled: false,
            }),
        ))
        .unwrap();
    app.messages();
    assert_eq!(app.view, View::Space);
    assert_eq!(app.ids, expected);
}

#[test]
fn keyboard_paging_uses_the_viewport_and_shift_extends_a_range() {
    let (_temp, mut app) = fixture(80);
    render(&mut app, 80, 24);
    let page = app.ui.page;
    plain(&mut app, KeyCode::Home);
    plain(&mut app, KeyCode::PageDown);
    assert_eq!(app.table.selected(), Some(page));
    key(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
    key(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
    assert_eq!(app.selected.len(), 3);
    plain(&mut app, KeyCode::End);
    render(&mut app, 80, 24);
    assert_eq!(app.table.selected(), Some(app.ids.len() - 1));
    assert!(app.table.offset() > 0);
    let last = app.ids.len() - 2;
    click(&mut app, Hit::Row(last));
    assert_eq!(app.focused(), Some(app.ids[last]));
}

#[test]
fn mouse_marks_ranges_scrolls_and_opens_folders() {
    let (_temp, mut app) = fixture(80);
    render(&mut app, 100, 30);
    click(&mut app, Hit::Check(1));
    assert_eq!(app.selected.len(), 1);
    let p = point(&app, Hit::Row(3));
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        p,
        KeyModifiers::SHIFT,
    );
    assert_eq!(app.selected.len(), 3);
    let p = point(&app, Hit::Row(3));
    mouse(&mut app, MouseEventKind::ScrollDown, p, KeyModifiers::NONE);
    render(&mut app, 100, 30);
    assert_eq!(app.table.offset(), 3);
    let folder = focus(&mut app, "folder");
    render(&mut app, 100, 30);
    let row = app.ids.iter().position(|id| *id == folder).unwrap();
    click(&mut app, Hit::Row(row));
    click(&mut app, Hit::Row(row));
    assert_eq!(app.folder, folder);
    assert!(app.selected.is_empty());
}

#[test]
fn mouse_confirmation_and_undo_update_rows_without_starting_a_full_scan() {
    let (_temp, mut app) = fixture(3);
    let source = app.root.join("file-000.txt");
    let destination = app.root.join("destination/file-000.txt");
    let file = focus(&mut app, "file-000.txt");
    let plan = ops::plan(
        app.index.as_ref().unwrap(),
        &[file],
        Action::Move {
            destination: app.root.join("destination").to_string_lossy().into(),
        },
        &app.state,
        0,
    )
    .unwrap();
    app.open_modal(Modal::Preview(Box::new(plan.clone()), Editor::default()));
    render(&mut app, 80, 24);
    plain(&mut app, KeyCode::Enter);
    assert!(app.modal.is_none());
    assert!(source.exists());
    app.open_modal(Modal::Preview(Box::new(plan), Editor::default()));
    render(&mut app, 80, 24);
    click(&mut app, Hit::ModalConfirm);
    settle(&mut app);
    assert!(!source.exists() && destination.exists());
    assert!(app.hidden.contains(&file));
    assert!(app.needs_rescan);
    assert!(app.scan.is_none());
    let reopened = App::new(app.root.clone(), app.state.clone());
    assert!(!reopened.ids.contains(&file));
    plain(&mut app, KeyCode::Char('u'));
    settle(&mut app);
    assert!(source.exists() && !destination.exists());
    assert!(!app.hidden.contains(&file));
    assert!(app.ids.contains(&file));
    assert!(app.scan.is_none());
}

#[test]
fn undo_after_a_rescan_refreshes_the_restored_file_and_survives_restart() {
    let (_temp, mut app) = fixture(3);
    let source = app.root.join("file-000.txt");
    let destination = app.root.join("destination/file-000.txt");
    let file = focus(&mut app, "file-000.txt");
    let plan = ops::plan(
        app.index.as_ref().unwrap(),
        &[file],
        Action::Move {
            destination: app.root.join("destination").to_string_lossy().into(),
        },
        &app.state,
        0,
    )
    .unwrap();
    app.apply(plan, "MOVE".into());
    settle(&mut app);
    app.scan();
    settle(&mut app);
    assert!(app.index.as_ref().unwrap().find_path(&source).is_none());
    assert!(!app.needs_rescan);

    plain(&mut app, KeyCode::Char('u'));
    settle(&mut app);
    assert!(source.exists() && !destination.exists());
    let restored = app.index.as_ref().unwrap().find_path(&source).unwrap();
    assert!(app.ids.contains(&restored));
    assert!(
        app.index
            .as_ref()
            .unwrap()
            .find_path(&destination)
            .is_none()
    );
    let reopened = App::new(app.root.clone(), app.state.clone());
    assert!(reopened.ids.contains(&restored));
    assert!(!reopened.needs_rescan);
}

#[test]
fn uncertain_operation_failures_keep_the_saved_index_marked_stale() {
    for automatic in [false, true] {
        let (_temp, mut app) = fixture(2);
        let error = "Journal could not be saved".into();
        app.tx
            .send(if automatic {
                Message::Auto(Err(error))
            } else {
                Message::Applied(Err(error))
            })
            .unwrap();
        app.messages();
        assert!(app.needs_rescan);
        if !automatic {
            assert!(matches!(&app.modal, Some(Modal::Notice(text)) if text.contains("Press r")));
        }
        let reopened = App::new(app.root.clone(), app.state.clone());
        assert!(reopened.needs_rescan);
    }
}

#[test]
fn uncertain_per_entry_failures_mark_the_index_stale_without_completed_paths() {
    let (_temp, mut app) = fixture(2);
    app.tx
        .send(Message::Applied(Ok(OperationUpdate {
            summary: "Operation needs reconciliation".into(),
            hidden: vec![],
            restored: None,
            reindex: false,
            uncertain: true,
            blocked: vec![ops::Blocked {
                path: app.root.join("file-000.txt").to_string_lossy().into(),
                reason: "Moved; completion journal failed".into(),
            }],
        })))
        .unwrap();
    app.messages();
    assert!(app.needs_rescan);
    assert!(app.modal.is_some());
    let reopened = App::new(app.root.clone(), app.state.clone());
    assert!(reopened.needs_rescan);
}

#[test]
fn modal_clicks_are_trapped_and_long_content_keeps_controls_visible() {
    let (_temp, mut app) = fixture(4);
    render(&mut app, 80, 24);
    let background = point(&app, Hit::View(View::Find));
    app.open_modal(Modal::Help);
    let text = render(&mut app, 48, 14);
    assert!(text.contains("Close"));
    assert!(app.ui.modal_max > 0);
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        background,
        KeyModifiers::NONE,
    );
    assert_eq!(app.view, View::Space);
    assert!(app.modal.is_some());
    plain(&mut app, KeyCode::End);
    render(&mut app, 48, 14);
    assert_eq!(app.modal_scroll, app.ui.modal_max);
    click(&mut app, Hit::ModalCancel);
    assert!(app.modal.is_none());
}

#[test]
fn bracketed_paste_is_bounded_and_query_control_keys_are_not_inserted() {
    let (_temp, mut app) = fixture(2);
    plain(&mut app, KeyCode::Char('/'));
    app.handle_event(Event::Paste("猫".repeat(2000)));
    assert!(app.query.text.len() <= 4096);
    assert!(app.query.text.is_char_boundary(app.query.cursor));
    let before = app.query.text.clone();
    key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    assert_eq!(app.query.cursor, 0);
    assert_eq!(app.query.text, before);
    key(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
    assert!(app.query.text.is_empty());
    settle(&mut app);
}

#[test]
fn rescan_keeps_the_current_folder_and_invalidates_only_old_overlay_ids() {
    let (_temp, mut app) = fixture(3);
    focus(&mut app, "folder");
    plain(&mut app, KeyCode::Enter);
    focus(&mut app, "folder/inside.txt");
    let previous = app.index.as_ref().unwrap().stamp();
    app.hidden.insert(id(&app, "file-000.txt"));
    app.needs_rescan = true;
    app.save_overlay();
    fs::write(app.root.join("another.txt"), b"new").unwrap();
    let path = index::scan(
        &app.root,
        &app.state,
        Arc::new(Mutex::new(Progress::default())),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    app.tx.send(Message::Scan(Ok(path))).unwrap();
    app.messages();
    assert_eq!(
        app.index.as_ref().unwrap().path(app.folder).unwrap(),
        app.root.join("folder")
    );
    assert_eq!(
        app.index
            .as_ref()
            .unwrap()
            .path(app.focused().unwrap())
            .unwrap(),
        app.root.join("folder/inside.txt")
    );
    assert!(app.hidden.is_empty());
    assert!(!app.needs_rescan);
    assert_ne!(app.index.as_ref().unwrap().stamp(), previous);
}

#[test]
fn background_scans_preserve_query_focus_and_do_not_turn_typing_into_commands() {
    let (_temp, mut app) = fixture(4);
    plain(&mut app, KeyCode::Char('/'));
    app.handle_event(Event::Paste("file-".into()));
    app.scan();
    settle(&mut app);
    assert_eq!(app.focus, Focus::Query);
    for letter in "qmax".chars() {
        assert!(!plain(&mut app, KeyCode::Char(letter)));
    }
    assert_eq!(app.query.text, "file-qmax");
    assert!(app.modal.is_none());
    settle(&mut app);
}

#[test]
fn editor_handles_unicode_insertion_deletion_and_horizontal_scrolling() {
    use unicode_width::UnicodeWidthStr;
    let mut input = Editor::new("héllo猫".into());
    input.key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
    input.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    input.key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
    assert_eq!(input.text, "hllo猫");
    input.insert("é");
    assert_eq!(input.text, "héllo猫");
    input.key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
    let (visible, cursor) = input.visible(4);
    assert!(visible.width() <= 4);
    assert!(cursor < 4);
    input.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    assert_eq!(input.text, "héllo");
}

#[test]
fn sorting_keeps_marks_and_the_focused_file() {
    let (_temp, mut app) = fixture(8);
    let file = focus(&mut app, "file-003.txt");
    plain(&mut app, KeyCode::Char(' '));
    let marked = app.selected.clone();
    plain(&mut app, KeyCode::Char('s'));
    assert_eq!(app.focused(), Some(file));
    assert_eq!(app.selected, marked);
}

#[test]
fn right_click_menu_is_keyboard_navigable_and_enter_activates_the_item() {
    let (_temp, mut app) = fixture(4);
    focus(&mut app, "file-000.txt");
    render(&mut app, 100, 30);
    let row = app.table.selected().unwrap();
    let point = point(&app, Hit::Row(row));
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Right),
        point,
        KeyModifiers::NONE,
    );
    assert!(matches!(app.modal, Some(Modal::Menu(0))));
    plain(&mut app, KeyCode::Down);
    assert!(matches!(app.modal, Some(Modal::Menu(1))));
    plain(&mut app, KeyCode::Up);
    plain(&mut app, KeyCode::Enter);
    assert!(matches!(app.modal, Some(Modal::Inspect)));
}

#[test]
fn releasing_outside_a_confirmation_button_cancels_the_click() {
    let (_temp, mut app) = fixture(2);
    let file = focus(&mut app, "file-000.txt");
    let plan = ops::plan(
        app.index.as_ref().unwrap(),
        &[file],
        Action::Move {
            destination: app.root.join("destination").to_string_lossy().into(),
        },
        &app.state,
        0,
    )
    .unwrap();
    app.open_modal(Modal::Preview(Box::new(plan), Editor::default()));
    render(&mut app, 80, 24);
    let confirm = point(&app, Hit::ModalConfirm);
    let cancel = point(&app, Hit::ModalCancel);
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        confirm,
        KeyModifiers::NONE,
    );
    assert!(!app.applying);
    mouse(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        cancel,
        KeyModifiers::NONE,
    );
    assert!(!app.applying);
    assert!(app.modal.is_some());
    assert!(app.root.join("file-000.txt").exists());
}

#[test]
fn help_keeps_focus_on_its_visible_close_button_and_quit_waits_for_file_changes() {
    let (_temp, mut app) = fixture(2);
    plain(&mut app, KeyCode::Char('?'));
    render(&mut app, 80, 24);
    plain(&mut app, KeyCode::Tab);
    assert_eq!(app.modal_focus, 0);
    plain(&mut app, KeyCode::Enter);
    assert!(app.modal.is_none());
    app.applying = true;
    assert!(!key(&mut app, KeyCode::Char('q'), KeyModifiers::CONTROL));
    assert!(app.quit_after_operation);
}

#[test]
fn invalid_queries_show_the_error_and_clear_previous_results() {
    let (_temp, mut app) = fixture(2);
    plain(&mut app, KeyCode::Char('/'));
    app.handle_event(Event::Paste("re:[".into()));
    settle(&mut app);
    assert!(app.ids.is_empty());
    assert!(app.search_error.is_some());
    let text = render(&mut app, 80, 24);
    assert!(text.contains("Search could not run"));
}

#[test]
fn changing_selection_cancels_a_pending_preview_without_stacking_workers() {
    let (_temp, mut app) = fixture(4);
    app.busy = true;
    app.plan_inflight = true;
    app.plan_generation = 20;
    plain(&mut app, KeyCode::Down);
    assert!(!app.busy);
    assert!(app.plan_inflight);
    assert!(!app.mutable());
    app.tx
        .send(Message::Plan(20, Err("canceled check".into())))
        .unwrap();
    app.messages();
    assert!(app.modal.is_none());
    assert!(!app.plan_inflight);
    assert!(app.mutable());
}

#[test]
fn palette_keeps_text_and_action_colors_readable_on_its_surfaces() {
    use super::theme::*;
    use ratatui::style::Color;
    fn luminance(c: Color) -> f64 {
        let Color::Rgb(r, g, b) = c else {
            panic!("RGB palette")
        };
        let linear = |v: u8| {
            let n = v as f64 / 255.;
            if n <= 0.04045 {
                n / 12.92
            } else {
                ((n + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
    }
    let contrast = |a, b| {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    };
    for surface in [BG, PANEL, SELECTED] {
        assert!(contrast(FG, surface) >= 7.);
        assert!(contrast(DIM, surface) >= 4.5);
        for action in [ACCENT, INFO, MINT, AMBER, DANGER] {
            assert!(contrast(action, surface) >= 4.5);
        }
    }
    assert!(contrast(BG, ACCENT) >= 7.);
    assert!(contrast(BG, DANGER) >= 4.5);
}

#[test]
fn an_entirely_blocked_plan_only_focuses_its_close_button() {
    let (_temp, mut app) = fixture(2);
    let file = focus(&mut app, "file-000.txt");
    let plan = ops::plan(
        app.index.as_ref().unwrap(),
        &[file],
        Action::Clean,
        &app.state,
        0,
    )
    .unwrap();
    assert!(plan.entries.is_empty());
    app.open_modal(Modal::Preview(Box::new(plan), Editor::default()));
    render(&mut app, 80, 24);
    plain(&mut app, KeyCode::Right);
    plain(&mut app, KeyCode::Tab);
    assert_eq!(app.modal_focus, 0);
    app.handle_event(Event::Paste("CLEAN".into()));
    assert_eq!(app.modal_focus, 0);
    assert!(matches!(app.modal,Some(Modal::Preview(_,ref input)) if input.text.is_empty()));
}
