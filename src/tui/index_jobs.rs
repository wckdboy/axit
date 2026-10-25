//! Explicit background metadata updates, with one snapshot publication path.
use super::*;
use crate::index::{self, RefreshReport};

impl App {
    pub(super) fn scan_if_missing(&mut self) {
        if self.index.is_none() && self.scan.is_none() {
            if let Some(error) = &self.index_error {
                self.status = error.clone();
            } else {
                self.scan();
            }
        }
    }
    pub(super) fn scan(&mut self) {
        self.start_index(None);
    }
    pub(super) fn update_folder(&mut self) {
        let Some(index) = &self.index else {
            self.scan();
            return;
        };
        let id = if self.view == View::Find {
            self.focused()
                .and_then(|id| index.entry(id))
                .map(|e| if e.is_dir() { e.id } else { e.parent })
                .unwrap_or(self.folder)
        } else {
            self.folder
        };
        match index.path(id) {
            Ok(path) => self.start_index(Some(path)),
            Err(error) => {
                self.status = format!("Cannot locate folder: {error:#}");
            }
        }
    }
    pub(super) fn cancel_index(&mut self) {
        if self.scan.is_some() {
            self.cancel.store(true, Ordering::Relaxed);
            self.status = "Canceling index work · browsing stays available".into();
        }
    }
    fn start_index(&mut self, path: Option<PathBuf>) {
        if self.scan.is_some() {
            self.status = "A scan or refresh is already running".into();
            return;
        }
        if self.applying || self.auto_busy {
            self.status = "Finish the file operation before updating".into();
            return;
        }
        self.cancel_plan();
        if matches!(
            self.modal,
            Some(Modal::Preview(_) | Modal::Destination(_) | Modal::Menu(_))
        ) {
            self.modal = None;
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
        self.refreshing = path.is_some();
        self.status = path
            .as_ref()
            .map(|p| format!("Updating {} · Ctrl-C cancels", crate::text(p)))
            .unwrap_or_else(|| "Scanning in the background · Ctrl-C cancels".into());
        let handle = thread::Builder::new()
            .name("axit-index".into())
            .spawn(move || {
                let message = if let Some(path) = path {
                    Message::Refreshed(
                        index::refresh(&root, &state, &[path], progress, cancel)
                            .map_err(|e| format!("{e:#}")),
                    )
                } else {
                    Message::Scan(
                        index::scan(&root, &state, progress, cancel).map_err(|e| format!("{e:#}")),
                    )
                };
                let _ = tx.send(message);
            });
        match handle {
            Ok(handle) => self.scan = Some(handle),
            Err(error) => {
                self.refreshing = false;
                self.status = format!("Could not start index worker: {error}");
            }
        }
    }
    pub(super) fn finish_index(
        &mut self,
        result: Result<PathBuf, String>,
        partial: Option<RefreshReport>,
    ) {
        if let Some(handle) = self.scan.take() {
            let _ = handle.join();
        }
        self.refreshing = false;
        match result.and_then(|p| Index::open(&p).map(Arc::new).map_err(|e| format!("{e:#}"))) {
            Ok(index) => {
                if !index.path(0).is_ok_and(|root| root == self.root) {
                    self.status =
                        "Published index scope changed externally · choose the intended root"
                            .into();
                    self.record(self.status.clone());
                    return;
                }
                let mut location = self.location();
                // Hidden-row changes also invalidate cursors when metadata is a no-op.
                location.page_cursor = None;
                if let Some(report) = &partial {
                    let previous = self.index.clone();
                    self.hidden.retain(|id| {
                        let Some(old) = previous.as_ref().and_then(|i| i.entry(*id)) else {
                            return false;
                        };
                        let Some(new) = index.entry(*id) else {
                            return false;
                        };
                        let Some(path) = previous.as_ref().and_then(|i| i.path(*id).ok()) else {
                            return false;
                        };
                        (old.device, old.inode) == (new.device, new.inode)
                            && index.path(*id).is_ok_and(|p| p == path)
                            && !report.paths.iter().any(|scope| path.starts_with(scope))
                    });
                    let whole_root = report.paths.iter().any(|p| self.root.starts_with(p));
                    self.needs_rescan =
                        !whole_root && (self.needs_rescan || !self.hidden.is_empty());
                } else {
                    self.hidden.clear();
                    self.needs_rescan = false;
                }
                self.index = Some(index);
                self.index_error = None;
                self.invalidate_rows();
                self.save_overlay();
                self.cancel_plan();
                if matches!(
                    self.modal,
                    Some(Modal::Preview(_) | Modal::Destination(_) | Modal::Menu(_))
                ) {
                    self.modal = None;
                }
                self.load_coverage();
                let focus = self.focus;
                self.restore_location(location);
                self.focus = focus;
                self.status = if let Some(report) = partial {
                    let summary = if report.published {
                        format!(
                            "Folder updated · +{} −{} · {} changed",
                            report.added, report.removed, report.updated
                        )
                    } else {
                        "Folder checked · no metadata changes".into()
                    };
                    match report.warning {
                        Some(warning) => format!("{summary} · {warning}"),
                        None => summary,
                    }
                } else {
                    let index = self.index.as_ref().unwrap();
                    let mut summary = format!(
                        "Indexed {} entries · {} restricted/missing",
                        index.live_count(),
                        index.errors
                    );
                    if let Ok(p) = self.progress.lock()
                        && p.message.starts_with("Scan complete · ")
                    {
                        summary.push_str(" · ");
                        summary.push_str(&p.message);
                    }
                    summary
                };
                self.record(self.status.clone());
            }
            Err(error) => {
                self.status = error;
                self.record(self.status.clone());
            }
        }
    }
}
