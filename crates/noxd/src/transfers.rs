//! Long-running jobs (copy, move, compress, extract) and their progress.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use noxfm_core::transfer;
use noxfm_proto::{Event, JobKind, TransferOp, TransferStatus};

use crate::Daemon;
use crate::undo::Action;

/// Progress events per job are capped at about 10 per second.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

struct Job {
    status: TransferStatus,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
pub struct Transfers {
    next_id: AtomicU64,
    jobs: Mutex<HashMap<u64, Job>>,
}

impl Transfers {
    pub fn list(&self) -> Vec<TransferStatus> {
        let mut v: Vec<_> = self.jobs.lock().unwrap().values().map(|j| j.status.clone()).collect();
        v.sort_by_key(|s| s.id);
        v
    }

    pub fn cancel(&self, id: u64) -> bool {
        match self.jobs.lock().unwrap().get(&id) {
            Some(j) => {
                j.cancel.store(true, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    fn update(&self, id: u64, f: impl FnOnce(&mut TransferStatus)) -> Option<TransferStatus> {
        let mut jobs = self.jobs.lock().unwrap();
        let job = jobs.get_mut(&id)?;
        f(&mut job.status);
        Some(job.status.clone())
    }
}

/// Reports a job's progress, at most every [`PROGRESS_EVERY`].
pub struct Progress<'a> {
    daemon: &'a Daemon,
    id: u64,
    started: Instant,
    last: Instant,
}

impl Progress<'_> {
    /// Sizing is done; copying starts now.
    pub fn total(&mut self, bytes: u64) {
        self.started = Instant::now();
        if let Some(s) = self.daemon.transfers.update(self.id, |s| {
            s.total_bytes = bytes;
            s.counting = false;
        }) {
            self.daemon.hub.broadcast(Event::TransferProgress(s));
        }
    }

    pub fn advance(&mut self, done: u64, current: &Path) {
        if self.last.elapsed() < PROGRESS_EVERY {
            return;
        }
        self.last = Instant::now();
        let elapsed = self.started.elapsed().as_millis() as u64;
        if let Some(s) = self.daemon.transfers.update(self.id, |s| {
            s.done_bytes = done;
            s.elapsed_ms = elapsed;
            s.current = Some(current.to_path_buf());
        }) {
            self.daemon.hub.broadcast(Event::TransferProgress(s));
        }
    }
}

/// The work of a job: runs on a blocking thread, returns what undoes it.
pub type Work = Box<dyn FnOnce(&mut Progress<'_>, &AtomicBool) -> anyhow::Result<Option<Action>> + Send>;

impl Daemon {
    pub fn start_transfer(self: &Arc<Self>, op: TransferOp, sources: Vec<PathBuf>, dest: PathBuf) -> anyhow::Result<u64> {
        anyhow::ensure!(!sources.is_empty(), "nothing to transfer");
        anyhow::ensure!(dest.is_absolute() && sources.iter().all(|s| s.is_absolute()), "paths must be absolute");
        let srcs = sources.clone();
        let to = dest.clone();
        let work: Work = Box::new(move |p, cancel| {
            p.total(transfer::total_bytes(&srcs, cancel)?);
            let pairs = transfer::run(op, &srcs, &to, cancel, &mut |done, current| p.advance(done, current))?;
            Ok((!pairs.is_empty()).then(|| match op {
                TransferOp::Copy => Action::Copied(pairs.into_iter().map(|(_, t)| t).collect()),
                TransferOp::Move => Action::Moved(pairs),
            }))
        });
        Ok(self.start_job(op.into(), sources.len(), dest, work))
    }

    /// Runs `work` as a job shown in every window's transfers tab.
    pub fn start_job(self: &Arc<Self>, kind: JobKind, items: usize, dest: PathBuf, work: Work) -> u64 {
        let id = self.transfers.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let cancel = Arc::new(AtomicBool::new(false));
        let status = TransferStatus {
            id,
            kind,
            items: items as u32,
            counting: true,
            done_bytes: 0,
            total_bytes: 0,
            elapsed_ms: 0,
            current: None,
            dest,
        };
        self.transfers.jobs.lock().unwrap().insert(id, Job { status: status.clone(), cancel: cancel.clone() });
        self.hub.broadcast(Event::TransferProgress(status));

        let me = self.clone();
        tokio::task::spawn_blocking(move || {
            let now = Instant::now();
            let mut progress = Progress { daemon: &me, id, started: now, last: now };
            let result = work(&mut progress, &cancel);
            me.transfers.jobs.lock().unwrap().remove(&id);
            let error = match result {
                Ok(undo) => {
                    if let Some(a) = undo {
                        me.push_undo(a);
                    }
                    None
                }
                Err(e) => Some(e.to_string()),
            };
            tracing::info!(id, ?kind, ?error, "job finished");
            me.hub.broadcast(Event::TransferDone { id, error });
        });
        id
    }
}
