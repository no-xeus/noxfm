//! The daemon connection, kept alive on a tokio thread. GTK code awaits
//! replies with [`Daemon::request`] and gets connection changes and events on
//! a channel.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use noxfm_proto::{Client, Event, Request, Response, Role};

pub enum Conn {
    Connected,
    Event(Event),
    Lost(String),
}

#[derive(Clone)]
pub struct Daemon {
    rt: tokio::runtime::Handle,
    client: Arc<Mutex<Option<Client>>>,
}

impl Daemon {
    /// Connects in the background, and reconnects whenever noxd goes away
    /// (it restarts itself after an update).
    pub fn start(socket: PathBuf) -> (Daemon, async_channel::Receiver<Conn>) {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("tokio runtime");
        // Lives as long as the process.
        let rt: &'static tokio::runtime::Runtime = Box::leak(Box::new(rt));
        let client = Arc::new(Mutex::new(None));
        let (tx, rx) = async_channel::unbounded();
        let shared = client.clone();
        rt.spawn(async move {
            loop {
                let lost = match Client::connect(&socket, Role::Browser).await {
                    Ok((c, mut events)) => {
                        *shared.lock().unwrap() = Some(c);
                        if tx.send(Conn::Connected).await.is_err() {
                            return;
                        }
                        while let Some(ev) = events.recv().await {
                            if tx.send(Conn::Event(ev)).await.is_err() {
                                return;
                            }
                        }
                        *shared.lock().unwrap() = None;
                        "daemon disconnected".to_owned()
                    }
                    Err(e) => e.to_string(),
                };
                if tx.send(Conn::Lost(lost)).await.is_err() {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
        (Daemon { rt: rt.handle().clone(), client }, rx)
    }

    /// Sends `req` from the GTK main loop; the reply is awaited there.
    pub async fn request(&self, req: Request) -> Result<Response, String> {
        let client = self.client.lock().unwrap().clone().ok_or_else(|| "noxd unavailable".to_owned())?;
        self.rt
            .spawn(async move { client.request(req).await.map_err(|e| e.to_string()) })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
    }
}
