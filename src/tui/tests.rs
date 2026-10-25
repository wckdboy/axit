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
    let context = crate::apps::Context {
        home: root.clone(),
        app_roots: vec![root.join("Applications")],
        desktop_roots: vec![root.join("launchers")],
        tools: Default::default(),
        scope: None,
    };
    (temp, App::new_in(root, state, View::Space, Some(context)))
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
        if !app.busy
            && !app.searching
            && !app.search_inflight
            && !app.listing
            && !app.list_inflight
            && !app.apps.inflight
            && !app.apps.loading
            && app.scan.is_none()
        {
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
                ..Default::default()
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
    app.open_modal(Modal::Preview(Box::new(plan.clone())));
    render(&mut app, 80, 24);
    plain(&mut app, KeyCode::Enter);
    assert!(app.modal.is_none());
    assert!(source.exists());
    app.open_modal(Modal::Preview(Box::new(plan)));
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
            mutated: true,
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
    let background = point(&app, Hit::Row(0));
    app.open_modal(Modal::Help);
    let text = render(&mut app, 48, 16);
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
    render(&mut app, 48, 16);
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
    app.open_modal(Modal::Preview(Box::new(plan)));
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
    app.open_modal(Modal::Preview(Box::new(plan)));
    render(&mut app, 80, 24);
    plain(&mut app, KeyCode::Right);
    plain(&mut app, KeyCode::Tab);
    assert_eq!(app.modal_focus, 0);
    app.handle_event(Event::Paste("CLEAN".into()));
    assert_eq!(app.modal_focus, 0);
    assert!(matches!(app.modal,Some(Modal::Preview(ref plan)) if plan.entries.is_empty()));
}

