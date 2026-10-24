use std::{
    collections::HashSet,
    fs,
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{MetadataExt, symlink},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64},
    },
};
use wckd_axit::{
    index::{self, Index, Progress},
    ops::{self, Action},
    query::{self, Query, Sort},
};

fn scan(root: &Path, state: &Path) -> Index {
    let path = index::scan(
        root,
        state,
        Arc::new(Mutex::new(Progress::default())),
        Arc::new(AtomicBool::new(false)),
    )
    .expect("scan");
    Index::open(&path).expect("load")
}

fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = tempfile::tempdir().expect("fixture");
    let root = temp.path().join("files");
    let state = temp.path().join("state");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&state).unwrap();
    let root = root.canonicalize().unwrap();
    let state = state.canonicalize().unwrap();
    (temp, root, state)
}

fn git(root: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
}

fn rust_cache() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let (temp, root, state) = fixture();
    git(&root, &["init", "-q"]);
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname='fixture'\nversion='0.0.0'\n",
    )
    .unwrap();
    fs::write(root.join(".gitignore"), "/target/\n").unwrap();
    fs::write(root.join("source.rs"), "keep me").unwrap();
    git(&root, &["add", "Cargo.toml", ".gitignore", "source.rs"]);
    let cache = root.join("target/debug/incremental");
    fs::create_dir_all(&cache).unwrap();
    fs::write(
        root.join("target/CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n",
    )
    .unwrap();
    fs::write(cache.join("work-products.bin"), vec![42; 16384]).unwrap();
    (temp, root, state, cache)
}

#[test]
fn full_scan_counts_hardlinks_once_and_never_follows_symlink_cycles() {
    let (_t, root, state) = fixture();
    fs::write(root.join("file"), vec![7; 16384]).unwrap();
    fs::hard_link(root.join("file"), root.join("alias")).unwrap();
    symlink(&root, root.join("cycle")).unwrap();
    let index = scan(&root, &state);
    assert_eq!(index.count, 4);
    let files: Vec<_> = (0..index.count)
        .filter_map(|id| index.entry(id))
        .filter(|e| !e.is_dir() && !e.is_link())
        .collect();
    assert_eq!(
        files.iter().map(|e| e.allocated).sum::<u64>(),
        fs::metadata(root.join("file")).unwrap().blocks() * 512
    );
    assert_eq!(
        files.iter().filter(|e| e.flags & index::ALIAS != 0).count(),
        1
    );
    let link = index.find_path(&root.join("cycle")).unwrap();
    assert!(index.entry(link).unwrap().is_link());
}

#[test]
fn non_utf8_and_control_char_names_round_trip_without_terminal_escapes() {
    let (_t, root, state) = fixture();
    let raw = if cfg!(target_os = "macos") {
        "odd-λ\x1b[2J\nfile".as_bytes().to_vec()
    } else {
        b"odd-\xff\x1b[2J\nfile".to_vec()
    };
    let name = std::ffi::OsString::from_vec(raw);
    let path = root.join(name);
    fs::write(&path, b"data").unwrap();
    let index = scan(&root, &state);
    let id = index.find_path(&path).unwrap();
    assert_eq!(
        index.path(id).unwrap().as_os_str().as_bytes(),
        path.as_os_str().as_bytes()
    );
    let display = index.display_name(index.entry(id).unwrap());
    assert!(!display.contains('\x1b') && !display.contains('\n'));
    assert_eq!(ops::decode(&ops::encode(&path)).unwrap(), path);
    assert!(ops::decode("💀").is_err());
    assert!(ops::decode("00").is_err());
}

#[test]
fn canceled_scan_preserves_last_complete_index() {
    let (_t, root, state) = fixture();
    fs::write(root.join("original"), b"ok").unwrap();
    let first = scan(&root, &state);
    let count = first.count;
    let before = fs::read(wckd_axit::index_path(&state)).unwrap();
    fs::write(root.join("new"), b"new").unwrap();
    assert!(
        index::scan(
            &root,
            &state,
            Arc::new(Mutex::new(Progress::default())),
            Arc::new(AtomicBool::new(true))
        )
        .is_err()
    );
    assert_eq!(fs::read(wckd_axit::index_path(&state)).unwrap(), before);
    assert_eq!(
        Index::open(&wckd_axit::index_path(&state)).unwrap().count,
        count
    );
}

