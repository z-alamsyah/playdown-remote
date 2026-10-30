//! playdown-remote — remote access companion for Playdown.
//!
//! Connects to Playdown's local bridge socket (see BRIDGE_PROTOCOL.md in the
//! playdown repo) and serves a mobile web terminal on your LAN/Tailscale:
//! session tabs with agent status, live output, and input — protected by a
//! per-run token (QR printed on startup).

mod bridge;
mod telegram;
mod web;

use rand::RngCore;

fn help() -> ! {
    println!(
        "playdown-remote — phone access to your Playdown terminal sessions\n\n\
         USAGE: playdown-remote [OPTIONS]\n\n\
         OPTIONS:\n  \
         --port <PORT>           HTTP port (default: 7423)\n  \
         --socket <PATH>         Playdown bridge socket (default: ~/.playdown/bridge.sock)\n  \
         --view-only             Disable input from remote clients\n  \
         --telegram <TOKEN>      Enable the Telegram bot (or env TELEGRAM_BOT_TOKEN)\n  \
         --telegram-chat <ID>    Allowlisted chat id (or env TELEGRAM_CHAT_ID);\n                          \
         without it the bot only replies with pairing instructions\n  \
         --json                  Print a machine-readable ready line (for supervisors),\n                          \
         then a \"urls\" line whenever the addresses change\n  \
         --parent-pid <PID>      Exit when that process dies (supervised mode)\n  \
         --help                  Show this help\n\n\
         Enable the bridge first: Playdown → Settings → Terminal & agents → Remote bridge."
    );
    std::process::exit(0);
}

/// Every address the web UI answers on right now, as (url, kind) with kind
/// "lan" | "tailscale".
fn addresses(port: u16, token: &str) -> Vec<(String, String)> {
    let mut urls: Vec<(String, String)> = Vec::new();
    if let Ok(ip) = local_ip_address::local_ip() {
        urls.push((format!("http://{ip}:{port}/#t={token}"), "lan".into()));
    }
    if let Ok(ifaces) = local_ip_address::list_afinet_netifas() {
        for (_, ip) in ifaces {
            // Tailscale interfaces (100.x.y.z) get their own entry.
            if ip.is_ipv4() && ip.to_string().starts_with("100.") {
                urls.push((format!("http://{ip}:{port}/#t={token}"), "tailscale".into()));
            }
        }
    }
    urls.dedup();
    urls
}

fn qr_art(url: &str) -> Option<String> {
    qrcode::QrCode::new(url.as_bytes()).ok().map(|code| {
        code.render::<qrcode::render::unicode::Dense1x2>()
            .quiet_zone(true)
            .build()
    })
}

fn urls_json(urls: &[(String, String)]) -> serde_json::Value {
    urls.iter()
        .map(|(u, kind)| serde_json::json!({ "url": u, "kind": kind, "qr": qr_art(u) }))
        .collect()
}

fn print_urls(urls: &[(String, String)]) {
    println!();
    for (u, kind) in urls {
        let suffix = if kind == "tailscale" { "  (tailscale)" } else { "" };
        println!("  {u}{suffix}");
    }
    // One QR per URL: the LAN address only works on the same network, the
    // Tailscale one works anywhere — scan whichever fits.
    for (u, kind) in urls {
        let label = if kind == "tailscale" { "TAILSCALE (works anywhere)" } else { "LAN (same wifi only)" };
        if let Some(art) = qr_art(u) {
            println!("\n  ▼ {label}\n{art}");
        }
    }
}

#[tokio::main]
async fn main() {
    let mut port: u16 = 7423;
    let mut socket: Option<String> = None;
    let mut view_only = false;
    let mut json_mode = false;
    let mut parent_pid: Option<u32> = None;
    let mut tg_token = std::env::var("TELEGRAM_BOT_TOKEN").ok();
    let mut tg_chat = std::env::var("TELEGRAM_CHAT_ID").ok();

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--port" => port = args.next().and_then(|v| v.parse().ok()).unwrap_or(7423),
            "--socket" => socket = args.next(),
            "--view-only" => view_only = true,
            "--json" => json_mode = true,
            "--parent-pid" => parent_pid = args.next().and_then(|v| v.parse().ok()),
            "--telegram" => tg_token = args.next(),
            "--telegram-chat" => tg_chat = args.next(),
            "--help" | "-h" => help(),
            _ => {}
        }
    }

    // Supervised mode (Playdown spawns us): exit when the parent dies, so a
    // crashed or force-quit Playdown never leaves an orphaned server behind.
    if let Some(pid) = parent_pid {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let alive = std::process::Command::new("kill")
                    .args(["-0", &pid.to_string()])
                    .stderr(std::process::Stdio::null())
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);
                if !alive {
                    // The parent dying usually breaks our stdout/stderr pipes
                    // too, and eprintln! PANICS on a failed write - which
                    // killed this task before it could exit, leaving an
                    // orphaned server behind. Write defensively, then leave.
                    use std::io::Write;
                    let _ = writeln!(std::io::stderr(), "[supervise] parent {pid} gone - exiting");
                    std::process::exit(0);
                }
            }
        });
    }

    let socket_path = socket.unwrap_or_else(|| {
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{home}/.playdown/bridge.sock")
    });

    if !std::path::Path::new(&socket_path).exists() {
        eprintln!(
            "Bridge socket not found at {socket_path}.\n\
             Open Playdown → Settings → Terminal & agents → Remote bridge: On."
        );
        std::process::exit(1);
    }

    let mut token_bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut token_bytes);
    let token: String = token_bytes.iter().map(|b| format!("{b:02x}")).collect();

    // Bind BEFORE printing URLs/QR — otherwise a port conflict surfaces after
    // a full page of output that looks like a successful start.
    let listener = web::bind(port).await;

    let hub = bridge::start(socket_path.clone());

    if let Some(token) = tg_token.filter(|t| !t.is_empty()) {
        telegram::start(
            telegram::Config { token, chat_id: tg_chat.filter(|c| !c.is_empty()), view_only },
            hub.clone(),
        );
    }

    let urls = addresses(port, &token);
    if json_mode {
        // Machine-readable handshake for supervisors (Playdown's Settings UI):
        // one JSON line on stdout, then we keep serving. Logs stay on stderr.
        let info = serde_json::json!({
            "event": "ready",
            "version": env!("CARGO_PKG_VERSION"),
            "port": port,
            "view_only": view_only,
            "urls": urls_json(&urls),
        });
        println!("{info}");
    } else {
        println!("playdown-remote v{}", env!("CARGO_PKG_VERSION"));
        println!("bridge: {socket_path}");
        if view_only {
            println!("mode:   VIEW ONLY (remote input disabled)");
        }
        print_urls(&urls);
        println!("\nOpen the URL on your phone (same LAN or Tailscale). Ctrl+C to stop.");
    }

    // Tailscale (or Wi-Fi) can come up after we start. Re-read the addresses
    // and announce changes, so the QR a supervisor shows (or this terminal)
    // never lacks the address the phone needs.
    {
        let token = token.clone();
        let mut known = urls;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                let now = addresses(port, &token);
                if now == known {
                    continue;
                }
                if json_mode {
                    let ev = serde_json::json!({ "event": "urls", "urls": urls_json(&now) });
                    // Not println!: it panics if the supervisor's pipe is gone.
                    use std::io::Write;
                    let _ = writeln!(std::io::stdout(), "{ev}");
                } else {
                    println!("\n  addresses changed:");
                    print_urls(&now);
                }
                known = now;
            }
        });
    }

    web::serve(listener, token, hub, view_only).await;
}
