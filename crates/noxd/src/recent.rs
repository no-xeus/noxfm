//! Watches the listen directories (Downloads, Documents, Projects, …) and
//! records files downloaded, edited and created there.
//!
//! - Every non-ignored folder below a listen directory gets its own
//!   non-recursive inotify watch, so build output, `.git` and hidden folders
//!   cost nothing.
//! - The log is persisted to `~/.local/state/noxfm/recent.log`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use notify::event::{AccessKind, AccessMode, CreateKind, ModifyKind, RenameMode};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use noxfm_core::places::{self, Place};
use noxfm_core::recent::{self, RecentLog};
use noxfm_proto::{Event, RecentKind};
use tokio::sync::mpsc;

use crate::Daemon;

const LOG_CAP: usize = 2000;
const MAX_WATCHES: usize = 20_000;
const SAVE_EVERY: Duration = Duration::from_secs(10);
/// Windows hear about new recent items at most this often.
const NOTIFY_EVERY: Duration = Duration::from_secs(1);

pub struct Recent {
    pub places: Vec<Place>,
    pub log: Mutex<RecentLog>,
    file: PathBuf,
    dirty: AtomicBool,
    watcher: Mutex<Option<RecommendedWatcher>>,
    watched: Mutex<usize>,
}

impl Recent {
    pub fn new(places: Vec<Place>, file: PathBuf) -> Self {
        let log = std::fs::File::open(&file)
            .map(|f| RecentLog::load(std::io::BufReader::new(f), LOG_CAP))
            .unwrap_or_else(|_| RecentLog::new(LOG_CAP));
        Recent {
            places,
            log: Mutex::new(log),
            file,
            dirty: AtomicBool::new(false),
            watcher: Mutex::new(None),
            watched: Mutex::new(0),
        }
    }

    /// The user's real folders and log file.
    pub fn for_user() -> Self {
        let home = noxfm_core::complete::home_dir();
        let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config"));
        let user_dirs = std::fs::read_to_string(config.join("user-dirs.dirs")).ok();
        let state = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/state"));
        Recent::new(places::listen_dirs(&home, user_dirs.as_deref()), state.join("noxfm/recent.log"))
    }

    fn place_of(&self, path: &Path) -> Option<&Place> {
        self.places.iter().filter(|p| path.starts_with(&p.path)).max_by_key(|p| p.path.as_os_str().len())
    }

    /// Watches `dir` and every non-ignored folder below it.
    fn watch_tree(&self, dir: &Path, root: &Path) {
        let mut guard = self.watcher.lock().unwrap();
        let Some(w) = guard.as_mut() else { return };
        let walker = walkdir::WalkDir::new(dir)
            .follow_links(false)
            .same_file_system(true)
            .into_iter()
            .filter_entry(|e| e.file_type().is_dir() && (e.path() == root || !recent::is_ignored(e.path(), root)));
        for entry in walker.filter_map(Result::ok) {
            let mut n = self.watched.lock().unwrap();
            if *n >= MAX_WATCHES {
                tracing::warn!("recent: watch limit reached, not watching {}", entry.path().display());
                return;
            }
            if w.watch(entry.path(), RecursiveMode::NonRecursive).is_ok() {
                *n += 1;
            }
        }
    }

    fn record(&self, path: PathBuf, kind: RecentKind) -> bool {
        let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        let added = self.log.lock().unwrap().record(path, kind, at);
        if added {
            self.dirty.store(true, Ordering::Relaxed);
        }
        added
    }

    fn forget(&self, path: &Path) {
        self.log.lock().unwrap().forget(path);
        self.dirty.store(true, Ordering::Relaxed);
    }

    fn save(&self) {
        if !self.dirty.swap(false, Ordering::Relaxed) {
            return;
        }
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(self.file.parent().expect("log file has a parent"))?;
            let tmp = self.file.with_extension("tmp");
            let mut f = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
            self.log.lock().unwrap().save(&mut f)?;
            std::io::Write::flush(&mut f)?;
            std::fs::rename(tmp, &self.file)
        };
        if let Err(e) = write() {
            tracing::warn!(%e, "could not save the recent log");
        }
    }

    /// Turns one filesystem event into log entries. Returns true if the log changed.
    fn on_event(&self, ev: notify::Event) -> bool {
        let mut changed = false;
        match ev.kind {
            EventKind::Create(CreateKind::Folder) => {
                for p in &ev.paths {
                    if let Some(place) = self.place_of(p).filter(|pl| !recent::is_ignored(p, &pl.path)) {
                        let root = place.path.clone();
                        self.watch_tree(p, &root);
                    }
                }
            }
            EventKind::Create(_) => {
                for p in ev.paths.iter().filter(|p| p.is_file()) {
                    changed |= self.new_file(p, false);
                }
            }
            // An in-progress download renamed to its final name, or a file moved in.
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)) if ev.paths.len() == 2 => {
                let (from, to) = (&ev.paths[0], &ev.paths[1]);
                self.forget(from);
                if to.is_dir() {
                    if let Some(place) = self.place_of(to) {
                        let root = place.path.clone();
                        self.watch_tree(to, &root);
                    }
                } else if recent::is_partial(from) {
                    changed |= self.new_file(to, true);
                } else if let Some(place) = self.place_of(to).filter(|pl| !recent::is_ignored(to, &pl.path)) {
                    // Editors save by writing a temp file and renaming it over the original.
                    let kind = if recent::is_ignored(from, &place.path) || from.parent() == to.parent() {
                        RecentKind::Modified
                    } else {
                        places::kind_for_new_file(place)
                    };
                    changed |= self.record(to.clone(), kind);
                }
            }
            EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
                for p in ev.paths.iter().filter(|p| p.is_file()) {
                    changed |= self.new_file(p, false);
                }
            }
            EventKind::Modify(ModifyKind::Name(RenameMode::From)) | EventKind::Remove(_) => {
                for p in &ev.paths {
                    self.forget(p);
                }
                changed = true;
            }
            EventKind::Access(AccessKind::Close(AccessMode::Write)) => {
                for p in &ev.paths {
                    if self.place_of(p).is_some_and(|pl| !recent::is_ignored(p, &pl.path)) {
                        changed |= self.record(p.clone(), RecentKind::Modified);
                    }
                }
            }
            _ => {}
        }
        changed
    }

    fn new_file(&self, p: &Path, finished_download: bool) -> bool {
        let Some(place) = self.place_of(p) else { return false };
        if recent::is_ignored(p, &place.path) {
            return false;
        }
        let kind = if finished_download { RecentKind::Downloaded } else { places::kind_for_new_file(place) };
        self.record(p.to_path_buf(), kind)
    }
}

