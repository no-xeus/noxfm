//! The noxfm daemon. `main.rs` binds the socket; everything else lives here
//! so integration tests can run a daemon in-process.

pub mod apps;
pub mod archive;
pub mod health;
pub mod hub;
pub mod mounts;
pub mod ops;
pub mod recent;
pub mod sizes;
pub mod supervisor;
pub mod transfers;
pub mod trash;
pub mod undo;
pub mod update;
pub mod watcher;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use noxfm_proto::{
    ClientMsg, Hello, PROTOCOL_VERSION, Request, Response, ServerMsg, Welcome, read_frame,
    write_frame,
};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

use hub::{ClientId, Hub};
use noxfm_proto::{Entry, EntryKind, Event};
use sizes::Sizes;
use supervisor::Supervisor;
use watcher::FsWatcher;

/// A size walk for a directory nobody looks at anymore is abandoned, but only
/// after this long: windows subscribe right after their first listing arrives.
const WALK_GRACE: Duration = Duration::from_secs(2);

pub struct Daemon {
    pub hub: Hub,
    pub supervisor: Supervisor,
    pub sizes: Sizes,
    pub transfers: transfers::Transfers,
    pub apps: apps::Apps,
    pub recent: recent::Recent,
    pub mounts: mounts::Mounts,
    pub undo: undo::Undo,
    pub pins: ops::Pins,
    pub collapsed: ops::CollapsedItems,
    health_dismissed: std::sync::atomic::AtomicBool,
    pub update: update::SelfUpdate,
    /// Purge age for the trash; `None` = don't purge or watch the trash (tests).
    trash_days: Option<u32>,
    /// Thumbnails being generated at once.
    thumb_permits: tokio::sync::Semaphore,
    watcher: FsWatcher,
    changes: Mutex<Option<mpsc::UnboundedReceiver<PathBuf>>>,
}

