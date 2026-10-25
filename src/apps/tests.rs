use super::*;
use std::{
    fs,
    os::unix::fs::symlink,
    path::Path,
    time::{Duration, Instant},
};

fn fixture() -> (tempfile::TempDir, Context) {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap().join("home");
    let app_roots = vec![home.join("Applications")];
    let desktop_roots = vec![home.join(".local/share/applications")];
    for p in app_roots.iter().chain(desktop_roots.iter()) {
        fs::create_dir_all(p).unwrap();
    }
    (
        temp,
        Context {
            home,
            app_roots,
            desktop_roots,
            tools: ManagedTools::default(),
            scope: None,
        },
    )
}
fn bundle(path: &Path, id: &str, binary: bool) {
    fs::create_dir_all(path.join("Contents")).unwrap();
    let mut values = plist::Dictionary::new();
    values.insert("CFBundleIdentifier".into(), id.into());
    values.insert("CFBundleName".into(), "Alpha".into());
    values.insert("CFBundleShortVersionString".into(), "1.2.3".into());
    let value = plist::Value::Dictionary(values);
    if binary {
        value
            .to_file_binary(path.join("Contents/Info.plist"))
            .unwrap();
    } else {
        value.to_file_xml(path.join("Contents/Info.plist")).unwrap();
    }
}
#[test]
fn bundle_inventory_reads_binary_and_xml_without_walking_inside_apps() {
    let (_temp, ctx) = fixture();
    let root = &ctx.app_roots[0];
    bundle(&root.join("Alpha.app"), "org.wckd.Alpha", true);
    bundle(&root.join("Beta.app"), "org.wckd.Beta", false);
    bundle(
        &root.join("Alpha.app/Contents/Helper.app"),
        "org.wckd.Helper",
        false,
    );
    let result = inventory(&ctx);
    assert_eq!(result.apps.len(), 2);
    assert!(
        result
            .apps
            .iter()
            .all(|a| a.version.as_deref() == Some("1.2.3"))
    );
    let bad = root.join("Bad.app");
    bundle(&bad, "../../.ssh", false);
    assert!(read_bundle(&bad).is_err());
    fs::write(
        bad.join("Contents/Info.plist"),
        vec![b'x'; 2 * 1024 * 1024 + 1],
    )
    .unwrap();
    assert!(read_bundle(&bad).unwrap_err().to_string().contains("2 MiB"));
}
#[test]
fn related_files_use_complete_ids_and_protect_shared_groups_and_symlinks() {
    let (_temp, ctx) = fixture();
    let app_path = ctx.app_roots[0].join("Alpha.app");
    bundle(&app_path, "org.wckd.Alpha", false);
    for p in [
        "Library/Caches/org.wckd.Alpha",
        "Library/Caches/org.wckd.AlphaBeta",
        "Library/Application Support/Alpha",
        "Library/Group Containers/org.wckd.Alpha",
    ] {
        fs::create_dir_all(ctx.home.join(p)).unwrap();
    }
    let mut app = read_bundle(&app_path).unwrap();
    app.related_hints
        .push(ctx.home.join("Library/Group Containers/org.wckd.Alpha"));
    let files = related_files(&ctx, &app);
    assert!(!files.iter().any(|f| f.path.ends_with("org.wckd.AlphaBeta")));
    assert!(files.iter().any(|f| f.kind == RelatedKind::Cache
        && f.confidence == Confidence::Exact
        && !f.contains_user_data));
    assert!(files.iter().any(|f| f.path.ends_with("Alpha")
        && f.confidence == Confidence::Review
        && f.contains_user_data));
    assert!(files.iter().any(|f| {
        f.path
            .starts_with(ctx.home.join("Library/Group Containers"))
            && !f.selectable
    }));
    let cache = ctx.home.join("Library/Caches/org.wckd.Alpha");
    fs::remove_dir(&cache).unwrap();
    symlink(ctx.home.join("Applications"), &cache).unwrap();
    assert!(
        related_files(&ctx, &app)
            .iter()
            .any(|f| f.path == cache && !f.selectable)
    );
}
#[test]
fn desktop_launchers_are_metadata_and_never_execute_their_exec_field() {
    let (_temp, ctx) = fixture();
    let marker = ctx.home.join("should-not-exist");
    let desktop = ctx.desktop_roots[0].join("org.wckd.Alpha.desktop");
    fs::write(
        &desktop,
        format!(
            "[Desktop Entry]\nType=Application\nName=Alpha\nExec=touch {}\n",
            marker.display()
        ),
    )
    .unwrap();
    let result = inventory(&ctx);
    assert_eq!(result.apps.len(), 1);
    assert_eq!(result.apps[0].id, "org.wckd.Alpha");
    assert!(!marker.exists());
}
#[test]
fn brew_hints_do_not_enable_globs_or_parent_traversal_and_flatpak_refs_are_precise() {
    let (_temp, ctx) = fixture();
    let json = serde_json::json!({"casks":[{"token":"alpha","name":["Alpha"],"installed":"1.0","artifacts":[{"zap":[{"trash":["~/Library/Caches/org.wckd.Alpha","~/../../outside","~/Library/Preferences/alpha*","/Library/shared"]}]}]}]});
    let apps = super::managed::parse_brew(&ctx, &json).unwrap();
    assert_eq!(
        apps[0].related_hints,
        vec![ctx.home.join("Library/Caches/org.wckd.Alpha")]
    );
    let apps = super::managed::parse_flatpak(
        "org.wckd.Alpha/x86_64/stable\tAlpha\tuser\napp/org.wckd.Alpha/x86_64/beta\tAlpha Beta\tsystem\n../../escape\tBad\tuser\n",
    );
    assert_eq!(apps.len(), 2);
    assert!(matches!(
        &apps[0].source,
        Source::Flatpak { user: true, reference } if reference=="app/org.wckd.Alpha/x86_64/stable"
    ));
    assert!(matches!(
        &apps[1].source,
        Source::Flatpak { user: false, .. }
    ));
}
#[test]
fn live_app_plans_need_no_index_and_revalidate_bundle_identity() {
    let (_temp, ctx) = fixture();
    let state = ctx.home.join("state");
    fs::create_dir(&state).unwrap();
    let app = ctx.app_roots[0].join("Alpha.app");
    bundle(&app, "org.wckd.Alpha", false);
    let plan = crate::ops::plan_app_trash(
        &ctx.home,
        std::slice::from_ref(&app),
        &state,
        None,
        Some(&read_bundle(&app).unwrap()),
    )
    .unwrap();
    assert_eq!(plan.entries.len(), 1);
    assert_eq!(plan.entries[0].id, u32::MAX);
    assert!(plan.entries[0].size_unknown);
    assert!(app.exists());
    bundle(&app, "org.wckd.Replaced", false);
    let result = crate::ops::apply(&state, &plan, "TRASH").unwrap();
    assert!(result.completed.is_empty());
    assert!(result.blocked[0].reason.contains("identity changed"));
    assert!(app.exists());
}
#[test]
fn package_queries_have_time_and_output_limits() {
    let start = Instant::now();
    assert!(
        super::command::query(
            Path::new("/bin/sh"),
            &["-c", "sleep 5"],
            Duration::from_millis(40)
        )
        .is_err()
    );
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(super::command::query(Path::new("/usr/bin/yes"), &[], Duration::from_secs(2)).is_err());
}

