# playdown-remote

Remote access companion for [Playdown](https://github.com/z-alamsyah/playdown) —
control your terminal sessions and AI agents from a phone browser.

- 📱 **Web terminal** — every Playdown session as a tab, with the same agent
  status at a glance (working / blocked / done), live output, full input
  (approve a Claude Code permission from your couch, or `vim` a file).
- ⚡ **Direct connection, no relay** — your phone talks straight to your
  laptop over LAN or [Tailscale](https://tailscale.com). Keystroke latency is
  your network RTT, nothing more.
- 🔒 **Token-gated** — a fresh token per run, shown as a QR code in the
  terminal. Scan → connected. No accounts, no cloud, no telemetry.
- 🪶 **One small binary** — xterm.js is embedded; the page makes zero
  external requests.
- 🤖 **Telegram bot (optional)** — your own bot pings you when an agent
  blocks or finishes, with inline keys to answer without opening anything.

## Setup

1. In Playdown: **Settings → Terminal & agents → Remote bridge: On**
   (this exposes a same-user local socket at `~/.playdown/bridge.sock`).
2. Run the companion on the same machine:

   ```bash
   playdown-remote
   ```

3. Scan the QR (or open the printed URL) on your phone — same Wi-Fi, or any
   network if both devices are on Tailscale.

```
USAGE: playdown-remote [OPTIONS]
  --port <PORT>           HTTP port (default: 7423)
  --socket <PATH>         Playdown bridge socket (default: ~/.playdown/bridge.sock)
  --view-only             Disable input from remote clients
  --telegram <TOKEN>      Enable the Telegram bot (or env TELEGRAM_BOT_TOKEN)
  --telegram-chat <ID>    Allowlisted chat id (or env TELEGRAM_CHAT_ID)
```

## Telegram bot (optional)

Get pinged the moment an agent needs you — no relay, long polling against
`api.telegram.org` with **your own bot**:

1. Message [@BotFather](https://t.me/BotFather) → `/newbot` → copy the token.
2. First run (pairing): `playdown-remote --telegram <token>`, then message
   your bot anything — it replies with your chat id.
3. Real run:

   ```bash
   playdown-remote --telegram <token> --telegram-chat <chat_id>
   # or: TELEGRAM_BOT_TOKEN=… TELEGRAM_CHAT_ID=… playdown-remote
   ```

You get: 🔴 *"session needs you"* alerts with the last lines of output and
inline **Enter / Esc / 1 / 2 / 3 / ^C** keys, 🟢 done notifications, plus
`/status`, `/send <n> <text>`, and `/tail <n>`. Only the allowlisted chat id
can interact — every other chat is ignored. With `--view-only` the bot only
notifies; it can't type.

## Security model

- The bridge socket is same-user only (`0600`); this program re-exposes it
  over HTTP **guarded by a per-run random token** (128-bit, in the URL
  fragment — it never appears in server logs).
- Traffic is plain HTTP on your LAN. On networks you don't own, use
  **Tailscale** (WireGuard-encrypted, peer-to-peer) — don't port-forward this
  to the open internet.
- `--view-only` for read-only monitoring.

## How it works

```
phone browser ── WS (token) ──► playdown-remote ── unix socket ──► Playdown
   xterm.js                        (this binary)   BRIDGE_PROTOCOL     PTYs
```

See [`BRIDGE_PROTOCOL.md`](https://github.com/z-alamsyah/playdown/blob/main/BRIDGE_PROTOCOL.md)
for the socket contract — you can build your own companions on it.

## Install

One-liner (macOS arm64/x64, Linux x64/arm64 — Jetson included):

```bash
curl -fsSL https://raw.githubusercontent.com/z-alamsyah/playdown-remote/main/install.sh | sh
```

Puts the binary in `/usr/local/bin` (or `~/.local/bin` if not writable).
Then just run:

```bash
playdown-remote
```

<details>
<summary>Manual install / build from source</summary>

Download the tarball for your platform from
[Releases](https://github.com/z-alamsyah/playdown-remote/releases/latest), then:

```bash
tar xzf playdown-remote-v*-<platform>.tar.gz
sudo mv playdown-remote /usr/local/bin/   # or anywhere on your PATH
playdown-remote
```

Or build from source:

```bash
cargo build --release   # → target/release/playdown-remote
```
</details>

## License

MIT — see [LICENSE](LICENSE). "Playdown" name & logo are trademarks of
z-alamsyah; see the main repo's NOTICE.