impl Daemon {
    pub fn new(socket: PathBuf) -> Arc<Self> {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| noxfm_core::complete::home_dir().join(".config"));
        let conf = std::fs::read_to_string(config.join("noxfm/noxfm.conf")).ok();
        Self::build(
            Supervisor::new(socket),
            Sizes::for_user(),
            recent::Recent::for_user(),
            mounts::Mounts::for_user(),
            ops::Pins::for_user(),
            ops::CollapsedItems::for_user(),
            Some(trash::configured_days(conf.as_deref())),
        )
    }

    /// A daemon that never spawns windows nor watches the user's folders (for tests).
    pub fn headless(socket: PathBuf) -> Arc<Self> {
        Self::build(
            Supervisor::headless(socket),
            Sizes::default(),
            recent::Recent::new(Vec::new(), "/dev/null".into()),
            mounts::Mounts::disabled(),
            ops::Pins::at(std::env::temp_dir().join(format!("noxd-test-pins-{}", std::process::id()))),
            ops::CollapsedItems::at(std::env::temp_dir().join(format!("noxd-test-collapsed-{}", std::process::id()))),
            None,
        )
    }

    fn build(
        supervisor: Supervisor,
        sizes: Sizes,
        recent: recent::Recent,
        mounts: mounts::Mounts,
        pins: ops::Pins,
        collapsed: ops::CollapsedItems,
        trash_days: Option<u32>,
    ) -> Arc<Self> {
        let (watcher, changes) = FsWatcher::new().expect("inotify unavailable");
        Arc::new(Daemon {
            hub: Hub::default(),
            supervisor,
            sizes,
            transfers: Default::default(),
            apps: Default::default(),
            recent,
            mounts,
            undo: Default::default(),
            pins,
            collapsed,
            health_dismissed: Default::default(),
            update: Default::default(),
            trash_days,
            thumb_permits: tokio::sync::Semaphore::new(3),
            watcher,
            changes: Mutex::new(Some(changes)),
        })
    }

    pub async fn serve(self: Arc<Self>, listener: UnixListener) -> std::io::Result<()> {
        if let Some(rx) = self.changes.lock().unwrap().take() {
            let me = self.clone();
            tokio::spawn(watcher::debounce(rx, move |paths| me.on_changes(paths)));
            self.start_recent();
            self.start_sizes();
            self.start_mounts();
            self.start_trash();
            // Once mounts had a moment to connect, say what's missing.
            if self.trash_days.is_some() {
                self.start_self_update();
                let me = self.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    let udisks = me.mounts.connected();
                    if let Ok(checks) = tokio::task::spawn_blocking(move || health::run(udisks)).await {
                        for c in checks.iter().filter(|c| !c.ok) {
                            tracing::warn!(check = %c.name, "{} unavailable: {}", c.feature, c.detail);
                        }
                    }
                });
            }
        }
        loop {
            let (stream, _) = listener.accept().await?;
            let me = self.clone();
            tokio::spawn(async move {
                if let Err(e) = me.connection(stream).await {
                    tracing::debug!(%e, "connection ended");
                }
            });
        }
    }

    async fn connection(self: Arc<Self>, stream: UnixStream) -> anyhow::Result<()> {
        let (mut r, mut w) = stream.into_split();
        let Some(hello) = read_frame::<_, Hello>(&mut r).await? else { return Ok(()) };
        if !hello.compatible() {
            // Usually a window from a newer install: see whether we were updated too.
            self.update.check_now.notify_one();
            tracing::warn!(role = ?hello.role, "client built from a different protocol revision");
            let reply = Welcome::VersionMismatch { daemon_version: PROTOCOL_VERSION };
            write_frame(&mut w, &reply).await?;
            return Ok(());
        }
        write_frame(&mut w, &Welcome::Ok { daemon_version: PROTOCOL_VERSION }).await?;
        self.supervisor.note_display_env(&hello.display_env);

        let (tx, mut rx) = mpsc::unbounded_channel::<ServerMsg>();
        let id = self.hub.register(hello.role, tx.clone());
        tracing::debug!(id, role = ?hello.role, "client connected");

        let writer = tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if write_frame(&mut w, &msg).await.is_err() {
                    break;
                }
            }
        });

        let result = async {
            while let Some(ClientMsg { id: req_id, req }) = read_frame(&mut r).await? {
                let me = self.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    let result = me.handle(id, req).await.map_err(|e| format!("{e:#}"));
                    let _ = tx.send(ServerMsg::Reply { id: req_id, result });
                });
            }
            anyhow::Ok(())
        }
        .await;

        for orphan in self.hub.unregister(id) {
            self.watcher.unwatch(&orphan);
        }
        writer.abort();
        result
    }

    async fn handle(self: &Arc<Self>, client: ClientId, req: Request) -> anyhow::Result<Response> {
        match req {
            Request::ListDir { path, watch } => {
                let me = self.clone();
                let (fs, mut entries) = blocking({
                    let path = path.clone();
                    move || {
                        let fs = noxfm_core::fskind::FsKind::of(&path).ok().map(|k| k.name().to_owned());
                        let mut entries = noxfm_core::list_dir(&path)?;
                        me.apps.annotate(&mut entries);
                        Ok((fs, entries))
                    }
                })
                .await?;
                if watch && self.hub.watch(client, path.clone()) {
                    self.watcher.watch(&path);
                }
                self.fill_sizes(&path, &mut entries);
                Ok(Response::Dir { path, fs, entries })
            }
            Request::Stat { path } => {
                let me = self.clone();
                let entry = blocking(move || {
                    let mut e = [noxfm_core::stat_entry(&path)?];
                    me.apps.annotate(&mut e);
                    let [e] = e;
                    Ok(e)
                })
                .await?;
                Ok(Response::Entry(entry))
            }
            Request::OpenWith { path, app } => {
                let mime = blocking({
                    let path = path.clone();
                    move || Ok(noxfm_core::stat_entry(&path)?.mime.unwrap_or_default())
                })
                .await?;
                self.apps.open(&path, &mime, app.as_deref())?;
                Ok(Response::Ok)
            }
            Request::Thumbnail { path } => {
                let _permit = self.thumb_permits.acquire().await?;
                let cache_path = tokio::task::spawn_blocking(move || {
                    let mime = noxfm_core::filetype::detect(&path).mime;
                    if !noxfm_core::thumbnail::supported(&mime) {
                        return None;
                    }
                    let root = noxfm_core::thumbnail::cache_root();
                    noxfm_core::thumbnail::get_or_create(&root, &path, &mime, noxfm_core::thumbnail::Size::Large)
                        .map_err(|e| tracing::debug!(path = %path.display(), %e, "no thumbnail"))
                        .ok()
                })
                .await?;
                Ok(Response::Thumbnail { cache_path })
            }
            Request::Complete { prefix } => {
                let home = noxfm_core::complete::home_dir();
                let found = tokio::task::spawn_blocking(move || {
                    noxfm_core::complete::complete_dirs(&prefix, &home)
                })
                .await?;
                Ok(Response::Completions(found))
            }
            Request::Subscribe { path } => {
                if self.hub.watch(client, path.clone()) {
                    self.watcher.watch(&path);
                }
                Ok(Response::Ok)
            }
            Request::Unsubscribe { path } => {
                if self.hub.unwatch(client, &path) {
                    self.watcher.unwatch(&path);
                }
                Ok(Response::Ok)
            }
            Request::OpenWindow { path, view, layout } => {
                self.supervisor.open_browser(path.as_deref(), view, layout)?;
                Ok(Response::Ok)
            }
            Request::Transfer { op, sources, dest } => {
                Ok(Response::TransferStarted { id: self.start_transfer(op, sources, dest)? })
            }
            Request::CancelTransfer { id } => {
                anyhow::ensure!(self.transfers.cancel(id), "no transfer {id}");
                Ok(Response::Ok)
            }
            Request::ListTransfers => Ok(Response::Transfers(self.transfers.list())),
            Request::ListDevices => Ok(Response::Devices(self.mounts.devices())),
            Request::ForgetRecent { path } => {
                self.recent.log.lock().unwrap().forget(&path);
                self.hub.broadcast(Event::RecentChanged);
                Ok(Response::Ok)
            }
            Request::Rename { path, new_name } => {
                let from = path.clone();
                let to = blocking(move || ops::rename(&path, &new_name).map_err(std::io::Error::other)).await?;
                if to != from {
                    self.push_undo(undo::Action::Renamed { from: from.clone(), to: to.clone() });
                    self.hub.broadcast(Event::Moved(vec![(from, to.clone())]));
                }
                Ok(Response::Path(to))
            }
            Request::Create { dir, kind } => {
                let made = blocking(move || ops::create(&dir, &kind).map_err(std::io::Error::other)).await?;
                self.push_undo(undo::Action::Created(made.clone()));
                Ok(Response::Path(made))
            }
            Request::Symlink { targets, dir } => {
                let links = blocking(move || ops::symlink(&targets, &dir).map_err(std::io::Error::other)).await?;
                self.push_undo(undo::Action::Linked(links));
                Ok(Response::Ok)
            }
            Request::Trash { paths } => {
                let ids = blocking(move || trash::trash(&paths).map_err(std::io::Error::other)).await?;
                self.push_undo(undo::Action::Trashed(ids));
                self.trash_changed();
                Ok(Response::Ok)
            }
            Request::DeleteForever { paths } => {
                blocking(move || trash::delete_forever(&paths).map_err(std::io::Error::other)).await?;
                Ok(Response::Ok)
            }
            Request::ListTrash => {
                let me = self.clone();
                let mut items = blocking(|| trash::list().map_err(std::io::Error::other)).await?;
                let mut entries: Vec<Entry> = items.iter().map(|(_, e)| e.clone()).collect();
                tokio::task::spawn_blocking(move || {
                    me.apps.annotate(&mut entries);
                    entries
                })
                .await?
                .into_iter()
                .zip(items.iter_mut())
                .for_each(|(annotated, (_, e))| *e = annotated);
                if let Some(days) = self.trash_days {
                    for (t, _) in &mut items {
                        t.purge_at = Some(t.deleted_at + i64::from(days) * 86_400);
                    }
                }
                Ok(Response::TrashItems(items))
            }
            Request::RestoreTrash { ids } => {
                blocking(move || trash::restore(&ids).map_err(std::io::Error::other)).await?;
                self.trash_changed();
                Ok(Response::Ok)
            }
            Request::PurgeTrash { ids } => {
                blocking(move || trash::purge(&ids).map_err(std::io::Error::other)).await?;
                self.trash_changed();
                Ok(Response::Ok)
            }
            Request::EmptyTrash => {
                blocking(|| trash::empty().map_err(std::io::Error::other)).await?;
                self.trash_changed();
                Ok(Response::Ok)
            }
            Request::Undo => {
                let me = self.clone();
                let (label, moved) = blocking(move || me.undo.undo().map_err(std::io::Error::other)).await?;
                if !moved.is_empty() {
                    self.hub.broadcast(Event::Moved(moved));
                }
                self.hub.broadcast(Event::UndoChanged(self.undo.label()));
                self.trash_changed();
                Ok(Response::Label(Some(label)))
            }
            Request::UndoLabel => Ok(Response::Label(self.undo.label())),
            Request::SidebarCollapsed => Ok(Response::Keys(self.collapsed.list())),
            Request::Health => {
                let udisks = self.mounts.connected();
                let checks = tokio::task::spawn_blocking(move || health::run(udisks)).await?;
                Ok(Response::Health { checks, dismissed: self.health_dismissed.load(std::sync::atomic::Ordering::Relaxed) })
            }
            Request::DismissHealth => {
                self.health_dismissed.store(true, std::sync::atomic::Ordering::Relaxed);
                Ok(Response::Ok)
            }
            Request::SetSidebarCollapsed { key, collapsed } => {
                self.collapsed.set(&key, collapsed)?;
                self.hub.broadcast(Event::PlacesChanged);
                Ok(Response::Ok)
            }
            Request::Compress { sources, dest_dir } => {
                anyhow::ensure!(!sources.is_empty(), "nothing to compress");
                let dest_zip = archive::zip_name_for(&sources, &dest_dir);
                Ok(Response::TransferStarted { id: self.compress_job(sources, dest_zip) })
            }
            Request::Extract { zip, dest_dir: _ } => Ok(Response::TransferStarted { id: self.extract_job(zip) }),
            Request::Chmod { path, mode } => {
                blocking(move || ops::chmod(&path, mode).map_err(std::io::Error::other)).await?;
                Ok(Response::Ok)
            }
            Request::Properties { paths } => {
                let me = self.clone();
                let props = blocking(move || {
                    let mut props = ops::properties(&paths).map_err(std::io::Error::other)?;
                    if let Some(e) = props.entry.as_mut() {
                        me.apps.annotate(std::slice::from_mut(e));
                    }
                    Ok(props)
                })
                .await?;
                Ok(Response::Properties(Box::new(props)))
            }
            Request::AppsFor { mime } => {
                let me = self.clone();
                Ok(Response::Apps(blocking(move || Ok(me.apps.apps_for(&mime))).await?))
            }
            Request::AllApps => {
                let me = self.clone();
                Ok(Response::Apps(blocking(move || Ok(me.apps.all())).await?))
            }
            Request::SetDefaultApp { mime, app } => {
                let me = self.clone();
                blocking(move || me.apps.set_default(&mime, &app).map_err(std::io::Error::other)).await?;
                Ok(Response::Ok)
            }
            Request::OpenTerminal { dir } => {
                ops::open_terminal(&dir)?;
                Ok(Response::Ok)
            }
            Request::Pin { path } | Request::Unpin { path } if !path.is_dir() => {
                anyhow::bail!("{} is not a folder", path.display())
            }
            Request::Pin { path } => {
                self.pins.set(&path, true)?;
                self.hub.broadcast(Event::PlacesChanged);
                Ok(Response::Ok)
            }
            Request::Unpin { path } => {
                self.pins.set(&path, false)?;
                self.hub.broadcast(Event::PlacesChanged);
                Ok(Response::Ok)
            }
            Request::Mount { device } => Ok(Response::Mounted(self.mount(&device).await?)),
            Request::Unmount { device } => {
                self.unmount(&device).await?;
                Ok(Response::Ok)
            }
            Request::SetMountPolicy { uuid, policy } => {
                self.set_mount_policy(uuid, policy).await?;
                Ok(Response::Ok)
            }
            Request::DismissAsk { device } => {
                self.dismiss_ask(&device);
                Ok(Response::Ok)
            }
            Request::Places => {
                let home = noxfm_core::complete::home_dir();
                let place = |name: String, path: PathBuf, pinned| noxfm_proto::Place { name, path, pinned };
                let places = std::iter::once(place("Home".into(), home, false))
                    .chain(self.recent.places.iter().map(|p| place(p.name.clone(), p.path.clone(), false)))
                    .chain(self.pins.list().into_iter().filter(|p| p.is_dir()).map(|p| {
                        let name = p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned());
                        place(name, p, true)
                    }))
                    .collect();
                Ok(Response::Places(places))
            }
            Request::Recent { kind, limit } => {
                let me = self.clone();
                let items = blocking(move || {
                    let items = me.recent.log.lock().unwrap().query(kind, limit as usize, |p| p.exists());
                    let mut pairs: Vec<_> = items
                        .into_iter()
                        .filter_map(|i| noxfm_core::stat_entry(&i.path).ok().map(|e| (i, e)))
                        .collect();
                    let mut entries: Vec<_> = pairs.iter().map(|(_, e)| e.clone()).collect();
                    me.apps.annotate(&mut entries);
                    for ((_, e), annotated) in pairs.iter_mut().zip(entries) {
                        *e = annotated;
                    }
                    Ok(pairs)
                })
                .await?;
                Ok(Response::Recent(items))
            }
        }
    }
}