#[test]
fn number_navigation_and_focus_shortcuts_never_require_escape() {
    let (_temp, mut app) = fixture(4);
    for _ in 0..3 {
        for (digit, view) in [
            ('1', View::Space),
            ('2', View::Find),
            ('3', View::Clean),
            ('4', View::Activity),
            ('5', View::Apps),
        ] {
            plain(&mut app, KeyCode::Char(digit));
            assert_eq!(app.view, view);
            assert_eq!(app.focus, Focus::Nav);
            assert!(app.modal.is_none());
        }
    }
    key(&mut app, KeyCode::PageDown, KeyModifiers::CONTROL);
    assert_eq!(app.view, View::Space);
    key(
        &mut app,
        KeyCode::BackTab,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    assert_eq!(app.view, View::Apps);
    plain(&mut app, KeyCode::F(6));
    assert_eq!(app.focus, Focus::Nav);
    plain(&mut app, KeyCode::Home);
    plain(&mut app, KeyCode::Right);
    assert_eq!(app.view, View::Find);
    plain(&mut app, KeyCode::Tab);
    assert_eq!(app.focus, Focus::Query);
    for digit in "1234".chars() {
        plain(&mut app, KeyCode::Char(digit));
    }
    assert_eq!(app.query.text, "1234");
    plain(&mut app, KeyCode::F(6));
    plain(&mut app, KeyCode::Right);
    assert_eq!(app.view, View::Clean);
    settle(&mut app);
}

#[test]
fn global_section_shortcuts_cancel_every_dialog_without_mutating_files() {
    let (_temp, mut app) = fixture(2);
    let file = focus(&mut app, "file-000.txt");
    let plan = ops::plan(
        app.index.as_ref().unwrap(),
        &[file],
        Action::Trash,
        &app.state,
        0,
    )
    .unwrap();
    let dialogs = vec![
        Modal::Help,
        Modal::Inspect,
        Modal::Auto,
        Modal::Menu(0),
        Modal::Notice("test".into()),
        Modal::Destination(Editor::new("folder-1234".into())),
        Modal::Preview(Box::new(plan)),
    ];
    for modal in dialogs {
        key(&mut app, KeyCode::Char('1'), KeyModifiers::ALT);
        app.open_modal(modal);
        key(&mut app, KeyCode::Tab, KeyModifiers::CONTROL);
        assert_eq!(app.view, View::Find);
        assert!(app.modal.is_none());
        assert!(app.root.join("file-000.txt").exists());
    }
    app.open_modal(Modal::Destination(Editor::default()));
    for digit in "1234".chars() {
        plain(&mut app, KeyCode::Char(digit));
    }
    assert!(matches!(&app.modal, Some(Modal::Destination(input)) if input.text == "1234"));
    key(&mut app, KeyCode::Char('3'), KeyModifiers::ALT);
    assert_eq!(app.view, View::Clean);
    assert!(app.modal.is_none());
    settle(&mut app);
}

#[test]
fn confirmation_has_two_stops_and_mouse_tabs_can_leave_it_safely() {
    let (_temp, mut app) = fixture(2);
    let file = focus(&mut app, "file-000.txt");
    let plan = ops::plan(
        app.index.as_ref().unwrap(),
        &[file],
        Action::Trash,
        &app.state,
        0,
    )
    .unwrap();
    app.open_modal(Modal::Preview(Box::new(plan)));
    let output = render(&mut app, 100, 30);
    assert!(output.contains("Move 1 item to Trash?"));
    assert!(!output.contains("TRASH >"));
    assert_eq!(app.modal_focus, 0);
    plain(&mut app, KeyCode::Tab);
    assert_eq!(app.modal_focus, 1);
    plain(&mut app, KeyCode::Tab);
    assert_eq!(app.modal_focus, 0);
    app.handle_event(Event::Paste("TRASH".into()));
    assert_eq!(app.modal_focus, 0);
    click(&mut app, Hit::View(View::Clean));
    assert_eq!(app.view, View::Clean);
    assert!(app.modal.is_none());
    assert!(app.root.join("file-000.txt").exists());
}

#[test]
fn large_listings_coalesce_and_cannot_overwrite_a_new_section() {
    let (_temp, mut app) = fixture(4200);
    assert!(app.listing && app.list_inflight);
    key(&mut app, KeyCode::Char('3'), KeyModifiers::ALT);
    assert!(app.pending_listing.is_some());
    key(&mut app, KeyCode::Char('4'), KeyModifiers::ALT);
    assert!(app.pending_listing.is_none());
    settle(&mut app);
    assert_eq!(app.view, View::Activity);
    assert!(app.ids.is_empty());
    key(&mut app, KeyCode::Char('1'), KeyModifiers::ALT);
    settle(&mut app);
    assert_eq!(app.ids.len(), 1000);
    assert_eq!(app.folder_count, 4202);
    let expected = app.ids.clone();
    key(&mut app, KeyCode::Char('4'), KeyModifiers::ALT);
    key(&mut app, KeyCode::Char('1'), KeyModifiers::ALT);
    assert!(!app.listing && !app.list_inflight);
    assert_eq!(app.ids, expected);
}

#[test]
fn listing_cache_invalidates_removed_rows_and_their_usage_totals() {
    let (_temp, mut app) = fixture(4);
    let file = id(&app, "file-000.txt");
    let previous_count = app.folder_count;
    let previous_bytes = app.folder_bytes;
    let size = app.index.as_ref().unwrap().entry(file).unwrap().allocated;
    app.hidden.insert(file);
    app.invalidate_rows();
    app.refresh(None, 0);
    assert!(!app.ids.contains(&file));
    assert_eq!(app.folder_count, previous_count - 1);
    assert_eq!(app.folder_bytes, previous_bytes - size);
    app.hidden.remove(&file);
    app.invalidate_rows();
    app.refresh(None, 0);
    assert!(app.ids.contains(&file));
    assert_eq!(app.folder_bytes, previous_bytes);
}

#[test]
fn map_tiles_select_and_open_the_same_items_as_the_list() {
    let (_temp, mut app) = fixture(8);
    fs::write(app.root.join("folder/inside.txt"), vec![7; 65536]).unwrap();
    app.scan();
    settle(&mut app);
    let folder = id(&app, "folder");
    let output = render(&mut app, 140, 40);
    assert!(output.contains("Space map"));
    click(&mut app, Hit::Tile(folder));
    assert_eq!(app.focused(), Some(folder));
    render(&mut app, 140, 40);
    click(&mut app, Hit::Tile(folder));
    assert_eq!(app.folder, folder);
    plain(&mut app, KeyCode::Char('g'));
    assert!(!app.show_map);
    let output = render(&mut app, 140, 40);
    assert!(!output.contains("Space map"));
}

#[test]
fn leaving_an_operation_notice_does_not_claim_that_no_files_changed() {
    let (_temp, mut app) = fixture(2);
    app.status = "1 completed · 1 blocked".into();
    app.open_modal(Modal::Notice("Some paths changed before an error".into()));
    key(&mut app, KeyCode::PageDown, KeyModifiers::CONTROL);
    assert!(!app.status.contains("no files changed"));
    assert!(app.modal.is_none());
    settle(&mut app);
}

#[test]
#[ignore = "Read-only benchmark; set AXIT_BENCH_STATE to an existing metadata index"]
fn benchmark_existing_index_navigation() {
    let state =
        PathBuf::from(std::env::var_os("AXIT_BENCH_STATE").expect("AXIT_BENCH_STATE is required"));
    let index = Arc::new(Index::open(&crate::index_path(&state)).unwrap());
    let entries = index.count;
    let start = Instant::now();
    let candidates = ops::cache_candidates(&index).len();
    let cold_ms = start.elapsed().as_secs_f64() * 1000.;
    let start = Instant::now();
    for _ in 0..1000 {
        std::hint::black_box(ops::cache_candidates(&index));
    }
    let warm_us = start.elapsed().as_secs_f64() * 1000.;
    let temporary = tempfile::tempdir().unwrap();
    let mut app = App::new(index.path(0).unwrap(), temporary.path().to_path_buf());
    app.index = Some(index);
    let start = Instant::now();
    app.refresh(None, 0);
    let dispatch_us = start.elapsed().as_secs_f64() * 1_000_000.;
    let mut max_key_us = 0f64;
    for digit in "341341341".chars() {
        let start = Instant::now();
        key(&mut app, KeyCode::Char(digit), KeyModifiers::ALT);
        max_key_us = max_key_us.max(start.elapsed().as_secs_f64() * 1_000_000.);
    }
    for _ in 0..1000 {
        app.messages();
        if !app.listing && !app.list_inflight {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(!app.listing && !app.list_inflight);
    println!(
        "{}",
        serde_json::json!({"entries":entries,"cache_candidates":candidates,"candidate_cold_ms":cold_ms,"candidate_warm_us":warm_us,"cold_listing_dispatch_us":dispatch_us,"max_navigation_key_us":max_key_us})
    );
}

#[test]
fn rapid_query_changes_coalesce_to_the_latest_search() {
    let (_temp, mut app) = fixture(100);
    plain(&mut app, KeyCode::Char('/'));
    for n in 0..20 {
        app.query = Editor::new(format!("file-{n:03}.txt"));
        app.query_changed();
    }
    assert!(app.search_inflight && app.search_pending);
    settle(&mut app);
    assert_eq!(app.ids, vec![id(&app, "file-019.txt")]);
    assert_eq!(app.matched, 1);
    assert!(!app.search_pending && !app.search_inflight);
}

#[test]
fn arrow_keys_move_between_content_and_navigation_without_clearing_input_or_marks() {
    let (_temp, mut app) = fixture(4);
    let first = app.focused().unwrap();
    app.selected.insert(first);
    plain(&mut app, KeyCode::Up);
    assert_eq!(app.focus, Focus::Nav);
    assert!(app.selected.contains(&first));
    plain(&mut app, KeyCode::Up);
    assert_eq!(app.focus, Focus::Nav);
    plain(&mut app, KeyCode::Right);
    assert_eq!(app.view, View::Find);
    plain(&mut app, KeyCode::Down);
    assert_eq!(app.focus, Focus::Query);
    plain(&mut app, KeyCode::Char('2'));
    plain(&mut app, KeyCode::Up);
    assert_eq!(app.focus, Focus::Nav);
    assert_eq!(app.query.text, "2");
    plain(&mut app, KeyCode::Down);
    assert_eq!(app.focus, Focus::Query);
    key(&mut app, KeyCode::Up, KeyModifiers::CONTROL);
    assert_eq!(app.focus, Focus::Nav);
    plain(&mut app, KeyCode::Left);
    plain(&mut app, KeyCode::Down);
    plain(&mut app, KeyCode::End);
    let last = app.table.selected().unwrap();
    plain(&mut app, KeyCode::Up);
    assert_eq!(app.focus, Focus::List);
    assert_eq!(app.table.selected(), Some(last - 1));
    plain(&mut app, KeyCode::Home);
    key(&mut app, KeyCode::Up, KeyModifiers::SHIFT);
    assert_eq!(app.focus, Focus::List);
    plain(&mut app, KeyCode::Up);
    assert_eq!(app.focus, Focus::Nav);
    app.focus = Focus::Actions;
    plain(&mut app, KeyCode::Up);
    assert_eq!(app.focus, Focus::List);
    app.focus = Focus::Inspect;
    app.inspect_scroll = 0;
    plain(&mut app, KeyCode::Up);
    assert_eq!(app.focus, Focus::Nav);
    key(&mut app, KeyCode::Char('4'), KeyModifiers::ALT);
    app.activity_scroll = 7;
    key(&mut app, KeyCode::Up, KeyModifiers::CONTROL);
    assert_eq!(app.activity_scroll, 7);
    plain(&mut app, KeyCode::Down);
    assert_eq!(app.activity_scroll, 7);
    app.activity_scroll = 1;
    plain(&mut app, KeyCode::Up);
    assert_eq!(app.activity_scroll, 0);
    assert_eq!(app.focus, Focus::List);
    plain(&mut app, KeyCode::Up);
    assert_eq!(app.focus, Focus::Nav);
    app.open_modal(Modal::Destination(Editor::new("keep this".into())));
    key(&mut app, KeyCode::Up, KeyModifiers::CONTROL);
    assert!(app.modal.is_none());
    assert_eq!(app.focus, Focus::Nav);
    settle(&mut app);
}

#[test]
fn minimum_find_view_shows_a_result_and_accessible_actions() {
    let (_temp, mut app) = fixture(2);
    plain(&mut app, KeyCode::Char('/'));
    app.query = Editor::new("file-000".into());
    app.query_changed();
    settle(&mut app);
    let output = render(&mut app, 48, 16);
    assert!(output.contains("file-000.txt"), "{output}");
    assert!(app.ui.hits.iter().any(|(_, h)| *h == Hit::Row(0)));
    assert!(
        app.ui
            .hits
            .iter()
            .any(|(_, h)| *h == Hit::Command(Command::Trash))
    );
    assert!(render(&mut app, 48, 14).contains("Expand to 48 × 16"));
}

#[test]
fn apps_view_filters_reviews_data_and_keeps_navigation_and_confirmation_accessible() {
    let (_temp, mut app) = fixture(1);
    let context = app.apps.context.as_ref().unwrap().clone();
    let path = context.app_roots[0].join("Alpha.app");
    fs::create_dir_all(path.join("Contents")).unwrap();
    fs::write(path.join("Contents/Info.plist"),br#"<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>org.wckd.Alpha</string><key>CFBundleName</key><string>Alpha</string></dict></plist>"#).unwrap();
    let cache = context.home.join("Library/Caches/org.wckd.Alpha");
    fs::create_dir_all(&cache).unwrap();
    let personal = context
        .home
        .join("Library/Application Support/org.wckd.Alpha");
    fs::create_dir_all(&personal).unwrap();
    plain(&mut app, KeyCode::Char('5'));
    settle(&mut app);
    assert_eq!(app.view, View::Apps);
    plain(&mut app, KeyCode::Down);
    app.handle_event(Event::Paste("Alpha".into()));
    assert_eq!(app.apps.filter.text, "Alpha");
    plain(&mut app, KeyCode::Enter);
    assert!(app.app_target().is_some());
    key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    assert!(
        app.selected.is_empty(),
        "Apps has a single focused app, not file-index marks"
    );
    let screen = render(&mut app, 48, 16);
    assert!(screen.contains("5 Apps") && screen.contains("Alpha"));
    click(&mut app, Hit::Command(Command::Uninstall));
    settle(&mut app);
    assert!(matches!(app.modal, Some(Modal::AppFiles(_))));
    if let Some(Modal::AppFiles(review)) = &app.modal {
        assert!(
            review
                .rows
                .iter()
                .enumerate()
                .any(|(i, f)| f.path == cache && review.selected.contains(&i))
        );
        assert!(
            review
                .rows
                .iter()
                .enumerate()
                .all(|(i, f)| f.path != personal || !review.selected.contains(&i))
        );
    }
    render(&mut app, 80, 24);
    assert_eq!(app.modal_focus, 2);
    plain(&mut app, KeyCode::Esc);
    assert!(app.modal.is_none() && path.exists() && personal.exists());
    plain(&mut app, KeyCode::Delete);
    settle(&mut app);
    render(&mut app, 80, 24);
    plain(&mut app, KeyCode::Enter);
    settle(&mut app);
    assert!(matches!(app.modal, Some(Modal::Preview(_))));
    assert_eq!(app.modal_focus, 0);
    key(&mut app, KeyCode::Up, KeyModifiers::CONTROL);
    assert_eq!(app.focus, Focus::Nav);
    assert!(app.modal.is_none() && path.exists() && personal.exists());
}

#[test]
fn result_windows_reach_every_item_preserve_identity_and_keep_global_map_targets() {
    let (_temp, mut app) = fixture(10057);
    settle(&mut app);
    while app.sort != Sort::Name {
        plain(&mut app, KeyCode::Char('s'));
        settle(&mut app);
    }
    // Sorting preserves the focused item; filesystem directory allocation can
    // make that item the last Name result. Explicitly request the first window.
    plain(&mut app, KeyCode::Home);
    settle(&mut app);
    assert_eq!(app.pages.nav.offset, 0);
    app.focus = Focus::List;
    let first = app.ids.clone();
    let marked = first[20];
    app.select_row(20);
    plain(&mut app, KeyCode::Char(' '));
    assert_eq!(app.chosen(), vec![marked]);
    let mut all = first.clone();
    while app.pages.nav.next_cursor.is_some() {
        plain(&mut app, KeyCode::Char(']'));
        settle(&mut app);
        assert!(app.selected.contains(&marked));
        all.extend(&app.ids);
    }
    assert_eq!(all.len(), 10059);
    assert_eq!(all.iter().collect::<HashSet<_>>().len(), 10059);
    assert_eq!(
        app.chosen(),
        vec![marked],
        "Marks remain bound to IDs, even off the visible window"
    );
    plain(&mut app, KeyCode::Home);
    settle(&mut app);
    assert_eq!(app.ids, first);
    assert_eq!(app.pages.nav.offset, 0);
    plain(&mut app, KeyCode::End);
    settle(&mut app);
    assert_eq!(app.pages.nav.offset, 9059);
    assert_eq!(app.table.selected(), Some(999));
    plain(&mut app, KeyCode::Home);
    settle(&mut app);
    render(&mut app, 140, 38);
    click(&mut app, Hit::Command(Command::NextPage));
    settle(&mut app);
    assert_eq!(app.pages.nav.offset, 1000);
    let second = app.ids.clone();
    let second_focus = app.focused();
    while app.sort != Sort::Modified {
        plain(&mut app, KeyCode::Char('s'));
        settle(&mut app);
    }
    settle(&mut app);
    assert!(app.selected.contains(&marked));
    plain(&mut app, KeyCode::Char('2'));
    settle(&mut app);
    key(&mut app, KeyCode::Left, KeyModifiers::ALT);
    settle(&mut app);
    assert_eq!(app.focused(), second_focus);
    assert_eq!(app.sort, Sort::Modified);
    assert!(app.selected.is_empty(), "Navigation clears marks");
    assert_eq!(second.len(), 1000);
    plain(&mut app, KeyCode::Home);
    settle(&mut app);
    key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    assert!(app.selected.is_empty());
    assert!(app.status.contains("128"));
    plain(&mut app, KeyCode::Char('2'));
    plain(&mut app, KeyCode::Down);
    app.handle_event(Event::Paste("ext:txt".into()));
    settle(&mut app);
    plain(&mut app, KeyCode::Enter);
    assert!(app.matched > 10000);
    plain(&mut app, KeyCode::End);
    settle(&mut app);
    assert_eq!(app.pages.nav.offset + app.ids.len() as u64, app.matched);
    let screen = render(&mut app, 48, 16);
    assert!(screen.contains("Prev") && screen.contains("Next"));
    plain(&mut app, KeyCode::Home);
    settle(&mut app);
    plain(&mut app, KeyCode::Up);
    assert_eq!(app.focus, Focus::Nav);
}

#[test]
fn a_map_target_outside_the_current_window_can_be_selected_and_inspected() {
    let (_temp, mut app) = fixture(4200);
    fs::write(app.root.join("zz-largest.bin"), vec![7; 65536]).unwrap();
    app.scan();
    settle(&mut app);
    while app.sort != Sort::Name {
        plain(&mut app, KeyCode::Char('s'));
        settle(&mut app);
    }
    // Sorting preserves the focused item; filesystem directory allocation can
    // make that item the last Name result. Explicitly request the first window.
    plain(&mut app, KeyCode::Home);
    settle(&mut app);
    assert_eq!(app.pages.nav.offset, 0);
    let target = id(&app, "zz-largest.bin");
    assert!(!app.ids.contains(&target));
    render(&mut app, 140, 40);
    let p = point(&app, Hit::Tile(target));
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Right),
        p,
        KeyModifiers::NONE,
    );
    settle(&mut app);
    assert!(matches!(app.modal, Some(Modal::Menu(_))));
    plain(&mut app, KeyCode::Esc);
    assert_eq!(app.focused(), Some(target));
    assert!(app.pages.nav.offset > 0);
    render(&mut app, 140, 40);
    click(&mut app, Hit::Tile(target));
    click(&mut app, Hit::Tile(target));
    settle(&mut app);
    assert!(matches!(app.modal, Some(Modal::Inspect)));
}

#[test]
fn mouse_wheel_crosses_windows_and_offscreen_marks_are_reviewed_by_identity() {
    let (_temp, mut app) = fixture(2100);
    settle(&mut app);
    app.focus = Focus::List;
    let first = app.ids[0];
    plain(&mut app, KeyCode::Char(' '));
    render(&mut app, 100, 30);
    app.select_row(999);
    render(&mut app, 100, 30);
    let p = point(&app, Hit::Row(999));
    mouse(&mut app, MouseEventKind::ScrollDown, p, KeyModifiers::NONE);
    settle(&mut app);
    assert_eq!(app.pages.nav.offset, 1000);
    let second = app.ids[0];
    plain(&mut app, KeyCode::Char(' '));
    assert_eq!(app.selected, HashSet::from([first, second]));
    plain(&mut app, KeyCode::Delete);
    settle(&mut app);
    let Some(Modal::Preview(plan)) = &app.modal else {
        panic!("Expected review");
    };
    let paths: HashSet<_> = plan.entries.iter().map(|e| e.path.clone()).collect();
    assert_eq!(
        paths,
        HashSet::from([
            crate::text(&app.index.as_ref().unwrap().path(first).unwrap()),
            crate::text(&app.index.as_ref().unwrap().path(second).unwrap()),
        ])
    );
    assert_eq!(app.modal_focus, 0);
    plain(&mut app, KeyCode::Esc);
    assert!(app.root.read_dir().unwrap().count() >= 2100);
    render(&mut app, 100, 30);
    let p = point(&app, Hit::Row(0));
    mouse(&mut app, MouseEventKind::ScrollUp, p, KeyModifiers::NONE);
    settle(&mut app);
    assert_eq!(app.pages.nav.offset, 0);
    assert_eq!(app.table.selected(), Some(999));
}

#[test]
fn window_requests_coalesce_and_malformed_cursors_can_restart_without_stale_results() {
    let (_temp, mut app) = fixture(4200);
    settle(&mut app);
    app.focus = Focus::List;
    plain(&mut app, KeyCode::Char(']'));
    plain(&mut app, KeyCode::Home);
    settle(&mut app);
    assert_eq!(app.pages.nav.offset, 0);
    assert!(!app.list_inflight && app.pending_listing.is_none());
    app.change_page(Some("invalid".into()), pages::Landing::First);
    settle(&mut app);
    assert!(app.ids.is_empty() && app.search_error.is_some());
    let screen = render(&mut app, 100, 30);
    assert!(screen.contains("Page could not load"));
    assert!(!screen.contains("no indexed children"));
    plain(&mut app, KeyCode::Home);
    settle(&mut app);
    assert_eq!(app.ids.len(), 1000);
    plain(&mut app, KeyCode::Char('/'));
    app.handle_event(Event::Paste("ext:txt".into()));
    settle(&mut app);
    plain(&mut app, KeyCode::Enter);
    plain(&mut app, KeyCode::Char(']'));
    app.query = Editor::new("file-123.txt".into());
    app.query_changed();
    settle(&mut app);
    assert_eq!(app.ids, vec![id(&app, "file-123.txt")]);
    assert_eq!(app.pages.nav.offset, 0);
    assert!(!app.search_inflight && !app.search_pending);
}

#[test]
fn operation_overlay_changes_invalidate_history_tokens_and_restore_the_saved_file() {
    let (_temp, mut app) = fixture(4200);
    settle(&mut app);
    app.focus = Focus::List;
    plain(&mut app, KeyCode::Char(']'));
    settle(&mut app);
    let saved = app.focused();
    assert!(app.pages.cursor.is_some());
    plain(&mut app, KeyCode::Char('2'));
    settle(&mut app);
    assert!(app.back.last().unwrap().page_cursor.is_some());
    let removed = id(&app, "file-000.txt");
    assert_ne!(saved, Some(removed));
    app.tx
        .send(Message::Applied(Ok(OperationUpdate {
            mutated: true,
            summary: "Fixture file removed".into(),
            hidden: vec![removed],
            restored: None,
            reindex: false,
            uncertain: false,
            blocked: vec![],
        })))
        .unwrap();
    settle(&mut app);
    assert!(
        app.back
            .iter()
            .all(|location| location.page_cursor.is_none())
    );
    key(&mut app, KeyCode::Left, KeyModifiers::ALT);
    settle(&mut app);
    assert_eq!(app.view, View::Space);
    assert_eq!(app.focused(), saved);
    assert!(app.search_error.is_none());
    assert!(!app.ids.contains(&removed));
}

#[test]
fn dragging_past_a_window_boundary_preserves_marks_and_drops_old_row_anchors() {
    let (_temp, mut app) = fixture(1057);
    settle(&mut app);
    app.select_row(999);
    render(&mut app, 100, 30);
    let marked = app.ids[999];
    let p = point(&app, Hit::Row(999));
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        p,
        KeyModifiers::CONTROL,
    );
    assert_eq!(app.drag_anchor, Some(999));
    let outside = (p.0, app.ui.list.bottom());
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        outside,
        KeyModifiers::NONE,
    );
    settle(&mut app);
    assert_eq!(app.pages.nav.offset, 1000);
    assert_eq!(app.ids.len(), 59);
    assert!(app.drag_anchor.is_none() && app.anchor.is_none());
    assert_eq!(app.selected, HashSet::from([marked]));
    render(&mut app, 100, 30);
    let p = point(&app, Hit::Row(1));
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        p,
        KeyModifiers::NONE,
    );
    assert_eq!(app.selected, HashSet::from([marked]));
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        p,
        KeyModifiers::CONTROL,
    );
    assert_eq!(app.drag_anchor, Some(1));
    plain(&mut app, KeyCode::Char('s'));
    settle(&mut app);
    assert!(app.drag_anchor.is_none() && app.anchor.is_none());
    assert!(app.selected.contains(&marked));
}

#[test]
fn corrupt_incremental_index_requires_explicit_repair_and_preserves_scan_scope() {
    let (_temp, app) = fixture(3);
    let (root, state) = (app.root.clone(), app.state.clone());
    drop(app);
    let delta = crate::index_path(&state).with_extension("delta");
    fs::write(&delta, b"broken").unwrap();
    let mut app = App::new(root.clone(), state.clone());
    assert!(app.index.is_none());
    assert!(app.status.contains("Index unavailable"));
    app.scan_if_missing();
    plain(&mut app, KeyCode::Char('2'));
    plain(&mut app, KeyCode::Char('3'));
    app.restore_location(app.location());
    assert!(app.scan.is_none());
    assert_eq!(fs::read(&delta).unwrap(), b"broken");
    plain(&mut app, KeyCode::Char('1'));
    plain(&mut app, KeyCode::Char('r'));
    settle(&mut app);
    assert_eq!(app.index.as_ref().unwrap().path(0).unwrap(), root);
    assert!(app.index_error.is_none());
    assert!(!delta.exists());
}