#[test]
fn corrupted_and_truncated_indexes_are_rejected() {
    let (_t, root, state) = fixture();
    fs::write(root.join("small"), b"ok").unwrap();
    scan(&root, &state);
    let valid = fs::read(wckd_axit::index_path(&state)).unwrap();
    for cut in [0, 7, 63, valid.len() - 1] {
        let p = state.join(format!("bad-{cut}"));
        fs::write(&p, &valid[..cut]).unwrap();
        assert!(Index::open(&p).is_err());
    }
    let mut bad = valid.clone();
    bad[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
    fs::write(state.join("overflow"), bad).unwrap();
    assert!(Index::open(&state.join("overflow")).is_err());
}

#[test]
fn queries_support_filters_globs_negation_sorting_and_cancellation() {
    let (_t, root, state) = fixture();
    fs::write(root.join("Keep.PDF"), vec![1; 20000]).unwrap();
    fs::write(root.join("other.pdf"), vec![1; 8000]).unwrap();
    fs::write(root.join("pic.png"), vec![1; 4000]).unwrap();
    let index = scan(&root, &state);
    let q = Query::parse("ext:pdf size:>10kb !other").unwrap();
    let r = query::search(&index, &q, Sort::Size, 10, &HashSet::new(), None);
    assert_eq!(r.matched, 1);
    assert_eq!(
        index.display_name(index.entry(r.ids[0]).unwrap()),
        "Keep.PDF"
    );
    let q = Query::parse("*.pdf").unwrap();
    let r = query::search(&index, &q, Sort::Name, 1, &HashSet::new(), None);
    assert_eq!(r.matched, 2);
    assert_eq!(r.ids.len(), 1);
    let q = Query::parse("").unwrap();
    let canceled = AtomicU64::new(1);
    assert!(
        query::search(
            &index,
            &q,
            Sort::Name,
            100,
            &HashSet::new(),
            Some((&canceled, 0))
        )
        .canceled
    );
    assert!(Query::parse("size:>nan").is_err());
    assert!(Query::parse("re:[").is_err());
    assert!(Query::parse("older:-1d").is_err());
    assert!(Query::parse("\"unclosed").is_err());
    assert_eq!(query::parse_size("1gib").unwrap(), 1024 * 1024 * 1024);
    assert_eq!(query::parse_size("1gb").unwrap(), 1_000_000_000);
}

#[test]
fn query_scope_excludes_sibling_private_directories() {
    let (_t, root, state) = fixture();
    fs::create_dir(root.join("project")).unwrap();
    fs::create_dir(root.join("private")).unwrap();
    fs::write(root.join("project/result.txt"), b"public").unwrap();
    fs::write(root.join("private/result.txt"), b"secret").unwrap();
    let index = scan(&root, &state);
    let scope = index.find_path(&root.join("project")).unwrap();
    let mut q = Query::parse("result").unwrap();
    q.set_scope(scope);
    let r = query::search(&index, &q, Sort::Name, 100, &HashSet::new(), None);
    assert_eq!(r.matched, 1);
    assert!(
        index
            .path(r.ids[0])
            .unwrap()
            .starts_with(root.join("project"))
    );
}

#[test]
fn plans_are_read_only_and_cache_cleanup_keeps_source_and_requires_confirmation() {
    let (_t, root, state, cache) = rust_cache();
    let index = scan(&root, &state);
    let id = index.find_path(&cache).unwrap();
    let plan = ops::plan(&index, &[id], Action::Clean, &state, 0).unwrap();
    assert_eq!(plan.entries.len(), 1, "{:?}", plan.blocked);
    assert!(cache.exists());
    assert!(ops::apply(&state, &plan, "yes").is_err());
    assert!(cache.exists());
    let result = ops::apply(&state, &plan, "CLEAN").unwrap();
    assert_eq!(result.completed.len(), 1, "{:?}", result.blocked);
    assert!(!cache.exists());
    assert_eq!(fs::read(root.join("source.rs")).unwrap(), b"keep me");
    assert!(root.join("Cargo.toml").exists());
    assert!(ops::apply(&state, &plan, "CLEAN").is_err());
}

#[test]
fn packages_signing_material_and_models_block_permanent_cleanup() {
    for name in ["release.apk", "cert.p12", "model.gguf", "backup.zip"] {
        let (_t, root, state, cache) = rust_cache();
        fs::write(cache.join(name), b"preserve").unwrap();
        let index = scan(&root, &state);
        let plan = ops::plan(
            &index,
            &[index.find_path(&cache).unwrap()],
            Action::Clean,
            &state,
            0,
        )
        .unwrap();
        assert!(plan.entries.is_empty(), "{name}");
        assert_eq!(fs::read(cache.join(name)).unwrap(), b"preserve");
    }
}

#[test]
fn tracked_source_and_external_symlinks_are_never_cleaned() {
    let (_t, root, state, cache) = rust_cache();
    fs::write(cache.join("tracked.rs"), b"source").unwrap();
    git(&root, &["add", "-f", "target/debug/incremental/tracked.rs"]);
    let index = scan(&root, &state);
    let plan = ops::plan(
        &index,
        &[index.find_path(&cache).unwrap()],
        Action::Clean,
        &state,
        0,
    )
    .unwrap();
    assert!(plan.entries.is_empty());
    git(
        &root,
        &[
            "rm",
            "--cached",
            "-q",
            "target/debug/incremental/tracked.rs",
        ],
    );
    fs::remove_file(cache.join("tracked.rs")).unwrap();
    symlink(root.join("source.rs"), cache.join("escape")).unwrap();
    let index = scan(&root, &state);
    let plan = ops::plan(
        &index,
        &[index.find_path(&cache).unwrap()],
        Action::Clean,
        &state,
        0,
    )
    .unwrap();
    assert!(plan.entries.is_empty());
    assert!(root.join("source.rs").exists());
}

#[test]
fn stale_or_changed_cache_plans_are_rejected_without_touching_files() {
    let (_t, root, state, cache) = rust_cache();
    let index = scan(&root, &state);
    let id = index.find_path(&cache).unwrap();
    let plan = ops::plan(&index, &[id], Action::Clean, &state, 0).unwrap();
    fs::write(cache.join("changed.bin"), b"new").unwrap();
    let result = ops::apply(&state, &plan, "CLEAN").unwrap();
    assert!(result.completed.is_empty());
    assert!(cache.join("changed.bin").exists());
    let mut expired = plan.clone();
    expired.created -= 601;
    assert!(ops::apply(&state, &expired, "CLEAN").is_err());
    let outside = root.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("valuable"), b"keep").unwrap();
    fs::remove_dir_all(&cache).unwrap();
    symlink(&outside, &cache).unwrap();
    let result = ops::plan(&index, &[id], Action::Clean, &state, 0).unwrap();
    assert!(result.entries.is_empty());
    assert_eq!(fs::read(outside.join("valuable")).unwrap(), b"keep");
}