impl Daemon {
    pub(crate) fn push_undo(&self, a: undo::Action) {
        self.undo.push(a);
        self.hub.broadcast(Event::UndoChanged(self.undo.label()));
    }

    fn trash_changed(&self) {
        self.hub.broadcast(Event::TrashChanged { items: trash::count() });
    }

    fn compress_job(self: &Arc<Self>, sources: Vec<PathBuf>, dest_zip: PathBuf) -> u64 {
        let n = sources.len();
        let zip = dest_zip.clone();
        let work: transfers::Work = Box::new(move |p, cancel| {
            // Both callbacks report through `p`.
            let p = std::cell::RefCell::new(p);
            archive::compress(&sources, &zip, cancel, &mut |t| p.borrow_mut().total(t), &mut |d, c| p.borrow_mut().advance(d, c))?;
            Ok(Some(undo::Action::Copied(vec![zip])))
        });
        self.start_job(noxfm_proto::JobKind::Compress, n, dest_zip, work)
    }

    fn extract_job(self: &Arc<Self>, zip: PathBuf) -> u64 {
        let dest = zip.parent().map(Path::to_path_buf).unwrap_or_default();
        let work: transfers::Work = Box::new(move |p, cancel| {
            let p = std::cell::RefCell::new(p);
            let out = archive::extract(&zip, cancel, &mut |t| p.borrow_mut().total(t), &mut |d, c| p.borrow_mut().advance(d, c))?;
            Ok(Some(undo::Action::Copied(vec![out])))
        });
        self.start_job(noxfm_proto::JobKind::Extract, 1, dest, work)
    }

