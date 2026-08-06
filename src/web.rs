//! Web server: token-gated WebSocket bridge + embedded mobile terminal page.
//! Everything is served from the binary — no CDN, no external requests.

use crate::bridge::Hub;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use std::collections::HashMap;
use std::sync::Arc;

const PAGE: &str = include_str!("page.html");
const XTERM_JS: &str = include_str!("../assets/xterm.js");
const XTERM_CSS: &str = include_str!("../assets/xterm.css");
const FIT_JS: &str = include_str!("../assets/addon-fit.js");

#[derive(Clone)]
struct AppState {
    hub: Arc<Hub>,
    token: Arc<String>,
    view_only: bool,
}

pub async fn serve(port: u16, token: String, hub: Arc<Hub>, view_only: bool) {
    let state = AppState { hub, token: Arc::new(token), view_only };
    let app = Router::new()
        .route("/", get(page))
        .route("/assets/xterm.js", get(|| async { js(XTERM_JS) }))
        .route("/assets/addon-fit.js", get(|| async { js(FIT_JS) }))
        .route("/assets/xterm.css", get(|| async { css(XTERM_CSS) }))
        .route("/ws", get(ws_upgrade))
        .with_state(state);

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Failed to bind port {port}: {e}");
            std::process::exit(1);
        }
    };
    axum::serve(listener, app).await.expect("server error");
}

fn js(body: &'static str) -> Response {
    ([(header::CONTENT_TYPE, "application/javascript")], body).into_response()
}
fn css(body: &'static str) -> Response {
    ([(header::CONTENT_TYPE, "text/css")], body).into_response()
}

async fn page(State(st): State<AppState>) -> Html<String> {
    // The token itself never reaches the server via the page URL (it lives in
    // the #fragment); only the WS handshake carries it.
    Html(PAGE.replace("__VIEW_ONLY__", if st.view_only { "true" } else { "false" }))
}

/// Constant-time-ish comparison (length check + XOR fold).
fn token_ok(given: &str, expected: &str) -> bool {
    let (a, b) = (given.as_bytes(), expected.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn ws_upgrade(
    ws: WebSocketUpgrade,
    Query(q): Query<HashMap<String, String>>,
    State(st): State<AppState>,
) -> Response {
    let given = q.get("token").map(String::as_str).unwrap_or("");
    if !token_ok(given, &st.token) {
        return (StatusCode::UNAUTHORIZED, "bad token").into_response();
    }
    ws.on_upgrade(move |socket| client(socket, st))
}

async fn client(socket: WebSocket, st: AppState) {
    let (mut tx, mut rx) = {
        use futures_util_split::split_ws;
        split_ws(socket)
    };

    // Replay the latest session list so the UI renders instantly.
    // (Clone out first — a MutexGuard held across .await makes the future !Send.)
    let snap = st.hub.last_sessions.lock().unwrap().clone();
    if let Some(snap) = snap {
        if tx.send_text(snap).await.is_err() {
            return;
        }
    }

    let mut events = st.hub.events.subscribe();
    loop {
        tokio::select! {
            ev = events.recv() => {
                match ev {
                    Ok(line) => {
                        if tx.send_text(line).await.is_err() { break; }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
            msg = rx.next_msg() => {
                let Some(Ok(Message::Text(text))) = msg else { break };
                let Ok(req) = serde_json::from_str::<serde_json::Value>(text.as_str()) else { continue };
                match req["op"].as_str().unwrap_or("") {
                    "sessions" | "attach" => {
                        let _ = st.hub.to_bridge.send(text.to_string()).await;
                    }
                    "input" if !st.view_only => {
                        let _ = st.hub.to_bridge.send(text.to_string()).await;
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Tiny split helper so we don't pull the futures crate: wraps WebSocket
/// halves with the two operations we need.
mod futures_util_split {
    use axum::extract::ws::{Message, WebSocket};
    use futures_core_stream::{SinkHalf, StreamHalf};

    pub fn split_ws(ws: WebSocket) -> (SinkHalf, StreamHalf) {
        let (sink, stream) = futures_core_stream::split(ws);
        (sink, stream)
    }

    pub mod futures_core_stream {
        use super::{Message, WebSocket};

        pub struct SinkHalf(tokio::sync::mpsc::Sender<Message>);
        pub struct StreamHalf(tokio::sync::mpsc::Receiver<Result<Message, axum::Error>>);

        /// Split by pumping the socket in a dedicated task — avoids a
        /// futures-util dependency for Sink/Stream splitting.
        pub fn split(mut ws: WebSocket) -> (SinkHalf, StreamHalf) {
            let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<Message>(256);
            let (in_tx, in_rx) = tokio::sync::mpsc::channel::<Result<Message, axum::Error>>(256);
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        m = ws.recv() => {
                            match m {
                                Some(m) => { if in_tx.send(m).await.is_err() { break; } }
                                None => break,
                            }
                        }
                        m = out_rx.recv() => {
                            match m {
                                Some(m) => { if ws.send(m).await.is_err() { break; } }
                                None => break,
                            }
                        }
                    }
                }
            });
            (SinkHalf(out_tx), StreamHalf(in_rx))
        }

        impl SinkHalf {
            pub async fn send_text(&mut self, s: String) -> Result<(), ()> {
                self.0.send(Message::Text(s.into())).await.map_err(|_| ())
            }
        }
        impl StreamHalf {
            pub async fn next_msg(&mut self) -> Option<Result<Message, axum::Error>> {
                self.0.recv().await
            }
        }
    }
}
