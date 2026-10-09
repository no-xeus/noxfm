//! Copy/move/zip jobs as seen by a window, and how they're described.
//!
//! The daemon broadcasts progress for every job to every window, so each
//! browser window shows all running transfers.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use noxfm_proto::{JobKind, TransferStatus};

use crate::fmt;

/// Successful jobs stay listed this long so the result can be seen.
const LINGER: Duration = Duration::from_secs(3);

pub struct Job {
    pub status: TransferStatus,
    pub finished: Option<(Result<(), String>, Instant)>,
}

impl Job {
    /// Average bytes per second. Timed by the daemon: events can reach us in
    /// bursts, so arrival times say nothing about speed.
    pub fn rate(&self) -> f64 {
        match self.status.elapsed_ms {
            0 => 0.0,
            ms => self.status.done_bytes as f64 * 1000.0 / ms as f64,
        }
    }

    pub fn fraction(&self) -> f32 {
        let s = &self.status;
        match s.total_bytes {
            0 if self.finished.is_some() => 1.0,
            0 => 0.0,
            t => s.done_bytes as f32 / t as f32,
        }
    }
}

#[derive(Default)]
pub struct Jobs {
    jobs: BTreeMap<u64, Job>,
}

pub struct Summary {
    pub running: usize,
    pub failed: usize,
    /// Over all running jobs.
    pub fraction: f32,
}

impl Jobs {
    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    /// Replaces everything with the daemon's view (on (re)connect).
    pub fn snapshot(&mut self, list: Vec<TransferStatus>) {
        self.jobs.retain(|_, j| j.finished.is_some());
        for s in list {
            self.progress(s);
        }
    }

    pub fn progress(&mut self, status: TransferStatus) {
        self.jobs
            .entry(status.id)
            .and_modify(|j| j.status = status.clone())
            .or_insert(Job { status, finished: None });
    }

    /// A job we never saw (it finished before we connected) is ignored.
    pub fn done(&mut self, id: u64, error: Option<String>) {
        if let Some(j) = self.jobs.get_mut(&id) {
            if error.is_none() {
                j.status.done_bytes = j.status.total_bytes;
            }
            j.finished = Some((error.map_or(Ok(()), Err), Instant::now()));
        }
    }

    pub fn dismiss(&mut self, id: u64) {
        self.jobs.remove(&id);
    }

    /// Drops successful jobs once they've lingered. Failures stay until dismissed.
    pub fn tick(&mut self) {
        self.jobs.retain(|_, j| !matches!(&j.finished, Some((Ok(()), at)) if at.elapsed() >= LINGER));
    }

    pub fn iter(&self) -> impl Iterator<Item = &Job> {
        self.jobs.values()
    }

    pub fn summary(&self) -> Summary {
        let running: Vec<_> = self.jobs.values().filter(|j| j.finished.is_none()).collect();
        let (done, total) = running
            .iter()
            .fold((0u64, 0u64), |(d, t), j| (d + j.status.done_bytes, t + j.status.total_bytes));
        Summary {
            running: running.len(),
            failed: self.jobs.values().filter(|j| matches!(j.finished, Some((Err(_), _)))).count(),
            fraction: if total == 0 { 0.0 } else { done as f32 / total as f32 },
        }
    }
}

impl Job {
    /// "Copying 3 items to Documents"
    pub fn title(&self) -> String {
        let s = &self.status;
        let done = self.finished.is_some();
        let verb = match (s.kind, &self.finished) {
            (_, Some((Err(_), _))) => "Failed",
            (JobKind::Copy, _) => if done { "Copied" } else { "Copying" },
            (JobKind::Move, _) => if done { "Moved" } else { "Moving" },
            (JobKind::Compress, _) => if done { "Compressed" } else { "Compressing" },
            (JobKind::Extract, _) => if done { "Extracted" } else { "Extracting" },
        };
        let items = if s.items == 1 { "1 item".to_owned() } else { format!("{} items", s.items) };
        let dest = s.dest.file_name().map_or_else(|| s.dest.display().to_string(), |n| n.to_string_lossy().into_owned());
        let prep = if s.kind == JobKind::Compress { "into" } else { "to" };
        format!("{verb} {items} {prep} {dest}")
    }

    /// Progress, speed and time left; the error; or the size when done.
    pub fn detail(&self) -> String {
        let s = &self.status;
        match &self.finished {
            Some((Err(e), _)) => e.clone(),
            Some((Ok(()), _)) => fmt::size(s.total_bytes),
            None if s.counting => "Preparing…".into(),
            None => {
                let mut d = format!("{} of {}", fmt::size(s.done_bytes), fmt::size(s.total_bytes));
                let rate = self.rate();
                if rate > 1.0 {
                    let left = (s.total_bytes.saturating_sub(s.done_bytes) as f64 / rate).ceil();
                    d += &format!("  ·  {}/s  ·  {} left", fmt::size(rate as u64), fmt::duration(left as u64));
                }
                if let Some(name) = s.current.as_ref().and_then(|c| c.file_name()) {
                    d += &format!("  ·  {}", name.to_string_lossy());
                }
                d
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(id: u64, done: u64, total: u64) -> TransferStatus {
        TransferStatus {
            id,
            kind: JobKind::Copy,
            items: 1,
            counting: false,
            done_bytes: done,
            total_bytes: total,
            elapsed_ms: 1000,
            current: None,
            dest: "/d".into(),
        }
    }

    #[test]
    fn lifecycle() {
        let mut jobs = Jobs::default();
        jobs.progress(status(1, 25, 100));
        jobs.progress(status(2, 75, 100));
        let s = jobs.summary();
        assert_eq!((s.running, s.failed, s.fraction), (2, 0, 0.5));

        jobs.done(1, None);
        jobs.done(2, Some("boom".into()));
        jobs.done(99, None); // never seen: ignored
        let s = jobs.summary();
        assert_eq!((s.running, s.failed), (0, 1));

        jobs.tick();
        assert_eq!(jobs.jobs.len(), 2, "finished jobs linger");
        jobs.jobs.get_mut(&1).unwrap().finished.as_mut().unwrap().1 -= LINGER;
        jobs.tick();
        assert_eq!(jobs.jobs.keys().copied().collect::<Vec<_>>(), [2], "failures stay until dismissed");
        jobs.dismiss(2);
        assert!(jobs.is_empty());
    }
}
