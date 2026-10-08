use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::{mpsc, oneshot};

use crate::*;

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Response, String>>>>>;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("noxd was built from a different protocol revision (v{0}); restart it")]
    Version(u32),
    #[error("connection closed")]
    Closed,
    #[error("daemon: {0}")]
    Remote(String),
}

/// Cheap to clone; all clones share one connection.
#[derive(Clone)]
pub struct Client {
    writer: Arc<tokio::sync::Mutex<OwnedWriteHalf>>,
    pending: Pending,
    next_id: Arc<AtomicU64>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Client")
    }
}

impl Client {
    /// Connects, handshakes, and spawns a reader task. Events arrive on the
    /// returned receiver, which closes when the daemon goes away.
    pub async fn connect(
        path: &Path,
        role: Role,
    ) -> Result<(Client, mpsc::UnboundedReceiver<Event>), ClientError> {
        let stream = UnixStream::connect(path).await?;
        let (mut r, mut w) = stream.into_split();
        write_frame(&mut w, &Hello::new(role)).await?;
        match read_frame::<_, Welcome>(&mut r).await? {
            Some(Welcome::Ok { .. }) => {}
            Some(Welcome::VersionMismatch { daemon_version }) => {
                return Err(ClientError::Version(daemon_version));
            }
            None => return Err(ClientError::Closed),
        }

        let pending: Pending = Default::default();
        let (tx, rx) = mpsc::unbounded_channel();
        let reader_pending = pending.clone();
        tokio::spawn(async move {
            while let Ok(Some(msg)) = read_frame::<_, ServerMsg>(&mut r).await {
                match msg {
                    ServerMsg::Reply { id, result } => {
                        if let Some(waiter) = reader_pending.lock().unwrap().remove(&id) {
                            let _ = waiter.send(result);
                        }
                    }
                    ServerMsg::Event(ev) => {
                        let _ = tx.send(ev);
                    }
                }
            }
            // Wake everyone still waiting so they see Closed.
            reader_pending.lock().unwrap().clear();
        });

        Ok((
            Client {
                writer: Arc::new(tokio::sync::Mutex::new(w)),
                pending,
                next_id: Arc::new(AtomicU64::new(1)),
            },
            rx,
        ))
    }

    pub async fn request(&self, req: Request) -> Result<Response, ClientError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let sent = write_frame(&mut *self.writer.lock().await, &ClientMsg { id, req }).await;
        if let Err(e) = sent {
            self.pending.lock().unwrap().remove(&id);
            return Err(e.into());
        }
        match rx.await {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(msg)) => Err(ClientError::Remote(msg)),
            Err(_) => Err(ClientError::Closed),
        }
    }
}