#[test]
fn active_files_and_recent_caches_are_blocked() {
    let (_t, root, state, cache) = rust_cache();
    let index = scan(&root, &state);
    let id = index.find_path(&cache).unwrap();
    let _open = fs::File::open(cache.join("work-products.bin")).unwrap();
    let plan = ops::plan(&index, &[id], Action::Clean, &state, 0).unwrap();
    assert!(plan.entries.is_empty());
    assert!(plan.blocked[0].reason.contains("In use"));
    drop(_open);
    let plan = ops::plan(&index, &[id], Action::Clean, &state, 86400).unwrap();
    assert!(plan.entries.is_empty());
    assert!(plan.blocked[0].reason.contains("recent"));
}

#[test]
fn moves_are_reversible_and_never_replace_an_existing_file() {
    let (_t, root, state) = fixture();
    let source = root.join("file.txt");
    let dest = root.join("organized");
    fs::write(&source, b"original").unwrap();
    fs::create_dir(&dest).unwrap();
    let index = scan(&root, &state);
    let plan = ops::plan(
        &index,
        &[index.find_path(&source).unwrap()],
        Action::Move {
            destination: dest.to_string_lossy().into(),
        },
        &state,
        0,
    )
    .unwrap();
    assert_eq!(plan.entries.len(), 1, "{:?}", plan.blocked);
    let result = ops::apply(&state, &plan, "MOVE").unwrap();
    assert_eq!(result.completed.len(), 1, "{:?}", result.blocked);
    assert_eq!(fs::read(dest.join("file.txt")).unwrap(), b"original");
    ops::undo(&state).unwrap();
    assert_eq!(fs::read(&source).unwrap(), b"original");
    fs::write(dest.join("file.txt"), b"keep destination").unwrap();
    let plan = ops::plan(
        &index,
        &[index.find_path(&source).unwrap()],
        Action::Move {
            destination: dest.to_string_lossy().into(),
        },
        &state,
        0,
    )
    .unwrap();
    assert!(plan.entries.is_empty());
    assert_eq!(
        fs::read(dest.join("file.txt")).unwrap(),
        b"keep destination"
    );
}