#[test]
fn scoped_inventory_and_files_reject_symlink_escapes() {
    let (temp, ctx) = fixture();
    let root = &ctx.app_roots[0];
    bundle(&root.join("Alpha.app"), "org.wckd.Alpha", false);
    let outside = temp.path().join("outside.app");
    bundle(&outside, "org.wckd.Private", false);
    symlink(&outside, root.join("Escaped.app")).unwrap();
    let scoped = ctx.clone().constrained(&ctx.home).unwrap();
    let catalog = inventory(&scoped);
    assert_eq!(catalog.apps.len(), 1);
    assert_eq!(catalog.apps[0].id, "org.wckd.Alpha");
    bundle(
        &root.join("Alpha.app/Contents/Helper.app"),
        "org.wckd.Helper",
        false,
    );
    let single = inventory(&ctx.clone().constrained(&root.join("Alpha.app")).unwrap());
    assert_eq!(single.apps.len(), 1);
    assert_eq!(single.apps[0].id, "org.wckd.Alpha");
    let file = root.join("Alpha.app/Contents/Info.plist");
    fs::remove_file(&file).unwrap();
    symlink(outside.join("Contents/Info.plist"), &file).unwrap();
    assert!(inventory(&scoped).apps.is_empty());
    assert!(read_bundle(&root.join("Alpha.app")).is_err());
    let cache = ctx.home.join("Library/Caches/org.wckd.Alpha");
    fs::create_dir_all(cache.parent().unwrap()).unwrap();
    symlink(temp.path(), &cache).unwrap();
    assert!(
        related_files(&scoped, &catalog.apps[0])
            .iter()
            .all(|f| f.path != cache)
    );
}

