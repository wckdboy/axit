use std::{
    collections::{BTreeMap, HashSet},
    fs,
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, PermissionsExt, symlink},
    },
    path::{Path, PathBuf},
    sync::{Arc, Mutex, atomic::AtomicBool},
};
use wckd_axit::{
    index::{self, Index, Progress},
    page::Request,
    query::{self, Query, Sort},
};
fn scan(root: &Path, state: &Path) -> Index {
    Index::open(
        &index::scan(
            root,
            state,
            Arc::new(Mutex::new(Progress::default())),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap(),
    )
    .unwrap()
}
fn refresh(root: &Path, state: &Path, paths: &[&str]) -> index::RefreshReport {
    index::refresh(
        root,
        state,
        &paths.iter().map(PathBuf::from).collect::<Vec<_>>(),
        Arc::new(Mutex::new(Progress::default())),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap()
}
fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("files");
    let state = t.path().join("state");
    fs::create_dir_all(root.join("a/nested")).unwrap();
    fs::create_dir_all(root.join("b")).unwrap();
    fs::create_dir(&state).unwrap();
    fs::write(root.join("a/one.txt"), vec![1; 8192]).unwrap();
    fs::write(root.join("a/nested/two.txt"), b"two").unwrap();
    fs::write(root.join("b/keep.txt"), b"keep").unwrap();
    (
        t,
        root.canonicalize().unwrap(),
        state.canonicalize().unwrap(),
    )
}
fn stamp(path: &Path) -> (u64, u64, u64, i64, i64) {
    let md = fs::metadata(path).unwrap();
    (md.dev(), md.ino(), md.len(), md.mtime(), md.mtime_nsec())
}
type MetadataRow = (u32, u32, u64, u64, i64, u64, u64);
fn rows(index: &Index) -> BTreeMap<PathBuf, MetadataRow> {
    (0..index.count)
        .filter_map(|id| index.entry(id))
        .map(|e| {
            (
                index.path(e.id).unwrap(),
                (
                    e.flags,
                    e.uid,
                    e.logical,
                    e.allocated,
                    e.modified,
                    e.device,
                    e.inode,
                ),
            )
        })
        .collect()
}
fn assert_matches_scan(root: &Path, state: &Path) {
    let parent = state.parent().unwrap();
    let reference = tempfile::tempdir_in(parent).unwrap();
    let expected = scan(root, reference.path());
    let actual = Index::open(&wckd_axit::index_path(state)).unwrap();
    assert_eq!(rows(&actual), rows(&expected));
    assert_eq!(actual.live_count(), expected.live_count());
}
#[test]
fn targeted_create_write_delete_and_type_changes_match_a_clean_scan_without_rewriting_the_base() {
    let (_t, root, state) = fixture();
    let old = scan(&root, &state);
    let base = wckd_axit::index_path(&state);
    let before = stamp(&base);
    let original = rows(&old);
    fs::write(root.join("a/one.txt"), vec![4; 32768]).unwrap();
    fs::remove_file(root.join("a/nested/two.txt")).unwrap();
    fs::create_dir_all(root.join("a/new/deep")).unwrap();
    fs::write(root.join("a/new/deep/new.txt"), b"new").unwrap();
    let report = refresh(&root, &state, &["a", "a/new/deep/new.txt"]);
    assert!(report.published);
    assert_eq!(report.scopes.len(), 1);
    assert_eq!(report.removed, 1);
    assert_eq!(report.added, 3);
    assert!(report.delta_bytes < 4096);
    assert_eq!(stamp(&base), before);
    assert_eq!(rows(&old), original);
    assert_matches_scan(&root, &state);
    let current = Index::open(&base).unwrap();
    let id = current.find_path(&root.join("a/one.txt")).unwrap();
    assert_eq!(Some(id), old.find_path(&root.join("a/one.txt")));
    fs::remove_file(root.join("a/one.txt")).unwrap();
    fs::create_dir(root.join("a/one.txt")).unwrap();
    fs::write(root.join("a/one.txt/inside"), b"inside").unwrap();
    refresh(&root, &state, &["a/one.txt"]);
    assert_matches_scan(&root, &state);
    fs::remove_dir_all(root.join("a/new")).unwrap();
    fs::write(root.join("a/new"), b"now a file").unwrap();
    refresh(&root, &state, &["a/new"]);
    assert_matches_scan(&root, &state);
    assert_eq!(stamp(&base), before);
}
#[test]
fn no_op_refresh_does_not_publish_and_changed_back_metadata_is_not_lost() {
    let (_t, root, state) = fixture();
    scan(&root, &state);
    let base = wckd_axit::index_path(&state);
    let before = stamp(&base);
    let first = refresh(&root, &state, &["a"]);
    assert!(!first.published);
    assert!(!base.with_extension("delta").exists());
    assert_eq!(stamp(&base), before);
    fs::write(root.join("a/one.txt"), vec![2; 16384]).unwrap();
    refresh(&root, &state, &["a/one.txt"]);
    let snapshot = fs::read(base.with_extension("delta")).unwrap();
    let noop = refresh(&root, &state, &["a/one.txt"]);
    assert!(!noop.published);
    assert_eq!(snapshot, fs::read(base.with_extension("delta")).unwrap());
    fs::write(root.join("a/one.txt"), vec![3; 8192]).unwrap();
    let back = refresh(&root, &state, &["a/one.txt"]);
    assert!(back.published);
    assert_matches_scan(&root, &state);
}
#[test]
fn subtree_moves_and_parent_allocations_reconcile_in_one_batch() {
    let (_t, root, state) = fixture();
    scan(&root, &state);
    fs::rename(root.join("a/nested"), root.join("b/moved")).unwrap();
    let report = refresh(&root, &state, &["a/nested", "b/moved/two.txt"]);
    assert_eq!(report.removed, 2);
    assert_eq!(report.added, 2);
    assert_matches_scan(&root, &state);
    fs::remove_dir_all(root.join("a")).unwrap();
    refresh(&root, &state, &["a"]);
    assert_matches_scan(&root, &state);
}
#[test]
fn hardlink_metadata_and_ownership_transfer_keep_physical_bytes_counted_once() {
    let (_t, root, state) = fixture();
    fs::hard_link(root.join("a/one.txt"), root.join("b/alias.txt")).unwrap();
    scan(&root, &state);
    fs::write(root.join("b/alias.txt"), vec![7; 65536]).unwrap();
    refresh(&root, &state, &["b/alias.txt"]);
    let index = Index::open(&wckd_axit::index_path(&state)).unwrap();
    let ids: Vec<_> = (0..index.count)
        .filter_map(|id| index.entry(id))
        .filter(|e| e.inode == fs::metadata(root.join("a/one.txt")).unwrap().ino() && !e.is_dir())
        .collect();
    assert_eq!(ids.len(), 2);
    assert!(ids.iter().all(|e| e.logical == 65536));
    assert_eq!(
        ids.iter().map(|e| e.allocated).sum::<u64>(),
        fs::metadata(root.join("a/one.txt")).unwrap().blocks() * 512
    );
    assert_eq!(
        ids.iter().filter(|e| e.flags & index::ALIAS != 0).count(),
        1
    );
    let owner = ids.iter().find(|e| e.flags & index::ALIAS == 0).unwrap().id;
    let owner_path = index.path(owner).unwrap();
    fs::remove_file(&owner_path).unwrap();
    index::refresh(
        &root,
        &state,
        &[owner_path],
        Arc::new(Mutex::new(Progress::default())),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    let current = Index::open(&wckd_axit::index_path(&state)).unwrap();
    let left: Vec<_> = (0..current.count)
        .filter_map(|id| current.entry(id))
        .filter(|e| ids.iter().any(|old| e.inode == old.inode) && !e.is_dir())
        .collect();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].flags & index::ALIAS, 0);
    assert_eq!(left[0].allocated, 65536);
    assert_matches_scan(&root, &state);
}
#[test]
fn refresh_publishes_a_new_cursor_generation_while_open_old_readers_remain_valid() {
    let (_t, root, state) = fixture();
    let old = scan(&root, &state);
    let q = Query::parse("type:file").unwrap();
    let hidden = HashSet::new();
    let first = query::search_page(
        &old,
        &q,
        Sort::Name,
        Request {
            limit: 1,
            cursor: None,
        },
        &hidden,
        None,
    )
    .unwrap();
    let token = first.paging.next_cursor.unwrap();
    fs::write(root.join("a/extra.txt"), b"extra").unwrap();
    refresh(&root, &state, &["a/extra.txt"]);
    let new = Index::open(&wckd_axit::index_path(&state)).unwrap();
    assert_ne!(new.stamp(), old.stamp());
    assert!(
        query::search_page(
            &new,
            &q,
            Sort::Name,
            Request {
                limit: 1,
                cursor: Some(&token)
            },
            &hidden,
            None
        )
        .is_err()
    );
    assert!(
        query::search_page(
            &old,
            &q,
            Sort::Name,
            Request {
                limit: 1,
                cursor: Some(&token)
            },
            &hidden,
            None
        )
        .is_ok()
    );
    assert!(old.find_path(&root.join("a/extra.txt")).is_none());
    assert!(new.find_path(&root.join("a/extra.txt")).is_some());
}
#[test]
fn cancellation_scope_and_symlink_failures_preserve_the_last_complete_snapshot() {
    let (_t, root, state) = fixture();
    scan(&root, &state);
    fs::write(root.join("a/extra"), b"extra").unwrap();
    refresh(&root, &state, &["a"]);
    let file = wckd_axit::index_path(&state).with_extension("delta");
    let before = fs::read(&file).unwrap();
    for paths in [
        vec![PathBuf::from("../escape")],
        vec![root.parent().unwrap().join("outside")],
        vec![state.clone()],
        vec![PathBuf::from("a"); 65],
    ] {
        assert!(
            index::refresh(
                &root,
                &state,
                &paths,
                Arc::new(Mutex::new(Progress::default())),
                Arc::new(AtomicBool::new(false))
            )
            .is_err()
        );
        assert_eq!(fs::read(&file).unwrap(), before);
    }
    assert!(
        index::refresh(
            &root,
            &state,
            &[root.join("a")],
            Arc::new(Mutex::new(Progress::default())),
            Arc::new(AtomicBool::new(true))
        )
        .is_err()
    );
    assert_eq!(fs::read(&file).unwrap(), before);
    let outside = root.parent().unwrap().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret"), b"secret").unwrap();
    symlink(&outside, root.join("escape")).unwrap();
    refresh(&root, &state, &["escape"]);
    let index = Index::open(&wckd_axit::index_path(&state)).unwrap();
    assert!(
        index
            .entry(index.find_path(&root.join("escape")).unwrap())
            .unwrap()
            .is_link()
    );
    assert!(index.find_path(&root.join("escape/secret")).is_none());
    let saved = fs::read(&file).unwrap();
    assert!(
        index::refresh(
            &root,
            &state,
            &[root.join("escape/secret")],
            Arc::new(Mutex::new(Progress::default())),
            Arc::new(AtomicBool::new(false))
        )
        .is_ok()
    );
    assert_eq!(fs::read(&file).unwrap(), saved);
}
#[test]
fn metadata_refresh_handles_unreadable_file_contents_and_raw_names() {
    let (_t, root, state) = fixture();
    scan(&root, &state);
    let name = if cfg!(target_os = "macos") {
        b"odd-\xce\xbb\nfile".to_vec()
    } else {
        b"odd-\xff\nfile".to_vec()
    };
    let path = root.join(std::ffi::OsStr::from_bytes(&name));
    fs::write(&path, b"private contents").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    let report = refresh(&root, &state, &["a", "."]);
    assert!(report.published);
    assert_eq!(report.scopes, vec![wckd_axit::text(&root)]);
    let index = Index::open(&wckd_axit::index_path(&state)).unwrap();
    let entry = index.entry(index.find_path(&path).unwrap()).unwrap();
    assert_eq!(index.name(entry), name);
    assert_eq!(entry.logical, 16);
    let delta = fs::read(wckd_axit::index_path(&state).with_extension("delta")).unwrap();
    assert!(!delta.windows(16).any(|bytes| bytes == b"private contents"));
    assert_matches_scan(&root, &state);
}

