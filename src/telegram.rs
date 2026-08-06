//! Optional Telegram bot — push notifications when an agent blocks or
//! finishes, plus lightweight control: /status, /send, /tail, and inline
//! keys (Enter/Esc/1/2/3/^C) on "needs you" alerts.
//!
//! Uses the user's OWN bot (@BotFather) with long polling: no webhook, no
//! public endpoint, no third-party relay. Fail-closed: without an
//! allowlisted chat id the bot only replies with pairing instructions.

use crate::bridge::Hub;
use base64::Engine;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub struct Config {
    pub token: String,
    /// Allowlisted chat. None = pairing mode: reply with the chat id, allow nothing.
    pub chat_id: Option<String>,
    pub view_only: bool,
}

/// Virtual screen size: wide enough for a desktop-sized PTY (cursor writes
/// beyond the grid are clamped), tall enough to hold a full TUI frame.
const VT_ROWS: u16 = 60;
const VT_COLS: u16 = 240;

struct State {
    cfg: Config,
    hub: Arc<Hub>,
    http: reqwest::Client,
    /// Per-session virtual terminal. TUIs (Claude Code) repaint the screen
    /// with cursor positioning — the raw byte stream is unreadable soup, but
    /// the RENDERED screen is exactly what the user would see on the desktop.
    screens: Mutex<HashMap<String, vt100::Parser>>,
}

pub fn start(cfg: Config, hub: Arc<Hub>) {
    let st = Arc::new(State {
        cfg,
        hub,
        http: reqwest::Client::new(),
        screens: Mutex::new(HashMap::new()),
    });
    tokio::spawn(announce(st.clone()));
    tokio::spawn(watch_events(st.clone()));
    tokio::spawn(poll_updates(st));
}

fn api(token: &str, method: &str) -> String {
    // Overridable for tests (mock server); defaults to the real API.
    let base = std::env::var("TELEGRAM_API").unwrap_or_else(|_| "https://api.telegram.org".into());
    format!("{base}/bot{token}/{method}")
}

async fn call(st: &State, method: &str, body: Value) -> Option<Value> {
    let resp = st
        .http
        .post(api(&st.cfg.token, method))
        .json(&body)
        .timeout(std::time::Duration::from_secs(65))
        .send()
        .await
        .ok()?;
    resp.json::<Value>().await.ok()
}

async fn send_text(st: &State, chat: &str, text: &str, keyboard: Option<Value>) {
    let mut body = json!({ "chat_id": chat, "text": text, "parse_mode": "HTML" });
    if let Some(kb) = keyboard {
        body["reply_markup"] = kb;
    }
    let _ = call(st, "sendMessage", body).await;
}

fn esc_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Startup: validate the token and describe the mode on stdout.
async fn announce(st: Arc<State>) {
    match call(&st, "getMe", json!({})).await {
        Some(v) if v["ok"].as_bool() == Some(true) => {
            let user = v["result"]["username"].as_str().unwrap_or("?");
            match &st.cfg.chat_id {
                Some(chat) => {
                    eprintln!("[telegram] @{user} ready (chat {chat})");
                    let _ = call(
                        &st,
                        "setMyCommands",
                        json!({ "commands": [
                            { "command": "status", "description": "Sessions and agent statuses" },
                            { "command": "send", "description": "/send <n> <text> — type into session n" },
                            { "command": "tail", "description": "/tail <n> — recent output of session n" },
                            { "command": "help", "description": "Show usage" },
                        ]}),
                    )
                    .await;
                }
                None => eprintln!(
                    "[telegram] @{user} in PAIRING mode — message the bot to get your chat id, \
                     then restart with --telegram-chat <id>"
                ),
            }
        }
        Some(_) => eprintln!("[telegram] token rejected by api.telegram.org — check --telegram"),
        None => eprintln!("[telegram] can't reach api.telegram.org — will keep retrying"),
    }
}

