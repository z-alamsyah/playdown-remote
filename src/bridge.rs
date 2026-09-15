//! Client for Playdown's bridge socket (BRIDGE_PROTOCOL.md, v1).
//!
//! One connection is shared by every web client: lines FROM Playdown fan out
//! on a broadcast channel; lines TO Playdown funnel through an mpsc queue.
//! Reconnects with backoff if Playdown restarts.

use std::sync::atomic::{AtomicU32, Ordering};
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
    /// Devices currently on the web UI, reported to Playdown so it can show
    /// (and drop) what is attached.
    pub viewers: AtomicU32,
}

impl Hub {
    /// Tell Playdown how many devices we are serving. Called whenever a phone
    /// connects or drops; harmless when the bridge is down.
    pub fn report_viewers(&self) {
        let n = self.viewers.load(Ordering::Relaxed);
        let _ = self
            .to_bridge
            .try_send(format!("{{\"op\":\"status\",\"viewers\":{n}}}"));
    }
}

pub fn start(socket_path: String) -> Arc<Hub> {
    let (to_tx, mut to_rx) = mpsc::channel::<String>(256);
    let hub = Arc::new(Hub {
        to_bridge: to_tx,
        events: broadcast::channel(512).0,
        last_sessions: Mutex::new(None),
        connected: Mutex::new(false),
        viewers: AtomicU32::new(0),
    });

    let hub2 = hub.clone();
    tokio::spawn(async move {
        loop {
            match UnixStream::connect(&socket_path).await {
                Ok(stream) => {
                    *hub2.connected.lock().unwrap() = true;
                    eprintln!("[bridge] connected");
                    let (read_half, mut write) = stream.into_split();
                    // Introduce ourselves so Playdown's Settings can name this
                    // connection, and re-report viewers after a reconnect.
                    let hello = format!(
                        "{{\"op\":\"hello\",\"v\":1,\"name\":\"playdown-remote\",\"version\":\"{}\",\"pid\":{}}}",
                        env!("CARGO_PKG_VERSION"),
                        std::process::id()
                    );
                    let _ = write.write_all(format!("{hello}\n").as_bytes()).await;
                    hub2.report_viewers();
                    let mut lines = BufReader::new(read_half).lines();
                    loop {
                        tokio::select! {
                            line = lines.next_line() => {
                                match line {
                                    Ok(Some(line)) => {
                                        // Playdown dropped us on purpose (the user
                                        // hit Disconnect): stop, don't reconnect -
                                        // that is the whole point of the button.
                                        if line.contains("\"ev\":\"bye\"") {
                                            use std::io::Write;
                                            let _ = writeln!(
                                                std::io::stderr(),
                                                "[bridge] disconnected by Playdown - exiting"
                                            );
                                            std::process::exit(0);
                                        }
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