    /// Purges old trash now and every few hours; reports trash changes made
    /// by other apps too.
    fn start_trash(self: &Arc<Self>) {
        let Some(days) = self.trash_days else { return };
        let me = self.clone();
        tokio::spawn(async move {
            let mut every = tokio::time::interval(trash::PURGE_EVERY);
            loop {
                every.tick().await;
                match tokio::task::spawn_blocking(move || trash::purge_older_than(days, trash::now())).await {
                    Ok(Ok(0)) => {}
                    Ok(Ok(n)) => {
                        tracing::info!(n, days, "trash: purged old items");
                        me.trash_changed();
                    }
                    Ok(Err(e)) => tracing::warn!(%e, "trash purge failed"),
                    Err(_) => {}
                }
            }
        });

        let files = trash::home_trash_files();
        let _ = std::fs::create_dir_all(&files);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if res.is_ok() {
                let _ = tx.send(());
            }
        });
        let Ok(mut watcher) = watcher else { return };
        if notify::Watcher::watch(&mut watcher, &files, notify::RecursiveMode::NonRecursive).is_err() {
            return;
        }
        let me = self.clone();
        tokio::spawn(async move {
            let _keep = watcher;
            while rx.recv().await.is_some() {
                tokio::time::sleep(Duration::from_millis(300)).await;
                while rx.try_recv().is_ok() {}
                me.trash_changed();
            }
        });
    }

    fn on_changes(&self, paths: std::collections::HashSet<PathBuf>) {
        for p in paths {
            self.sizes.invalidate(&p);
            if self.hub.is_watched(&p) {
                tracing::debug!(dir = %p.display(), "changed");
                self.hub.publish(&p, Event::DirChanged { path: p.clone() });
            }
        }
    }

    /// Saves what must survive a restart (blocking).
    pub fn save_state(&self) {
        self.save_recent();
        self.sizes.save();
    }

    fn start_sizes(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move {
            let mut save = tokio::time::interval(sizes::SAVE_EVERY);
            loop {
                save.tick().await;
                let me = me.clone();
                let _ = tokio::task::spawn_blocking(move || me.sizes.save()).await;
            }
        });
    }

    /// Fills in cached directory sizes and starts walks for the rest.
    /// Results reach whoever watches `parent` as `SizeUpdated`.
    fn fill_sizes(self: &Arc<Self>, parent: &Path, entries: &mut [Entry]) {
        for e in entries.iter_mut().filter(|e| e.kind == EntryKind::Dir && !e.symlink) {
            let cached = self.sizes.cached(&e.path);
            if let Some((bytes, _)) = cached {
                e.size = Some(bytes);
            }
            if !cached.is_some_and(|(_, fresh)| fresh) && self.sizes.begin(&e.path) {
                tokio::spawn(self.clone().size_job(parent.to_path_buf(), e.path.clone()));
            }
        }
    }

    async fn size_job(self: Arc<Self>, parent: PathBuf, dir: PathBuf) {
        let queued = Instant::now();
        let permit = self.sizes.permits.acquire().await;
        let me = self.clone();
        let (p, d) = (parent.clone(), dir.clone());
        let result = tokio::task::spawn_blocking(move || {
            sizes::walk(&d, || queued.elapsed() < WALK_GRACE || me.hub.is_watched(&p))
        })
        .await;
        drop(permit);
        self.sizes.end(&dir);
        let Ok(walk) = result else { return };
        // Windows already showing a subfolder's parent get its size too.
        for (sub, bytes) in walk.subdirs {
            if let Some(p) = sub.parent() {
                self.hub.publish(p, Event::SizeUpdated { path: sub.clone(), bytes });
            }
            self.sizes.store(sub, bytes);
        }
        if let Some(bytes) = walk.total {
            self.sizes.store(dir.clone(), bytes);
            self.hub.publish(&parent, Event::SizeUpdated { path: dir, bytes });
        }
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> anyhow::Result<T> {
    Ok(tokio::task::spawn_blocking(f).await??)
}