/// Screen-line cleanup for chat: drop trailing whitespace, box borders, and
/// lines that are nothing but box drawing.
fn clean_line(l: &str) -> String {
    let t = l.trim_end();
    let t = t.trim_matches(|c: char| matches!(c, '│' | '┃' | '║')).trim();
    if t.chars().all(|c| {
        c.is_whitespace()
            || matches!(c, '─' | '━' | '═' | '╭' | '╮' | '╰' | '╯' | '┌' | '┐' | '└' | '┘' | '├' | '┤' | '┬' | '┴' | '┼')
    }) {
        return String::new();
    }
    t.to_string()
}

fn status_emoji(status: &str) -> &'static str {
    match status {
        "blocked" => "🔴",
        "working" => "🔵",
        "done" => "🟢",
        _ => "⚪",
    }
}

fn session_name(s: &Value) -> String {
    s["custom"]
        .as_str()
        .or_else(|| s["title"].as_str())
        .or_else(|| s["label"].as_str())
        .unwrap_or("?")
        .to_string()
}

/// Last `max_lines` meaningful lines of a session's RENDERED screen.
fn tail_snippet(st: &State, id: &str, max_lines: usize) -> Option<String> {
    let screens = st.screens.lock().unwrap();
    let parser = screens.get(id)?;
    let contents = parser.screen().contents();
    let lines: Vec<String> = contents.lines().map(clean_line).filter(|l| !l.is_empty()).collect();
    let take = lines.len().min(max_lines);
    let mut snip = lines[lines.len() - take..].join("\n");
    if snip.len() > 3200 {
        let cut = snip.len() - 3200;
        let cut = snip.char_indices().map(|(i, _)| i).find(|&i| i >= cut).unwrap_or(0);
        snip = snip[cut..].to_string();
    }
    if snip.is_empty() { None } else { Some(snip) }
}

fn keys_keyboard(id: &str) -> Value {
    let k = |label: &str, key: &str| json!({ "text": label, "callback_data": format!("k:{id}:{key}") });
    json!({ "inline_keyboard": [
        [k("↵ Enter", "enter"), k("Esc", "esc")],
        [k("1", "1"), k("2", "2"), k("3", "3"), k("^C", "ctrlc")],
    ]})
}

/// Watch bridge events: keep output tails, notify on status transitions.
async fn watch_events(st: Arc<State>) {
    let Some(chat) = st.cfg.chat_id.clone() else { return };
    let mut rx = st.hub.events.subscribe();
    let mut statuses: HashMap<String, String> = HashMap::new();
    // Seed from the cached snapshot: the bridge usually connects (and
    // broadcasts the first sessions event) before this task subscribes, so
    // without the seed the next real transition would look like a first
    // sighting and be swallowed.
    let cached = st.hub.last_sessions.lock().unwrap().clone();
    if let Some(ev) = cached.and_then(|l| serde_json::from_str::<Value>(&l).ok()) {
        if let Some(list) = ev["sessions"].as_array() {
            for s in list {
                if let (Some(id), Some(status)) = (s["id"].as_str(), s["status"].as_str()) {
                    statuses.insert(id.to_string(), status.to_string());
                }
            }
        }
    }
    // Attach to every seeded session so the scrollback replay fills the
    // virtual screens — /tail works even for tabs quiet since before start.
    for id in statuses.keys() {
        st.screens
            .lock()
            .unwrap()
            .insert(id.clone(), vt100::Parser::new(VT_ROWS, VT_COLS, 0));
        let _ = st.hub.to_bridge.send(json!({ "op": "attach", "id": id }).to_string()).await;
    }
    let b64 = base64::engine::general_purpose::STANDARD;
    loop {
        let line = match rx.recv().await {
            Ok(l) => l,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(_) => break,
        };
        let Ok(ev) = serde_json::from_str::<Value>(&line) else { continue };
        match ev["ev"].as_str() {
            // Both live output and the attach replay feed the virtual screen.
            Some("output") | Some("scrollback") => {
                let (Some(id), Some(data)) = (ev["id"].as_str(), ev["data"].as_str()) else { continue };
                let Ok(bytes) = b64.decode(data) else { continue };
                let mut screens = st.screens.lock().unwrap();
                screens
                    .entry(id.to_string())
                    .or_insert_with(|| vt100::Parser::new(VT_ROWS, VT_COLS, 0))
                    .process(&bytes);
            }
            Some("sessions") => {
                let Some(list) = ev["sessions"].as_array() else { continue };
                let mut seen: Vec<String> = Vec::new();
                for s in list {
                    let (Some(id), Some(status)) = (s["id"].as_str(), s["status"].as_str()) else { continue };
                    seen.push(id.to_string());
                    // Attach once per session so the scrollback replay seeds
                    // the screen — /tail works even for tabs quiet since
                    // before this process started.
                    if !st.screens.lock().unwrap().contains_key(id) {
                        st.screens
                            .lock()
                            .unwrap()
                            .insert(id.to_string(), vt100::Parser::new(VT_ROWS, VT_COLS, 0));
                        let _ = st.hub.to_bridge.send(json!({ "op": "attach", "id": id }).to_string()).await;
                    }
                    let prev = statuses.insert(id.to_string(), status.to_string());
                    let Some(prev) = prev else { continue }; // first sight — no alert spam on connect
                    if prev == status {
                        continue;
                    }
                    let name = esc_html(&session_name(s));
                    if status == "blocked" {
                        let mut text = format!("🔴 <b>{name}</b> needs you");
                        if let Some(snip) = tail_snippet(&st, id, 8) {
                            text.push_str(&format!("\n<pre>{}</pre>", esc_html(&snip)));
                        }
                        let kb = if st.cfg.view_only { None } else { Some(keys_keyboard(id)) };
                        send_text(&st, &chat, &text, kb).await;
                    } else if status == "done" && prev == "working" {
                        send_text(&st, &chat, &format!("🟢 <b>{name}</b> done"), None).await;
                    }
                }
                statuses.retain(|id, _| seen.contains(id));
                st.screens.lock().unwrap().retain(|id, _| seen.contains(id));
            }
            _ => {}
        }
    }
}