#[test]
fn full_scan_compacts_and_retires_the_delta_without_invalidating_open_readers() {
    let (_t, root, state) = fixture();
    scan(&root, &state);
    fs::remove_dir_all(root.join("a/nested")).unwrap();
    fs::write(root.join("new.txt"), b"new").unwrap();
    refresh(&root, &state, &["a/nested", "new.txt"]);
    let path = wckd_axit::index_path(&state);
    let old = Index::open(&path).unwrap();
    let expected = rows(&old);
    assert!(old.last_refresh().is_some());
    assert!(old.live_count() < old.count);
    let compacted = scan(&root, &state);
    assert_eq!(rows(&compacted), expected);
    assert_eq!(rows(&old), expected);
    assert_eq!(compacted.count, compacted.live_count());
    assert!(compacted.last_refresh().is_none());
    assert!(!path.with_extension("delta").exists());
}

#[test]
fn full_scan_and_refresh_share_one_lock_and_failed_scans_preserve_the_delta() {
    let (_t, root, state) = fixture();
    scan(&root, &state);
    fs::write(root.join("new.txt"), b"new").unwrap();
    refresh(&root, &state, &["new.txt"]);
    let base = wckd_axit::index_path(&state);
    let before = (
        fs::read(&base).unwrap(),
        fs::read(base.with_extension("delta")).unwrap(),
    );
    let lock = fs::OpenOptions::new()
        .write(true)
        .open(state.join("scan.lock"))
        .unwrap();
    fs2::FileExt::lock_exclusive(&lock).unwrap();
    let progress = Arc::new(Mutex::new(Progress::default()));
    let cancel = Arc::new(AtomicBool::new(false));
    assert!(
        index::refresh(
            &root,
            &state,
            &[PathBuf::from("a")],
            progress.clone(),
            cancel.clone()
        )
        .is_err()
    );
    assert!(index::scan(&root, &state, progress.clone(), cancel).is_err());
    fs2::FileExt::unlock(&lock).unwrap();
    assert!(index::scan(&root, &state, progress, Arc::new(AtomicBool::new(true))).is_err());
    assert_eq!(fs::read(&base).unwrap(), before.0);
    assert_eq!(fs::read(base.with_extension("delta")).unwrap(), before.1);
}

