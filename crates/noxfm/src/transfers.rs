//! Copy/move jobs as seen by a window: the state behind the transfers tab.
//!
//! The daemon broadcasts progress for every job to every window, so each
//! browser window shows all running transfers.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use cosmic::iced::{Alignment, Length};
use cosmic::prelude::*;
use cosmic::widget;
use noxfm_proto::{JobKind, TransferStatus};

use crate::fmt;

/// Successful jobs stay listed this long so the result can be seen.
const LINGER: Duration = Duration::from_secs(3);

struct Job {
    status: TransferStatus,
    finished: Option<(Result<(), String>, Instant)>,
}

impl Job {
    /// Average bytes per second. Timed by the daemon: events can reach us in
    /// bursts, so arrival times say nothing about speed.
    fn rate(&self) -> f64 {
        match self.status.elapsed_ms {
            0 => 0.0,
            ms => self.status.done_bytes as f64 * 1000.0 / ms as f64,
        }
    }

    fn fraction(&self) -> f32 {
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

    pub fn view<'a, M: Clone + 'static>(
        &'a self,
        cancel: impl Fn(u64) -> M,
        dismiss: impl Fn(u64) -> M,
    ) -> Element<'a, M> {
        let list = self.jobs.values().fold(widget::column::with_capacity(self.jobs.len()).spacing(12), |c, j| {
            c.push(job_view(j, cancel(j.status.id), dismiss(j.status.id)))
        });
        widget::scrollable(list).into()
    }
}

fn job_view<M: Clone + 'static>(j: &Job, cancel: M, dismiss: M) -> Element<'_, M> {
    let s = &j.status;
    let done = j.finished.is_some();
    let verb = match (s.kind, &j.finished) {
        (_, Some((Err(_), _))) => "Failed",
        (JobKind::Copy, _) => if done { "Copied" } else { "Copying" },
        (JobKind::Move, _) => if done { "Moved" } else { "Moving" },
        (JobKind::Compress, _) => if done { "Compressed" } else { "Compressing" },
        (JobKind::Extract, _) => if done { "Extracted" } else { "Extracting" },
    };
    let items = if s.items == 1 { "1 item".to_owned() } else { format!("{} items", s.items) };
    let dest = s.dest.file_name().map_or_else(|| s.dest.display().to_string(), |n| n.to_string_lossy().into_owned());
    let prep = if s.kind == JobKind::Compress { "into" } else { "to" };
    let title = widget::text::heading(format!("{verb} {items} {prep} {dest}"));

    let detail = match &j.finished {
        Some((Err(e), _)) => e.clone(),
        Some((Ok(()), _)) => fmt::size(s.total_bytes),
        None if s.counting => "Preparing…".into(),
        None => {
            let mut d = format!("{} of {}", fmt::size(s.done_bytes), fmt::size(s.total_bytes));
            let rate = j.rate();
            if rate > 1.0 {
                let left = (s.total_bytes.saturating_sub(s.done_bytes) as f64 / rate).ceil();
                d += &format!("  ·  {}/s  ·  {} left", fmt::size(rate as u64), fmt::duration(left as u64));
            }
            if let Some(name) = s.current.as_ref().and_then(|c| c.file_name()) {
                d += &format!("  ·  {}", name.to_string_lossy());
            }
            d
        }
    };

    let bar = widget::progress_bar::linear::Linear::new().progress(j.fraction()).girth(6.0).width(Length::Fill);

    let action = match &j.finished {
        None => widget::button::standard("Cancel").on_press(cancel),
        Some(_) => widget::button::standard("Dismiss").on_press(dismiss),
    };

    widget::row::with_capacity(2)
        .spacing(12)
        .align_y(Alignment::Center)
        .push(
            widget::column::with_capacity(3)
                .spacing(4)
                .push(title)
                .push(bar)
                .push(widget::text::caption(detail))
                .width(Length::Fill),
        )
        .push(action)
        .into()
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