#[test]
fn history_distinguishes_remembered_apps_from_unmatched_data_and_shared_copies() {
    let (temp, ctx) = fixture();
    let path = ctx.app_roots[0].join("Alpha.app");
    bundle(&path, "org.wckd.Alpha", false);
    let app = read_bundle(&path).unwrap();
    let cache = ctx.home.join("Library/Caches/org.wckd.Alpha");
    fs::create_dir_all(&cache).unwrap();
    fs::create_dir_all(ctx.home.join("Library/Caches/org.service.Unknown")).unwrap();
    let state = temp.path().join("state");
    fs::create_dir(&state).unwrap();
    remember(&state, &app).unwrap();
    let catalog = inventory(&ctx);
    assert!(
        leftovers(&ctx, &catalog, &state)
            .unwrap()
            .iter()
            .all(|a| a.id != app.id)
    );
    let mut copy = app.clone();
    copy.path = Some(ctx.app_roots[0].join("Other.app"));
    let shared = Catalog {
        apps: vec![app.clone(), copy],
        warnings: vec![],
    };
    assert!(
        review_files(&ctx, &shared, &app)
            .iter()
            .filter(|f| f.kind != RelatedKind::Application)
            .all(|f| !f.selectable)
    );
    fs::remove_dir_all(&path).unwrap();
    let candidates = leftovers(&ctx, &inventory(&ctx), &state).unwrap();
    let known = candidates.iter().find(|a| a.id == app.id).unwrap();
    assert!(known.remembered);
    assert!(
        related_files(&ctx, known)
            .iter()
            .any(|f| default_file(known, f, false))
    );
    let unknown = candidates
        .iter()
        .find(|a| a.id == "org.service.Unknown")
        .unwrap();
    assert!(!unknown.remembered);
    assert!(
        related_files(&ctx, unknown)
            .iter()
            .all(|f| !default_file(unknown, f, false))
    );
}

#[test]
fn changed_primary_bundle_keeps_all_selected_personal_data() {
    let (temp, ctx) = fixture();
    let path = ctx.app_roots[0].join("Alpha.app");
    bundle(&path, "org.wckd.Alpha", false);
    let app = read_bundle(&path).unwrap();
    let data = ctx.home.join("Library/Application Support/org.wckd.Alpha");
    fs::create_dir_all(&data).unwrap();
    fs::write(data.join("important.txt"), b"keep me").unwrap();
    let state = temp.path().join("state");
    fs::create_dir(&state).unwrap();
    let plan = crate::ops::plan_app_trash(
        &ctx.home,
        &[data.clone(), path.clone()],
        &state,
        None,
        Some(&app),
    )
    .unwrap();
    assert_eq!(plan.entries.len(), 2);
    assert_eq!(plan.entries[0].path, crate::text(&path));
    fs::rename(&path, ctx.app_roots[0].join("Old.app")).unwrap();
    bundle(&path, "org.wckd.Alpha", false);
    let result = crate::ops::apply(&state, &plan, "TRASH").unwrap();
    assert!(result.completed.is_empty());
    assert_eq!(result.blocked.len(), 2);
    assert!(path.exists() && data.join("important.txt").exists());
}