#[test]
fn exclusive_rename_refuses_racing_destinations() {
    let (_t, root, _s) = fixture();
    fs::write(root.join("a"), b"A").unwrap();
    fs::write(root.join("b"), b"B").unwrap();
    assert!(ops::rename_exclusive(&root.join("a"), &root.join("b")).is_err());
    assert_eq!(fs::read(root.join("a")).unwrap(), b"A");
    assert_eq!(fs::read(root.join("b")).unwrap(), b"B");
}

#[test]
fn automatic_cleanup_is_disabled_by_default_and_rejects_broad_scopes() {
    let (_t, _root, state) = fixture();
    let p = ops::auto_policy(&state).unwrap();
    assert!(!p.enabled);
    assert!(p.roots.is_empty());
    let mut p = p;
    p.enabled = true;
    assert!(ops::save_auto(&state, &p).is_err());
    p.roots = vec![wckd_axit::home().unwrap().to_string_lossy().into()];
    assert!(ops::save_auto(&state, &p).is_err());
}

#[test]
fn automatic_cleanup_only_removes_old_debug_cache_and_obeys_its_cooldown() {
    let (_t, root, state, cache) = rust_cache();
    let release = root.join("target/release");
    fs::create_dir(&release).unwrap();
    fs::write(release.join("published.unknown"), b"keep release").unwrap();
    let old = fs::FileTimes::new()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(8 * 86400));
    for path in [
        cache.join("work-products.bin"),
        cache.clone(),
        root.join("target/debug"),
    ] {
        fs::File::open(path).unwrap().set_times(old).unwrap();
    }
    let index = scan(&root, &state);
    let policy = ops::AutoPolicy {
        enabled: true,
        roots: vec![root.to_string_lossy().into()],
        below_free_bytes: u64::MAX,
        ..Default::default()
    };
    ops::save_auto(&state, &policy).unwrap();
    let result = ops::auto_run(&state, &index).unwrap().unwrap();
    assert_eq!(result.completed.len(), 1, "{:?}", result.blocked);
    assert!(!root.join("target/debug").exists());
    assert_eq!(
        fs::read(release.join("published.unknown")).unwrap(),
        b"keep release"
    );
    assert!(root.join("source.rs").exists());
    assert!(ops::auto_run(&state, &index).unwrap().is_none());
}

#[test]
fn tui_renders_small_and_wide_terminals_without_panicking() {
    let (_t, root, state) = fixture();
    fs::write(root.join("hello.txt"), b"hello").unwrap();
    scan(&root, &state);
    let mut app = wckd_axit::tui::App::new(root, state);
    for (width, height) in [(160, 44), (85, 24), (48, 14), (30, 8)] {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| wckd_axit::tui::draw(frame, &mut app))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let output = buffer
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(output.contains("AXIT"));
    }
}

#[test]
fn scanner_excludes_its_private_state_inside_the_scope() {
    let (_t, root, _state) = fixture();
    let state = root.join("private-index");
    fs::create_dir(&state).unwrap();
    fs::write(root.join("keep"), b"data").unwrap();
    let index = scan(&root, &state);
    assert_eq!(index.count, 2);
    assert!(index.find_path(&state).is_none());
}

#[cfg(target_os = "macos")]
#[test]
fn data_volume_aliases_work_for_lookup_and_self_exclusion() {
    let (_t, root, _state) = fixture();
    let state = root.join("private-index");
    fs::create_dir(&state).unwrap();
    fs::write(root.join("find-me"), b"data").unwrap();
    let alias = Path::new("/System/Volumes/Data").join(root.strip_prefix("/").unwrap());
    let index = scan(&alias, &state);
    assert_eq!(index.count, 2);
    assert!(index.find_path(&root.join("find-me")).is_some());
    assert!(index.find_path(&state).is_none());
}