#[test]
fn state_inside_the_root_is_excluded_from_full_and_incremental_scans() {
    let (_t, root, _state) = fixture();
    let state = root.join("private-state");
    fs::create_dir(&state).unwrap();
    fs::write(state.join("secret"), b"private").unwrap();
    scan(&root, &state);
    fs::write(root.join("new"), b"new").unwrap();
    refresh(&root, &state, &["."]);
    let current = Index::open(&wckd_axit::index_path(&state)).unwrap();
    assert!(current.find_path(&state).is_none());
    assert!(current.find_path(&state.join("secret")).is_none());
    assert!(current.find_path(&root.join("new")).is_some());
    let old = current.stamp();
    assert!(
        index::refresh(
            &root,
            &state,
            std::slice::from_ref(&state),
            Arc::new(Mutex::new(Progress::default())),
            Arc::new(AtomicBool::new(false))
        )
        .is_err()
    );
    assert_eq!(
        Index::open(&wckd_axit::index_path(&state)).unwrap().stamp(),
        old
    );
}

#[test]
fn damaged_sidecars_are_rejected_instead_of_silently_hiding_published_updates() {
    let (_t, root, state) = fixture();
    scan(&root, &state);
    fs::write(root.join("new.txt"), b"new").unwrap();
    refresh(&root, &state, &["new.txt"]);
    let base = wckd_axit::index_path(&state);
    let delta = base.with_extension("delta");
    let valid = fs::read(&delta).unwrap();
    let records = u32::from_le_bytes(valid[68..72].try_into().unwrap()) as usize;
    let root_row = (0..records)
        .map(|n| 128 + n * 72)
        .find(|n| valid[*n..*n + 4] == [0; 4])
        .unwrap();
    let new_id = Index::open(&base)
        .unwrap()
        .find_path(&root.join("new.txt"))
        .unwrap();
    let new_row = (0..records)
        .map(|n| 128 + n * 72)
        .find(|n| valid[*n..*n + 4] == new_id.to_le_bytes())
        .unwrap();
    let mut cases = Vec::<(&str, Vec<u8>)>::new();
    for length in [0, 8, 127, valid.len() - 1] {
        cases.push(("truncation", valid[..length].to_vec()));
    }
    for (label, at, value) in [
        ("magic", 0, 0),
        ("version", 8, 2),
        ("record width", 12, 64),
        ("base count", 56, 0),
        ("live count", 64, 0),
        ("record kind", new_row + 4, 2),
        ("root deletion", root_row + 4, 1),
        ("root type", root_row + 12, 0),
        ("unknown flags", new_row + 12, 128),
        ("directory symlink", new_row + 12, 3),
    ] {
        let mut bytes = valid.clone();
        bytes[at] = value;
        cases.push((label, bytes));
    }
    for (label, at, value) in [
        ("ID budget", 60, new_id + 131_073),
        ("record budget", 68, 131_073),
        ("parent cycle", new_row + 8, new_id),
        ("empty name", new_row + 24, 0),
        ("name length", new_row + 24, u32::MAX),
        ("missing record", new_row, u32::MAX),
    ] {
        let mut bytes = valid.clone();
        bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
        cases.push((label, bytes));
    }
    let mut bytes = valid.clone();
    bytes[new_row + 16..new_row + 24].copy_from_slice(&u64::MAX.to_le_bytes());
    cases.push(("name overflow", bytes));
    let mut bytes = valid.clone();
    bytes[root_row + 64..root_row + 72].copy_from_slice(&0u64.to_le_bytes());
    cases.push(("root identity", bytes));
    let mut bytes = valid.clone();
    bytes[128 + records * 72] = 0;
    cases.push(("NUL name", bytes));
    let mut bytes = valid.clone();
    bytes[128 + records * 72] = b'/';
    cases.push(("slash name", bytes));
    for (label, bytes) in cases {
        let candidate = state.join("broken");
        fs::write(&candidate, bytes).unwrap();
        fs::rename(candidate, &delta).unwrap();
        assert!(Index::open(&base).is_err(), "accepted {label}");
        // A full scan can recover without reading the damaged sidecar.
    }
    let candidate = state.join("valid");
    fs::write(&candidate, &valid).unwrap();
    fs::rename(candidate, &delta).unwrap();
    assert!(
        Index::open(&base)
            .unwrap()
            .find_path(&root.join("new.txt"))
            .is_some()
    );
    fs::write(&delta, b"broken").unwrap();
    scan(&root, &state);
    assert!(!delta.exists());
}