#[test]
fn system_flatpak_inventory_replaces_the_launcher_protection_with_native_ownership() {
    use std::os::unix::fs::PermissionsExt;
    let (temp, mut ctx) = fixture();
    let exported = temp.path().join("system-applications");
    fs::create_dir(&exported).unwrap();
    fs::write(
        exported.join("org.wckd.Flat.desktop"),
        "[Desktop Entry]\nType=Application\nName=Flat\nX-Flatpak=org.wckd.Flat\n",
    )
    .unwrap();
    ctx.desktop_roots = vec![exported];
    assert!(inventory(&ctx).apps[0].protected);
    let tool = temp.path().join("flatpak");
    fs::write(
        &tool,
        "#!/bin/sh\nprintf 'org.wckd.Flat/x86_64/stable\\tFlat\\tsystem\\n'\n",
    )
    .unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o700)).unwrap();
    ctx.tools.flatpak = Some(tool);
    let catalog = inventory(&ctx);
    assert_eq!(catalog.apps.len(), 1);
    assert!(matches!(
        catalog.apps[0].source,
        Source::Flatpak { user: false, .. }
    ));
    assert!(!catalog.apps[0].protected);
}

#[test]
fn multi_bundle_casks_have_unique_selection_keys_and_ambiguous_tokens_require_a_choice() {
    let (_temp, ctx) = fixture();
    let a = ctx.app_roots[0].join("Alpha.app");
    let b = ctx.app_roots[0].join("Beta.app");
    bundle(&a, "org.wckd.Alpha", false);
    bundle(&b, "org.wckd.Beta", false);
    let mut a = read_bundle(&a).unwrap();
    let mut b = read_bundle(&b).unwrap();
    a.source = Source::Brew {
        token: "suite".into(),
    };
    b.source = a.source.clone();
    let catalog = Catalog {
        apps: vec![a.clone(), b.clone()],
        warnings: vec![],
    };
    assert_ne!(a.key(), b.key());
    assert_eq!(resolve(&catalog, &b.key()).unwrap().id, b.id);
    assert!(resolve(&catalog, "brew:suite").is_err());
    assert_eq!(
        resolve(
            &Catalog {
                apps: vec![b.clone()],
                warnings: vec![]
            },
            "brew:suite"
        )
        .unwrap()
        .id,
        b.id
    );
}

#[test]
fn malformed_advisory_history_is_recoverable_and_keeps_one_private_backup() {
    use std::os::unix::fs::MetadataExt;
    let (temp, ctx) = fixture();
    let path = ctx.app_roots[0].join("Alpha.app");
    bundle(&path, "org.wckd.Alpha", false);
    let app = read_bundle(&path).unwrap();
    let state = temp.path().join("state");
    fs::create_dir(&state).unwrap();
    let file = state.join("apps-history.json");
    fs::write(&file, b"{truncated").unwrap();
    assert!(leftovers(&ctx, &inventory(&ctx), &state).is_ok());
    remember(&state, &app).unwrap();
    let backup = state.join("apps-history.unreadable.json");
    assert_eq!(fs::read(&backup).unwrap(), b"{truncated");
    assert_eq!(fs::metadata(&backup).unwrap().mode() & 0o077, 0);
    let mut records: serde_json::Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    let mut future = records[0].clone();
    future["app"]["source"] = serde_json::json!({"kind":"future_provider"});
    records.as_array_mut().unwrap().push(future);
    let damaged = serde_json::to_vec(&records).unwrap();
    fs::write(&file, &damaged).unwrap();
    remember(&state, &app).unwrap();
    assert_eq!(fs::read(&backup).unwrap(), damaged);
    let valid: serde_json::Value = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
    assert_eq!(valid.as_array().unwrap().len(), 1);
}
