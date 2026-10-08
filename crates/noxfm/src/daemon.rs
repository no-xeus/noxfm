//! Keeps one daemon connection alive as an iced subscription.

use std::path::PathBuf;
use std::time::Duration;

use cosmic::iced::futures::SinkExt;
use cosmic::iced::{Subscription, stream};
use noxfm_proto::{Client, Event, Role};

#[derive(Debug, Clone)]
pub enum Conn {
    Connected(Client),
    Event(Event),
    Lost(String),
}

pub fn subscription(socket: PathBuf, role: Role) -> Subscription<Conn> {
    Subscription::run_with((socket, role), |(socket, role)| {
        let (socket, role) = (socket.clone(), *role);
        stream::channel(64, async move |mut out| {
            loop {
                match Client::connect(&socket, role).await {
                    Ok((client, mut events)) => {
                        let _ = out.send(Conn::Connected(client)).await;
                        while let Some(ev) = events.recv().await {
                            let _ = out.send(Conn::Event(ev)).await;
                        }
                        let _ = out.send(Conn::Lost("daemon disconnected".into())).await;
                    }
                    Err(e) => {
                        let _ = out.send(Conn::Lost(e.to_string())).await;
                    }
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        })
    })
}