#[test]
fn sidecars_from_another_base_are_ignored_and_special_files_are_not_followed() {
    let (_t, root, state) = fixture();
    scan(&root, &state);
    fs::write(root.join("new.txt"), b"new").unwrap();
    refresh(&root, &state, &["new.txt"]);
    let base = wckd_axit::index_path(&state);
    let delta = base.with_extension("delta");
    let saved = fs::read(&delta).unwrap();
    scan(&root, &state);
    fs::write(&delta, saved).unwrap();
    assert!(Index::open(&base).unwrap().last_refresh().is_none());
    fs::remove_file(&delta).unwrap();
    symlink(&base, &delta).unwrap();
    assert!(Index::open(&base).is_err());
    fs::remove_file(&delta).unwrap();
    fs::create_dir(&delta).unwrap();
    assert!(Index::open(&base).is_err());
    fs::remove_dir(&delta).unwrap();
    let file = fs::File::create(&delta).unwrap();
    file.set_len(16 * 1024 * 1024 + 1).unwrap();
    assert!(Index::open(&base).is_err());
}

#[test]
fn replaced_ancestors_require_a_broader_refresh_and_do_not_publish_partial_results() {
    let (_t, root, state) = fixture();
    let old = scan(&root, &state);
    let original = rows(&old);
    fs::rename(root.join("a"), root.join("previous-a")).unwrap();
    fs::create_dir_all(root.join("a/nested")).unwrap();
    fs::write(root.join("a/nested/two.txt"), b"replacement").unwrap();
    assert!(
        index::refresh(
            &root,
            &state,
            &[root.join("a/nested/two.txt")],
            Arc::new(Mutex::new(Progress::default())),
            Arc::new(AtomicBool::new(false))
        )
        .is_err()
    );
    let current = Index::open(&wckd_axit::index_path(&state)).unwrap();
    assert_eq!(rows(&current), original);
    refresh(&root, &state, &["a", "previous-a"]);
    assert_matches_scan(&root, &state);
}

