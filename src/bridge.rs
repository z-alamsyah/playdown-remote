//! Client for Playdown's bridge socket (BRIDGE_PROTOCOL.md, v1).
//!
//! One connection is shared by every web client: lines FROM Playdown fan out
//! on a broadcast channel; lines TO Playdown funnel through an mpsc queue.
//! Reconnects with backoff if Playdown restarts.

use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{broadcast, mpsc};

pub struct Hub {
    /// JSON lines to forward to Playdown (input/attach/sessions requests).
    pub to_bridge: mpsc::Sender<String>,
    /// JSON lines received from Playdown (output/sessions/scrollback events).
    pub events: broadcast::Sender<String>,
    /// Last `sessions` event — replayed to newly connected web clients.
    pub last_sessions: Mutex<Option<String>>,
    /// Whether the bridge connection is currently up.
    pub connected: Mutex<bool>,
}

pub fn start(socket_path: String) -> Arc<Hub> {
    let (to_tx, mut to_rx) = mpsc::channel::<String>(256);
    let hub = Arc::new(Hub {
        to_bridge: to_tx,
        events: broadcast::channel(512).0,
        last_sessions: Mutex::new(None),
        connected: Mutex::new(false),
    });

    let hub2 = hub.clone();
    tokio::spawn(async move {
        loop {
            match UnixStream::connect(&socket_path).await {
                Ok(stream) => {
                    *hub2.connected.lock().unwrap() = true;
                    eprintln!("[bridge] connected");
                    let (read_half, mut write) = stream.into_split();
                    let mut lines = BufReader::new(read_half).lines();
                    loop {
                        tokio::select! {
                            line = lines.next_line() => {
                                match line {
                                    Ok(Some(line)) => {
                                        if line.contains("\"ev\":\"sessions\"") {
                                            *hub2.last_sessions.lock().unwrap() = Some(line.clone());
                                        }
                                        let _ = hub2.events.send(line);
                                    }
                                    _ => break, // Playdown closed / bridge off
                                }
                            }
                            out = to_rx.recv() => {
                                let Some(msg) = out else { return };
                                if write.write_all(format!("{msg}\n").as_bytes()).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    *hub2.connected.lock().unwrap() = false;
                    eprintln!("[bridge] disconnected — retrying");
                }
                Err(_) => { /* Playdown not running or bridge off */ }
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    });

    hub
}
