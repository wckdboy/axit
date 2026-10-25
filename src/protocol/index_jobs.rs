//! One cancellable index publisher; searches retain their previous Arc snapshot.
use super::*;
use crate::index::{self, RefreshReport};

#[derive(Clone)]
pub(super) struct JobStatus {
    kind: &'static str,
    scope: PathBuf,
    owner: Option<String>,
    report: Option<RefreshReport>,
    error: Option<String>,
    finished: Option<i64>,
}
enum Work {
    Scan,
    Refresh(Vec<PathBuf>),
}
impl Shared {
    pub(super) fn scan(self: &Arc<Self>, owner: Option<&str>) -> Result<Value> {
        self.start_index(Work::Scan, self.root.clone(), owner)
    }

    pub(super) fn refresh_index(
        self: &Arc<Self>,
        paths: &[PathBuf],
        scope: &Path,
        owner: Option<&str>,
    ) -> Result<Value> {
        let paths = index::refresh_paths(scope, paths)?;
        self.index()?;
        let excluded = crate::scoped_alias(&self.state.canonicalize()?, &self.root)
            .unwrap_or_else(|_| self.state.clone());
        ensure!(
            paths.iter().all(|p| !p.starts_with(&excluded)),
            "Axit's private index directory is excluded from refresh"
        );
        self.start_index(Work::Refresh(paths), scope.to_path_buf(), owner)
    }

    fn start_index(
        self: &Arc<Self>,
        work: Work,
        scope: PathBuf,
        owner: Option<&str>,
    ) -> Result<Value> {
        let mut thread = self
            .scan_thread
            .lock()
            .map_err(|_| anyhow::anyhow!("Index job lock unavailable"))?;
        ensure!(
            !self.stopping.load(Ordering::Acquire),
            "Server is shutting down"
        );
        if self.scanning.load(Ordering::Acquire) {
            return Ok(
                json!({"status":"already_scanning","note":"A scan or refresh is already running; poll axit_status"}),
            );
        }
        if let Some(old) = thread.take() {
            let _ = old.join();
        }
        let kind = match &work {
            Work::Scan => "scan",
            Work::Refresh(_) => "refresh",
        };
        *self
            .job
            .lock()
            .map_err(|_| anyhow::anyhow!("Index status lock unavailable"))? = Some(JobStatus {
            kind,
            scope: scope.clone(),
            owner: owner.map(str::to_owned),
            report: None,
            error: None,
            finished: None,
        });
        self.cancel.store(false, Ordering::Relaxed);
        if let Ok(mut p) = self.progress.lock() {
            *p = Progress::default();
        }
        self.scanning.store(true, Ordering::Release);
        let shared = self.clone();
        let handle = thread::Builder::new()
            .name("axit-index".into())
            .spawn(move || {
                let mut report = None;
                let result = (|| -> Result<Index> {
                    ensure!(
                        !shared.stopping.load(Ordering::Acquire),
                        "Server is shutting down"
                    );
                    match work {
                        Work::Scan => {
                            index::scan(
                                &shared.root,
                                &shared.state,
                                shared.progress.clone(),
                                shared.cancel.clone(),
                            )?;
                        }
                        Work::Refresh(paths) => {
                            report = Some(index::refresh_scoped(
                                &shared.root,
                                &shared.state,
                                &paths,
                                &scope,
                                shared.progress.clone(),
                                shared.cancel.clone(),
                            )?);
                        }
                    }
                    let index = Index::open(&crate::index_path(&shared.state)).context(
                        "Index work completed, but the resulting snapshot could not be reopened",
                    )?;
                    ensure!(
                        index.path(0)? == shared.root,
                        "The published index scope changed externally"
                    );
                    Ok(index)
                })();
                let failure = match result {
                    Ok(index) => match shared.index.write() {
                        Ok(mut value) => {
                            *value = Some(Arc::new(index));
                            None
                        }
                        Err(_) => Some(
                            "Index published, but the server snapshot lock is unavailable".into(),
                        ),
                    },
                    Err(error) => Some(format!("{error:#}")),
                };
                if let Ok(mut p) = shared.progress.lock() {
                    p.done = true;
                    if let Some(error) = &failure {
                        p.message = error.clone();
                    }
                }
                if let Ok(mut job) = shared.job.lock()
                    && let Some(job) = &mut *job
                {
                    job.report = report;
                    job.error = failure;
                    job.finished = Some(crate::unix_now());
                }
                shared.scanning.store(false, Ordering::Release);
            });
        match handle {
            Ok(handle) => {
                *thread = Some(handle);
            }
            Err(error) => {
                self.scanning.store(false, Ordering::Release);
                if let Ok(mut job) = self.job.lock()
                    && let Some(job) = &mut *job
                {
                    job.error = Some(error.to_string());
                    job.finished = Some(crate::unix_now());
                }
                if let Ok(mut p) = self.progress.lock() {
                    p.done = true;
                    p.message = error.to_string();
                }
                return Err(error).context("Could not start index worker");
            }
        }
        Ok(json!({"status":"started","operation":kind,"root":crate::text(&self.root)}))
    }

    pub(super) fn cancel_index(&self, owner: Option<&str>) -> Result<Value> {
        // Serialize with job replacement so cancellation cannot target its successor.
        let _thread = self
            .scan_thread
            .lock()
            .map_err(|_| anyhow::anyhow!("Index job lock unavailable"))?;
        if !self.scanning.load(Ordering::Acquire) {
            return Ok(json!({"status":"idle"}));
        }
        let job = self
            .job
            .lock()
            .map_err(|_| anyhow::anyhow!("Index status lock unavailable"))?;
        ensure!(
            owner.is_none() || job.as_ref().is_some_and(|j| j.owner.as_deref() == owner),
            "This index job belongs to another session"
        );
        self.cancel.store(true, Ordering::Relaxed);
        Ok(json!({"status":"cancellation_requested"}))
    }

    pub(super) fn status(&self) -> Value {
        self.status_for(None)
    }
    pub(super) fn status_for(&self, owner: Option<&str>) -> Value {
        let _thread = self.scan_thread.lock().ok();
        let running = self.scanning.load(Ordering::Acquire);
        let index = self.index().ok();
        let job = self.job.lock().ok().and_then(|j| j.clone());
        let visible = owner.is_none() || job.as_ref().is_none_or(|j| j.owner.as_deref() == owner);
        let details = job.map(|j| if visible {
            json!({"operation":j.kind,"scope":crate::text(&j.scope),"report":j.report,"error":j.error,"finished_unix":j.finished,"owned":true})
        } else { json!({"operation":j.kind,"owned":false}) });
        json!({"name":"AXIT","version":crate::VERSION,"root":crate::text(&self.root),"scanning":running,
            "progress":if visible { self.progress.lock().ok().map(|p|p.clone()) } else { None },"index_job":details,
            "entries":index.as_ref().map(|i|i.live_count()),"index_bytes":index.as_ref().map(|i|i.disk_bytes()),
            "last_full_scan_unix":index.as_ref().map(|i|i.finished),"last_refresh_unix":index.as_ref().and_then(|i|i.last_refresh()),
            "restricted_or_missing":index.as_ref().map(|i|i.errors),"volume":crate::system::volume(&self.root).ok(),"mutations_enabled":self.writes,
            "coverage_note":"Partial refresh checks only selected paths and related hardlinks. Full-scan permission gaps remain; folder totals overlap and shared blocks affect reclaimable space."})
    }
}