#[test]
fn small_refresh_in_a_large_fixture_keeps_io_and_sidecar_size_bounded() {
    let (_t, root, state) = fixture();
    let bulk = root.join("bulk");
    fs::create_dir(&bulk).unwrap();
    for n in 0..10_000 {
        fs::File::create(bulk.join(format!("file-{n:05}"))).unwrap();
    }
    scan(&root, &state);
    let base = wckd_axit::index_path(&state);
    let before = fs::read(&base).unwrap();
    let started = std::time::Instant::now();
    fs::write(bulk.join("file-05000"), vec![8; 8192]).unwrap();
    let report = refresh(&root, &state, &["bulk/file-05000"]);
    assert_eq!(report.visited, 1);
    assert_eq!(report.added, 0);
    assert_eq!(report.removed, 0);
    assert!(report.delta_bytes < before.len() as u64 / 100);
    assert_eq!(fs::read(&base).unwrap(), before);
    assert_matches_scan(&root, &state);
    eprintln!(
        "10,000-file fixture: one visited entry, {} delta bytes, {:.1} ms refresh ({} ms including clean comparison)",
        report.delta_bytes,
        report.elapsed_ms,
        started.elapsed().as_millis()
    );
}

#[test]
fn inaccessible_directories_fail_without_removing_their_indexed_descendants() {
    let (_t, root, state) = fixture();
    // Root runners can bypass mode bits; the behavior is exercised on normal users.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let original = scan(&root, &state);
    let expected = rows(&original);
    let blocked = root.join("a/nested");
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();
    let outcome = index::refresh(
        &root,
        &state,
        &[PathBuf::from("a")],
        Arc::new(Mutex::new(Progress::default())),
        Arc::new(AtomicBool::new(false)),
    );
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(outcome.is_err());
    assert_eq!(
        rows(&Index::open(&wckd_axit::index_path(&state)).unwrap()),
        expected
    );
    assert!(
        !wckd_axit::index_path(&state)
            .with_extension("delta")
            .exists()
    );
}