/// Cached session list (from the hub) as (id, name, status) rows.
fn session_rows(st: &State) -> Vec<(String, String, String)> {
    let cached = st.hub.last_sessions.lock().unwrap().clone();
    let Some(line) = cached else { return Vec::new() };
    let Ok(ev) = serde_json::from_str::<Value>(&line) else { return Vec::new() };
    ev["sessions"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|s| {
                    Some((
                        s["id"].as_str()?.to_string(),
                        session_name(s),
                        s["status"].as_str().unwrap_or("idle").to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn send_input(st: &State, id: &str, data: &str) {
    let b64 = base64::engine::general_purpose::STANDARD.encode(data.as_bytes());
    let _ = st
        .hub
        .to_bridge
        .send(json!({ "op": "input", "id": id, "data": b64 }).to_string())
        .await;
}

const HELP: &str = "Playdown Remote bot\n\
    /status — sessions and agent statuses\n\
    /send <n> <text> — type into session n (Enter appended)\n\
    /tail <n> — recent output of session n\n\
    Alerts for 🔴 blocked agents carry Enter/Esc/1/2/3/^C buttons.";

async fn handle_command(st: &Arc<State>, chat: &str, text: &str) {
    let bridge_up = *st.hub.connected.lock().unwrap();
    let mut parts = text.split_whitespace();
    let cmd = parts.next().unwrap_or("");
    // "/status@MyBot" also matches.
    let cmd = cmd.split('@').next().unwrap_or(cmd);
    match cmd {
        "/start" | "/help" => send_text(st, chat, HELP, None).await,
        "/status" => {
            if !bridge_up {
                send_text(st, chat, "Playdown bridge is down (app closed or bridge off).", None).await;
                return;
            }
            let rows = session_rows(st);
            if rows.is_empty() {
                send_text(st, chat, "No terminal sessions in Playdown.", None).await;
                return;
            }
            let mut out = String::new();
            for (i, (_, name, status)) in rows.iter().enumerate() {
                out.push_str(&format!("{} {}. <b>{}</b> — {}\n", status_emoji(status), i + 1, esc_html(name), status));
            }
            send_text(st, chat, out.trim_end(), None).await;
        }
        "/send" => {
            if st.cfg.view_only {
                send_text(st, chat, "Running with --view-only: input is disabled.", None).await;
                return;
            }
            let n: Option<usize> = parts.next().and_then(|v| v.parse().ok());
            let rest = parts.collect::<Vec<_>>().join(" ");
            let rows = session_rows(st);
            match n.and_then(|n| rows.get(n.wrapping_sub(1))) {
                Some((id, name, _)) if !rest.is_empty() => {
                    send_input(st, id, &format!("{rest}\r")).await;
                    send_text(st, chat, &format!("→ sent to <b>{}</b>", esc_html(name)), None).await;
                }
                _ => send_text(st, chat, "Usage: /send <n> <text> — n from /status", None).await,
            }
        }
        "/tail" => {
            let n: Option<usize> = parts.next().and_then(|v| v.parse().ok());
            let rows = session_rows(st);
            match n.and_then(|n| rows.get(n.wrapping_sub(1))) {
                Some((id, name, _)) => {
                    let snip = tail_snippet(st, id, 25).unwrap_or_else(|| "(no recent output)".into());
                    send_text(st, chat, &format!("<b>{}</b>\n<pre>{}</pre>", esc_html(name), esc_html(&snip)), None).await;
                }
                None => send_text(st, chat, "Usage: /tail <n> — n from /status", None).await,
            }
        }
        _ => {}
    }
}

async fn handle_callback(st: &Arc<State>, cb: &Value) {
    let cb_id = cb["id"].as_str().unwrap_or("");
    let chat = cb["message"]["chat"]["id"].as_i64().map(|v| v.to_string()).unwrap_or_default();
    let allowed = st.cfg.chat_id.as_deref() == Some(chat.as_str());
    let mut ack = json!({ "callback_query_id": cb_id });
    if allowed && !st.cfg.view_only {
        if let Some(data) = cb["data"].as_str() {
            let mut it = data.splitn(3, ':');
            if let (Some("k"), Some(id), Some(key)) = (it.next(), it.next(), it.next()) {
                let bytes = match key {
                    "enter" => "\r",
                    "esc" => "\x1b",
                    "ctrlc" => "\x03",
                    "1" => "1",
                    "2" => "2",
                    "3" => "3",
                    _ => "",
                };
                if !bytes.is_empty() {
                    send_input(st, id, bytes).await;
                    ack["text"] = json!(format!("sent {key}"));
                }
            }
        }
    }
    let _ = call(st, "answerCallbackQuery", ack).await;
}

/// Long-poll getUpdates. Only the allowlisted chat may control anything;
/// unknown chats get pairing instructions when no chat is configured, and
/// silence otherwise.
async fn poll_updates(st: Arc<State>) {
    let mut offset: i64 = 0;
    loop {
        let resp = call(
            &st,
            "getUpdates",
            json!({ "timeout": 50, "offset": offset, "allowed_updates": ["message", "callback_query"] }),
        )
        .await;
        let Some(v) = resp else {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            continue;
        };
        let Some(updates) = v["result"].as_array() else {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            continue;
        };
        for u in updates {
            if let Some(id) = u["update_id"].as_i64() {
                offset = offset.max(id + 1);
            }
            if let Some(cb) = u.get("callback_query") {
                handle_callback(&st, cb).await;
                continue;
            }
            let msg = &u["message"];
            let (Some(chat), Some(text)) = (msg["chat"]["id"].as_i64(), msg["text"].as_str()) else { continue };
            let chat = chat.to_string();
            match &st.cfg.chat_id {
                None => {
                    send_text(
                        &st,
                        &chat,
                        &format!(
                            "Your chat id: <code>{chat}</code>\n\
                             Restart playdown-remote with:\n\
                             <code>--telegram-chat {chat}</code>"
                        ),
                        None,
                    )
                    .await;
                }
                Some(allowed) if *allowed == chat => handle_command(&st, &chat, text).await,
                Some(_) => {} // unknown chat: silence
            }
        }
    }
}