#[test]
fn a_replaced_destination_directory_blocks_a_move() {
    let (_t, root, state) = fixture();
    fs::write(root.join("source"), b"keep").unwrap();
    let dest = root.join("dest");
    fs::create_dir(&dest).unwrap();
    let index = scan(&root, &state);
    let plan = ops::plan(
        &index,
        &[index.find_path(&root.join("source")).unwrap()],
        Action::Move {
            destination: dest.to_string_lossy().into(),
        },
        &state,
        0,
    )
    .unwrap();
    fs::rename(&dest, root.join("old-dest")).unwrap();
    fs::create_dir(&dest).unwrap();
    let applied = ops::apply(&state, &plan, "MOVE").unwrap();
    assert!(applied.completed.is_empty());
    assert_eq!(fs::read(root.join("source")).unwrap(), b"keep");
}

#[test]
fn deleted_parent_hides_its_indexed_descendants() {
    let (_t, root, state) = fixture();
    fs::create_dir(root.join("parent")).unwrap();
    fs::write(root.join("parent/file"), b"data").unwrap();
    let index = scan(&root, &state);
    let parent = index.find_path(&root.join("parent")).unwrap();
    let mut hidden = HashSet::new();
    hidden.insert(parent);
    let result = query::search(
        &index,
        &Query::parse("").unwrap(),
        Sort::Name,
        100,
        &hidden,
        None,
    );
    assert!(
        result
            .ids
            .iter()
            .all(|id| !index.is_descendant(*id, parent))
    );
}

#[test]
fn cargo_release_profiles_survive_cleanup_while_debug_caches_remain_eligible() {
    let (_t, root, state, _cache) = rust_cache();
    let target = root.join("target");
    fs::create_dir_all(target.join("release")).unwrap();
    for name in [
        "program",
        "published.deb",
        "published.rpm",
        "future-package.unknown",
    ] {
        fs::write(target.join("release").join(name), b"release output").unwrap();
    }
    let index = scan(&root, &state);
    let id = index.find_path(&target).unwrap();
    let plan = ops::plan(&index, &[id], Action::Clean, &state, 0).unwrap();
    assert!(plan.entries.is_empty());
    assert!(!plan.blocked.is_empty());
    let debug = target.join("debug");
    let entry = index.entry(index.find_path(&debug).unwrap()).unwrap();
    assert!(ops::cache_candidate(&index, entry));
    let plan = ops::plan(&index, &[entry.id], Action::Clean, &state, 0).unwrap();
    assert_eq!(plan.entries.len(), 1, "{:?}", plan.blocked);
    let applied = ops::apply(&state, &plan, "CLEAN").unwrap();
    assert_eq!(applied.completed.len(), 1, "{:?}", applied.blocked);
    assert!(!debug.exists());
    for name in [
        "program",
        "published.deb",
        "published.rpm",
        "future-package.unknown",
    ] {
        assert_eq!(
            fs::read(target.join("release").join(name)).unwrap(),
            b"release output"
        );
    }
    assert!(root.join("source.rs").exists());
    fs::rename(target.join("release"), target.join("release-lto")).unwrap();
    let index = scan(&root, &state);
    let plan = ops::plan(
        &index,
        &[index.find_path(&target).unwrap()],
        Action::Clean,
        &state,
        0,
    )
    .unwrap();
    assert!(
        plan.entries.is_empty(),
        "A custom release profile must not permit whole-target cleanup"
    );
    assert_eq!(
        fs::read(target.join("release-lto/program")).unwrap(),
        b"release output"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn cache_cleanup_works_through_the_data_volume_alias() {
    let (_t, root, state, cache) = rust_cache();
    let alias = Path::new("/System/Volumes/Data").join(root.strip_prefix("/").unwrap());
    let index = scan(&alias, &state);
    let id = index.find_path(&cache).unwrap();
    let plan = ops::plan(&index, &[id], Action::Clean, &state, 0).unwrap();
    assert_eq!(plan.entries.len(), 1, "{:?}", plan.blocked);
    let result = ops::apply(&state, &plan, "CLEAN").unwrap();
    assert_eq!(result.completed.len(), 1, "{:?}", result.blocked);
    assert!(!cache.exists());
    assert!(root.join("source.rs").exists());
}
