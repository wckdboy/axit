//! Isolated PTY harness. Every mutation requires an explicit temporary fixture marker.
use anyhow::{Context as _, Result, ensure};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};
use wckd_axit::apps::{Context, ManagedTools};
fn bundle(path: &Path, id: &str) -> Result<()> {
    fs::create_dir_all(path.join("Contents"))?;
    fs::write(
        path.join("Contents/Info.plist"),
        format!(
            r#"<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>{id}</string><key>CFBundleName</key><string>{}</string></dict></plist>"#,
            path.file_stem().unwrap().to_string_lossy()
        ),
    )?;
    Ok(())
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let root = PathBuf::from(
        std::env::var_os("AXIT_APP_FIXTURE_ROOT").context("Explicit fixture root required")?,
    )
    .canonicalize()?;
    ensure!(
        root.starts_with(Path::new("/tmp").canonicalize()?)
            && fs::read(root.join(".axit-apps-fixture"))? == b"axit-owned-test-fixture",
        "Only marked temporary fixtures are allowed"
    );
    if args.first().is_some_and(|s| s == "info") {
        let casks:Vec<_>=["Alpha","Gamma"].iter().filter(|name|root.join(format!("Applications/{name}.app")).exists()).map(|name|serde_json::json!({"token":name.to_lowercase(),"name":[name],"installed":"1.0","artifacts":[{"app":[format!("{name}.app")],"target":root.join(format!("Applications/{name}.app"))},{"zap":[{"trash":[format!("~/Library/Caches/org.wckd.{name}")]}]}]})).collect();
        println!("{}", serde_json::json!({"casks":casks}));
        return Ok(());
    }
    if args
        .first()
        .is_some_and(|s| s == "install" || s == "uninstall")
    {
        ensure!(
            args.len() == 4 && args[1] == "--cask" && args[2] == "--",
            "Unexpected native arguments"
        );
        let name = match args[3].as_str() {
            "alpha" => "Alpha",
            "gamma" => "Gamma",
            _ => anyhow::bail!("Unknown fixture package"),
        };
        let path = root.join(format!("Applications/{name}.app"));
        print!("Proceed with fixture {}? [y/N] ", args[0]);
        io::stdout().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        ensure!(answer.trim() == "y", "Fixture operation canceled");
        if args[0] == "install" {
            bundle(&path, &format!("org.wckd.{name}"))?;
        } else {
            fs::remove_dir_all(&path)?;
        }
        println!("Fixture {} complete: {name}", args[0]);
        return Ok(());
    }
    let state = args
        .windows(2)
        .find(|a| a[0] == "--state-dir")
        .map(|a| PathBuf::from(&a[1]))
        .context("State required")?;
    ensure!(
        state.starts_with(&root),
        "Fixture state must stay inside fixture root"
    );
    fs::create_dir_all(&state)?;
    let context = Context {
        home: root.clone(),
        app_roots: vec![root.join("Applications")],
        desktop_roots: vec![],
        tools: ManagedTools {
            brew: Some(std::env::current_exe()?),
            flatpak: None,
        },
        scope: None,
    };
    wckd_axit::tui::run_apps_with_context(root, state, context)
}