impl Daemon {
    /// Starts watching the listen directories (call once, from a runtime).
    pub fn start_recent(self: &Arc<Self>) {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                let _ = tx.send(ev);
            }
        });
        match watcher {
            Ok(w) => *self.recent.watcher.lock().unwrap() = Some(w),
            Err(e) => {
                tracing::warn!(%e, "recent: can't watch folders");
                return;
            }
        }

        let me = self.clone();
        tokio::task::spawn_blocking(move || {
            for place in &me.recent.places {
                me.recent.watch_tree(&place.path, &place.path);
            }
            tracing::info!(folders = *me.recent.watched.lock().unwrap(), "recent: watching");
        });

        let me = self.clone();
        tokio::spawn(async move {
            let mut save = tokio::time::interval(SAVE_EVERY);
            let mut pending_notify = false;
            let mut notify = tokio::time::interval(NOTIFY_EVERY);
            loop {
                tokio::select! {
                    ev = rx.recv() => {
                        let Some(ev) = ev else { break };
                        let me2 = me.clone();
                        pending_notify |= tokio::task::spawn_blocking(move || me2.recent.on_event(ev)).await.unwrap_or(false);
                    }
                    _ = notify.tick() => {
                        if std::mem::take(&mut pending_notify) {
                            me.hub.broadcast(Event::RecentChanged);
                        }
                    }
                    _ = save.tick() => {
                        let me2 = me.clone();
                        let _ = tokio::task::spawn_blocking(move || me2.recent.save()).await;
                    }
                }
            }
        });
    }

    pub fn save_recent(&self) {
        self.recent.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{EventAttributes, RemoveKind};

    fn ev(kind: EventKind, paths: &[&Path]) -> notify::Event {
        notify::Event { kind, paths: paths.iter().map(|p| p.to_path_buf()).collect(), attrs: EventAttributes::new() }
    }

    #[test]
    fn classifies_events() {
        let t = tempfile::tempdir().unwrap();
        let (dl, proj) = (t.path().join("Downloads"), t.path().join("Projects"));
        std::fs::create_dir_all(&dl).unwrap();
        std::fs::create_dir_all(proj.join("app/target")).unwrap();
        let places = vec![
            Place { name: "Downloads".into(), path: dl.clone(), downloads: true },
            Place { name: "Projects".into(), path: proj.clone(), downloads: false },
        ];
        let r = Recent::new(places, t.path().join("recent.log"));
        let touch = |p: &Path| std::fs::write(p, "x").unwrap();

        // Browser download: .part, then renamed to the final name.
        let (part, iso) = (dl.join("big.iso.part"), dl.join("big.iso"));
        touch(&part);
        assert!(!r.on_event(ev(EventKind::Create(CreateKind::File), &[&part])));
        std::fs::rename(&part, &iso).unwrap();
        assert!(r.on_event(ev(EventKind::Modify(ModifyKind::Name(RenameMode::Both)), &[&part, &iso])));

        // curl-style download straight to the final name.
        let pdf = dl.join("paper.pdf");
        touch(&pdf);
        assert!(r.on_event(ev(EventKind::Create(CreateKind::File), &[&pdf])));

        // New source file, then an edit long after.
        let src = proj.join("app/main.rs");
        touch(&src);
        assert!(r.on_event(ev(EventKind::Create(CreateKind::File), &[&src])));

        // Build output never counts.
        let obj = proj.join("app/target/main.o");
        touch(&obj);
        assert!(!r.on_event(ev(EventKind::Create(CreateKind::File), &[&obj])));
        assert!(!r.on_event(ev(EventKind::Access(AccessKind::Close(AccessMode::Write)), &[&obj])));

        let kinds = |k| r.log.lock().unwrap().query(Some(k), 10, |_| true).into_iter().map(|i| i.path).collect::<Vec<_>>();
        let mut downloads = kinds(RecentKind::Downloaded);
        downloads.sort();
        assert_eq!(downloads, [iso.clone(), pdf.clone()]);
        assert_eq!(kinds(RecentKind::Created), std::slice::from_ref(&src));

        // Deleting forgets.
        std::fs::remove_file(&pdf).unwrap();
        r.on_event(ev(EventKind::Remove(RemoveKind::File), &[&pdf]));
        assert_eq!(kinds(RecentKind::Downloaded), [iso]);

        // Persisted and reloaded.
        r.save();
        let again = Recent::new(Vec::new(), t.path().join("recent.log"));
        assert_eq!(again.log.lock().unwrap().query(None, 10, |_| true).len(), 2);
    }
}
