//! Tracks connected clients and routes events to them.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use noxfm_proto::{Event, Role, ServerMsg};
use tokio::sync::mpsc;

pub type ClientId = u64;

struct ClientSlot {
    role: Role,
    tx: mpsc::UnboundedSender<ServerMsg>,
    watching: HashSet<PathBuf>,
}

#[derive(Default)]
pub struct Hub {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    next_id: ClientId,
    clients: HashMap<ClientId, ClientSlot>,
}

impl Hub {
    pub fn register(&self, role: Role, tx: mpsc::UnboundedSender<ServerMsg>) -> ClientId {
        let mut g = self.inner.lock().unwrap();
        g.next_id += 1;
        let id = g.next_id;
        g.clients.insert(id, ClientSlot { role, tx, watching: HashSet::new() });
        id
    }

    /// Returns the paths that nobody watches anymore.
    pub fn unregister(&self, id: ClientId) -> Vec<PathBuf> {
        let mut g = self.inner.lock().unwrap();
        let Some(gone) = g.clients.remove(&id) else { return Vec::new() };
        gone.watching
            .into_iter()
            .filter(|p| !g.clients.values().any(|c| c.watching.contains(p)))
            .collect()
    }

    pub fn is_watched(&self, path: &Path) -> bool {
        self.inner.lock().unwrap().clients.values().any(|c| c.watching.contains(path))
    }

    /// Returns true if this is the first client watching `path`.
    pub fn watch(&self, id: ClientId, path: PathBuf) -> bool {
        let mut g = self.inner.lock().unwrap();
        let first = !g.clients.values().any(|c| c.watching.contains(&path));
        match g.clients.get_mut(&id) {
            Some(c) => c.watching.insert(path) && first,
            None => false,
        }
    }

    /// Returns true if this client was the last one watching `path`.
    pub fn unwatch(&self, id: ClientId, path: &Path) -> bool {
        let mut g = self.inner.lock().unwrap();
        let removed = g.clients.get_mut(&id).is_some_and(|c| c.watching.remove(path));
        removed && !g.clients.values().any(|c| c.watching.contains(path))
    }

    /// Sends to every client watching `dir`.
    pub fn publish(&self, dir: &Path, ev: Event) {
        let g = self.inner.lock().unwrap();
        for c in g.clients.values().filter(|c| c.watching.contains(dir)) {
            let _ = c.tx.send(ServerMsg::Event(ev.clone()));
        }
    }

    pub fn broadcast(&self, ev: Event) {
        let g = self.inner.lock().unwrap();
        for c in g.clients.values() {
            let _ = c.tx.send(ServerMsg::Event(ev.clone()));
        }
    }

    pub fn has_role(&self, role: Role) -> bool {
        self.inner.lock().unwrap().clients.values().any(|c| c.role == role)
    }
}
