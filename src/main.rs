//! playdown-remote — remote access companion for Playdown.
//!
//! Connects to Playdown's local bridge socket (see BRIDGE_PROTOCOL.md in the
//! playdown repo) and serves a mobile web terminal on your LAN/Tailscale:
//! session tabs with agent status, live output, and input — protected by a
//! per-run token (QR printed on startup).

mod bridge;
mod web;

use rand::RngCore;

fn help() -> ! {
    println!(
        "playdown-remote — phone access to your Playdown terminal sessions\n\n\
         USAGE: playdown-remote [OPTIONS]\n\n\
         OPTIONS:\n  \
         --port <PORT>      HTTP port (default: 7423)\n  \
         --socket <PATH>    Playdown bridge socket (default: ~/.playdown/bridge.sock)\n  \
         --view-only        Disable input from remote clients\n  \
         --help             Show this help\n\n\
         Enable the bridge first: Playdown → Settings → Terminal & agents → Remote bridge."
    );
    std::process::exit(0);
}

#[tokio::main]
async fn main() {
    let mut port: u16 = 7423;
    let mut socket: Option<String> = None;
    let mut view_only = false;

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--port" => port = args.next().and_then(|v| v.parse().ok()).unwrap_or(7423),
            "--socket" => socket = args.next(),
            "--view-only" => view_only = true,
            "--help" | "-h" => help(),
            _ => {}
        }
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

    println!("playdown-remote v{}", env!("CARGO_PKG_VERSION"));
    println!("bridge: {socket_path}");
    if view_only {
        println!("mode:   VIEW ONLY (remote input disabled)");
    }
    println!();

    let mut urls = Vec::new();
    if let Ok(ip) = local_ip_address::local_ip() {
        urls.push(format!("http://{ip}:{port}/#t={token}"));
    }
    if let Ok(ifaces) = local_ip_address::list_afinet_netifas() {
        for (name, ip) in ifaces {
            // Tailscale interfaces (100.x.y.z) get their own line.
            if ip.is_ipv4() && ip.to_string().starts_with("100.") {
                urls.push(format!("http://{ip}:{port}/#t={token}  ({name}/tailscale)"));
            }
        }
    }
    urls.dedup();
    for u in &urls {
        println!("  {u}");
    }
    if let Some(first) = urls.first() {
        let plain = first.split_whitespace().next().unwrap_or(first);
        if let Ok(code) = qrcode::QrCode::new(plain.as_bytes()) {
            let art = code
                .render::<qrcode::render::unicode::Dense1x2>()
                .quiet_zone(true)
                .build();
            println!("\n{art}");
        }
    }
    println!("\nOpen the URL on your phone (same LAN or Tailscale). Ctrl+C to stop.");

    web::serve(listener, token, hub, view_only).await;
}