#[test]
fn cli_status_and_search_use_live_overlay_records_not_the_id_high_water_mark() {
    let (_t, root, state) = fixture();
    scan(&root, &state);
    fs::remove_dir_all(root.join("a/nested")).unwrap();
    fs::write(root.join("new.txt"), b"new").unwrap();
    refresh(&root, &state, &["a/nested", "new.txt"]);
    let index = Index::open(&wckd_axit::index_path(&state)).unwrap();
    let run = |args: &[&str]| {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_axit"))
            .arg("--root")
            .arg(&root)
            .arg("--state-dir")
            .arg(&state)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    let status = run(&["status"]);
    assert_eq!(status["entries"].as_u64(), Some(index.live_count() as u64));
    assert_ne!(index.count, index.live_count());
    assert_eq!(status["last_refresh_unix"].as_i64(), index.last_refresh());
    let result = run(&["search", "new.txt", "--json"]);
    assert!(result.to_string().contains("new.txt"));
    let removed = run(&["search", "two.txt", "--json"]);
    assert_eq!(removed["matched"].as_u64(), Some(0));
}

#[test]
fn post_publication_report_failure_is_a_warning_with_a_readable_new_index() {
    let (_t, root, state) = fixture();
    scan(&root, &state);
    fs::write(root.join("new.txt"), b"new").unwrap();
    refresh(&root, &state, &["new.txt"]);
    fs::remove_file(state.join("latest-scan.json")).unwrap();
    fs::create_dir(state.join("latest-scan.json")).unwrap();
    let progress = Arc::new(Mutex::new(Progress::default()));
    let path = index::scan(
        &root,
        &state,
        progress.clone(),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    let current = Index::open(&path).unwrap();
    assert!(current.find_path(&root.join("new.txt")).is_some());
    assert!(current.last_refresh().is_none());
    assert!(!path.with_extension("delta").exists());
    assert!(
        progress
            .lock()
            .unwrap()
            .message
            .contains("coverage report could not be saved")
    );
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_axit"))
        .arg("--root")
        .arg(&root)
        .arg("--state-dir")
        .arg(&state)
        .arg("scan")
        .output()
        .unwrap();
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        report["scan_message"]
            .as_str()
            .unwrap()
            .contains("coverage report could not be saved")
    );
}

#[test]
fn damaged_sidecar_recovery_keeps_the_indexed_root_instead_of_falling_back_to_home() {
    let (temp, root, state) = fixture();
    scan(&root, &state);
    let home = temp.path().join("different-home");
    fs::create_dir(&home).unwrap();
    fs::write(home.join("not-in-scope"), b"private").unwrap();
    fs::write(
        wckd_axit::index_path(&state).with_extension("delta"),
        b"broken",
    )
    .unwrap();
    assert_eq!(
        Index::base_root(&wckd_axit::index_path(&state)).unwrap(),
        root
    );
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_axit"))
        .env("HOME", &home)
        .arg("--state-dir")
        .arg(&state)
        .arg("scan")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["root"].as_str(),
        Some(wckd_axit::text(&root).as_str())
    );
    let current = Index::open(&wckd_axit::index_path(&state)).unwrap();
    assert!(current.find_path(&root.join("a/one.txt")).is_some());
    assert!(current.find_path(&home.join("not-in-scope")).is_none());
}

