# Open IPMsg

> **简体中文版 → [README.zh-CN.md](README.zh-CN.md)**

Open IPMsg is a cross-platform LAN instant messenger compatible with [IP Messenger (IPMsg)](https://ipmsg.org/) ("Feige"/飞鸽传书), built with **Tauri v2** and **Vue 3**. The UI is modeled after the WeChat PC client: a three-column layout (sidebar / contact list / chat window), green message bubbles, and a custom frameless title bar.

- **Rust backend** — a complete implementation of the IPMsg UDP/TCP protocol (port 2425) with no third-party protocol libraries
- **Vue 3 frontend** — WeChat-PC-style UI, all vector icons and procedural avatars, zero external image assets
- **Windows · macOS · Linux** — cross platform

---

## Highlights

- **Works with the official client and other open-source IPMsg implementations** — discover, message, and transfer files with official IPMsg, iptux, golang-ipmsg, and more on the same network
- **End-to-end encryption** — RSA-2048 key exchange + AES-256-CBC message encryption + SHA-256 signing, plus AES-CTR encrypted TCP file streams; automatically falls back to plaintext for peers that don't support encryption
- **Files & folders** — multi-file attachments, resumable TCP streaming with progress, and recursive folder transfer (both directions)
- **Clipboard paste image** — natively compatible with the official "paste image" feature (FILE_CLIPBOARD), both directions: send screenshots with Ctrl+V and preview images pasted by official clients inline
- **Read receipts** — see when your messages are read, with per-bubble read/unread status
- **Tray & notifications** — closing the window minimizes to the tray (WeChat-style); system notifications when messages arrive while unfocused
- **Bilingual UI** — Simplified Chinese / English, switchable at runtime

---

## Features

| Area | What you get |
| --- | --- |
| Contact discovery | Automatic LAN discovery (broadcast), presence (online/offline), 45s refresh, 30min timeout cleanup |
| Messaging | Text messages, Unicode/emoji, message deduplication |
| Files | Multi-file attachments, TCP streaming, progress, resume, folder transfer (both directions) |
| Paste image | Send screenshots via Ctrl+V; inline preview of images pasted by official clients |
| Read receipts | READMSG-based receipts with read/unread bubbles |
| Groups | Contact list grouped by broadcast group |
| Away mode | "Away" broadcast with auto-reply, configurable away status text |
| Recall / secret | Recall your own text messages (DELMSG); secret messages (SECRETEX) that only mark as read when opened; password lock (PASSWORDOPT) |
| Broadcast / multicast | Broadcast to the whole network without receipts; multi-select group send |
| Member master (IPDict) | Directory service with signed full-network member lists (RSA-2048/SHA-256) |
| NAT proxy | Relay through a configured proxy address |
| IPv6 | Group-multicast member discovery (`ff15::979` / `ff02::1`), auto-falls back to IPv4 |
| Encoding | Send in UTF-8 or GBK (for older Chinese clients); auto-detect on receive |
| Privacy & crypto | E2E message and file encryption with visible key fingerprints |
| More | Emoji panel, unread badges, search, custom download directory, open-in-file-manager |

---

## Installation

Prebuilt installers are published on the [Releases](https://github.com/xspeed1989/open_ipmsg/releases) page when a release is cut:

- **Windows**: `.msi` / `.exe` installer (requires WebView2, built into Windows 11)
- **macOS**: `.dmg` / `.app` bundle (requires Xcode Command Line Tools)
- **Linux**: `.deb` / `.rpm` / `.AppImage` packages; Arch Linux users can also use the `.pkg.tar.zst` package

> Want a fresh build from source? See [Building from source](#building-from-source) below.

---

## Quick start

1. **First launch**: a setup dialog asks for your **nickname** (required) and **group** (optional). Saving broadcasts your presence to the LAN.
2. **Contacts**: other IPMsg clients on your network (official Windows/Mac client, iptux, etc.) appear in the contact list — a green dot means online.
3. **Chat**: click a contact to start a conversation. **Enter** sends, **Ctrl+Enter** inserts a newline. Your own messages show read/unread status underneath.
4. **Send files**: use the 📎 toolbar button; for incoming files click **Download**, then **Open** or reveal in folder.
5. **Tray**: closing the window minimizes to the tray; left-click the tray icon to restore, use **Exit** in the tray menu to truly go offline.
6. **Notifications**: when the window is unfocused, incoming messages trigger a system notification; the contact shows an unread badge.
7. **Download folder**: received files default to `接收文件` inside the app data directory — changeable in Settings.

---

## Building from source

Requirements: Rust 1.77+, Node 18+, pnpm (or npm).

```bash
pnpm install
pnpm tauri dev        # development mode (hot reload)
pnpm tauri build      # packaged bundles in src-tauri/target/release/bundle/
```

System dependencies for Linux (Debian/Ubuntu example, same package names on Arch/Manjaro):

```bash
sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file \
  libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev patchelf
```

See the [Tauri v2 prerequisites](https://tauri.app/start/prerequisites/) for platform-specific setup (WebView2 on Windows, Xcode CLT on macOS).

---

## FAQ

**Q: My messages show as garbled text (乱码) on older Chinese clients.**
Older Chinese IPMsg clients default to GBK encoding. Receiving is auto-detected, so nothing to configure there — if *sending* shows garbled, switch **Settings → Send encoding** to GBK.

**Q: File transfer fails or times out.**
File transfer uses **TCP port 2425** (discovery/messages use UDP 2425 — both must be allowed). The first time it runs, Windows shows a firewall prompt — choose **Allow**; on Linux make sure your firewall isn't blocking inbound TCP 2425.

**Q: A contact appears twice (v4/v6 duplicate entries) and interop breaks.**
You can turn off **IPv6 multicast discovery** in Settings.

**Q: How do I verify encryption is active?**
Both sides show a key fingerprint in Settings — compare them to confirm the same key pair.

**Q: Where are my chat records stored?**
Chat history is stored in the app data directory, one file per conversation; recent records load automatically when you open a session.

---

## Roadmap

- [x] RSA-2048/AES key exchange and encrypted messaging + encrypted TCP file streams
- [x] Recursive folder transfer (both directions)
- [x] Broadcast mode (BROADCASTOPT)
- [ ] Auto-start on boot, send screenshots

---

## License

No license has been chosen for this project yet. See the repository for details; contact the maintainers if you intend to distribute or modify it.