#[test]
fn scoped_refresh_rejects_escape_and_unindexed_ancestors_before_publication() {
    let (_t, root, state) = fixture();
    scan(&root, &state);
    let base = wckd_axit::index_path(&state);
    let before = stamp(&base);
    let scope = root.join("a");
    for path in [
        PathBuf::from("../b"),
        root.join("b"),
        PathBuf::new(),
        PathBuf::from("bad\0name"),
    ] {
        assert!(
            index::refresh_scoped(
                &root,
                &state,
                &[path],
                &scope,
                Arc::new(Mutex::new(Progress::default())),
                Arc::new(AtomicBool::new(false))
            )
            .is_err()
        );
    }
    let scope = root.join("new-parent/session");
    fs::create_dir_all(&scope).unwrap();
    fs::write(scope.join("visible.txt"), b"visible").unwrap();
    fs::write(root.join("new-parent/private.txt"), b"private").unwrap();
    let error = index::refresh_scoped(
        &root,
        &state,
        &[PathBuf::from(".")],
        &scope,
        Arc::new(Mutex::new(Progress::default())),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap_err();
    assert!(error.to_string().contains("outside this scope"));
    assert_eq!(stamp(&base), before);
    assert!(!base.with_extension("delta").exists());
    refresh(&root, &state, &["new-parent"]);
    fs::write(scope.join("added.txt"), b"added").unwrap();
    let result = index::refresh_scoped(
        &root,
        &state,
        &[PathBuf::from(".")],
        &scope,
        Arc::new(Mutex::new(Progress::default())),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    assert_eq!(result.scopes, vec![wckd_axit::text(&scope)]);
    assert_matches_scan(&root, &state);
}

#[test]
fn refresh_path_preflight_is_bounded_and_does_not_follow_links() {
    let (_t, root, _state) = fixture();
    assert!(index::refresh_paths(&root, &[]).is_err());
    assert!(index::refresh_paths(&root, &vec![PathBuf::from("a"); 65]).is_err());
    assert!(index::refresh_paths(&root, &[PathBuf::from("a".repeat(65537))]).is_err());
    assert!(index::refresh_paths(Path::new("relative"), &[PathBuf::from("a")]).is_err());
    symlink(root.parent().unwrap(), root.join("escape")).unwrap();
    let value = index::refresh_paths(&root, &[PathBuf::from("escape/absent")]).unwrap();
    assert_eq!(value, vec![root.join("escape/absent")]);
    assert_eq!(
        index::refresh_paths(&root, &[PathBuf::from(".")]).unwrap(),
        vec![root]
    );
}

#[test]
fn cli_refresh_handles_relative_and_absolute_names_with_a_persistent_bounded_sidecar() {
    let (_t, root, state) = fixture();
    scan(&root, &state);
    let base = wckd_axit::index_path(&state);
    let before = stamp(&base);
    fs::write(root.join("a/with spaces.txt"), b"new").unwrap();
    fs::remove_file(root.join("b/keep.txt")).unwrap();
    let run = |paths: &[PathBuf]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_axit"))
            .arg("--state-dir")
            .arg(&state)
            .arg("refresh")
            .args(paths)
            .output()
            .unwrap()
    };
    let output = run(&[PathBuf::from("a"), root.join("b/keep.txt")]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["added"], 1);
    assert_eq!(result["removed"], 1);
    assert_eq!(result["published"], true);
    assert_eq!(stamp(&base), before);
    assert_matches_scan(&root, &state);
    let delta = fs::read(base.with_extension("delta")).unwrap();
    for paths in [
        vec![],
        vec![PathBuf::from("../outside")],
        vec![PathBuf::from("a"); 65],
    ] {
        assert!(!run(&paths).status.success());
        assert_eq!(fs::read(base.with_extension("delta")).unwrap(), delta);
    }
    let result = run(&[PathBuf::from("a")]);
    assert!(result.status.success());
    let result: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(result["published"], false);
    assert_eq!(fs::read(base.with_extension("delta")).unwrap(), delta);
}

struct RestoreDirectoryPermissions(PathBuf);
impl Drop for RestoreDirectoryPermissions {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
    }
}
#[test]
fn previously_restricted_empty_directories_keep_their_gap_while_other_paths_update() {
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let (_temp, root, state) = fixture();
    let blocked = root.join("a/nested");
    let _restore = RestoreDirectoryPermissions(blocked.clone());
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();
    let original = scan(&root, &state);
    assert_eq!(original.errors, 1);
    fs::write(root.join("a/new.txt"), b"new").unwrap();
    let report = refresh(&root, &state, &["a"]);
    assert_eq!(report.added, 1);
    let current = Index::open(&wckd_axit::index_path(&state)).unwrap();
    let restricted = current.entry(current.find_path(&blocked).unwrap()).unwrap();
    assert!(restricted.flags & index::RESTRICTED != 0);
    assert_eq!(current.errors, original.errors);
    assert!(current.find_path(&blocked.join("two.txt")).is_none());
    assert_matches_scan(&root, &state);
    let noop = refresh(&root, &state, &["a/nested"]);
    assert!(!noop.published);
}
#[test]
fn restricted_flags_never_allow_a_refresh_to_remove_previously_indexed_descendants() {
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let (_temp, root, state) = fixture();
    let blocked = root.join("a/nested");
    let _restore = RestoreDirectoryPermissions(blocked.clone());
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();
    scan(&root, &state);
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();
    refresh(&root, &state, &["a/nested/two.txt"]);
    let base = wckd_axit::index_path(&state);
    let old = Index::open(&base).unwrap();
    assert!(old.find_path(&blocked.join("two.txt")).is_some());
    assert!(old.entry(old.find_path(&blocked).unwrap()).unwrap().flags & index::RESTRICTED != 0);
    let before = fs::read(base.with_extension("delta")).unwrap();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();
    assert!(
        index::refresh(
            &root,
            &state,
            &[PathBuf::from("a")],
            Arc::new(Mutex::new(Progress::default())),
            Arc::new(AtomicBool::new(false))
        )
        .is_err()
    );
    assert_eq!(fs::read(base.with_extension("delta")).unwrap(), before);
    assert_eq!(Index::open(&base).unwrap().stamp(), old.stamp());
}
#[test]
fn newly_unreadable_directories_require_a_full_scan_to_record_the_new_coverage_gap() {
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let (_temp, root, state) = fixture();
    scan(&root, &state);
    let blocked = root.join("a/new-restricted");
    fs::create_dir(&blocked).unwrap();
    let _restore = RestoreDirectoryPermissions(blocked.clone());
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();
    assert!(
        index::refresh(
            &root,
            &state,
            &[PathBuf::from("a")],
            Arc::new(Mutex::new(Progress::default())),
            Arc::new(AtomicBool::new(false))
        )
        .is_err()
    );
    assert!(
        !wckd_axit::index_path(&state)
            .with_extension("delta")
            .exists()
    );
    let full = scan(&root, &state);
    assert_eq!(full.errors, 1);
    assert!(full.entry(full.find_path(&blocked).unwrap()).unwrap().flags & index::RESTRICTED != 0);
}
