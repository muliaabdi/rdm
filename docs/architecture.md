# RDM — Architecture Documentation

## Overview

RDM (Rust Download Manager) adalah download manager bergaya IDM: segmented
engine Rust, daemon dengan queue persisten, UI Tauri, dan browser extension.

## Layering

```
L0: rdm-core     (engine segmented + HLS, tanpa dependensi crate RDM lain)
L1: rdm-daemon   (socket protocol, DB, extractor ladder; pakai rdm-core)
L2: rdm-ui / rdm-host / rdm-cli  (klien daemon; UI = Tauri v2 webview)
```

Tidak ada crate di bawah rdm-ui yang bergantung pada Tauri/webkit.

## Crates

### rdm-core
- Segmented HTTP download engine (dynamic splitting, work-stealing)
- HLS (m3u8): master playlist, AES-128, live polling
- Resume state (`*.rdm-part.json`, `*.rdm-hls.json`), kategori file,
  SHA-256, limiter (global + per-job), DoH resolver

### rdm-daemon
- Unix socket daemon (`$XDG_RUNTIME_DIR/rdm.sock`, mode 0600)
- SQLite job DB + settings (`~/.local/share/rdm/`)
- Modul: `settings` (settings.json + path), `protocol` (pesan socket),
  `jobs` (lifecycle + queue), `runners` (driver engine), `uilaunch`
- Extractor ladder: yt-dlp → gallery-dl → packer rescue (HLS packed JS)

### rdm-ui
- Tauri v2 shell, vanilla JS tanpa bundler (`withGlobalTauri`)
- `ui/index.html` + 5 file JS urut: `app-core`, `state`, `render`,
  `daemon-events`, `dialogs`
- Main window, tray, detail popup (webview always-on-top)

### rdm-host
- Browser native-messaging host (`ext/install-host.sh`), stdio ↔ socket

## Extension (MV3)

### Files
- `ext/chrome/manifest.json` — MV3 manifest
- `ext/chrome/background.js` — service worker (takeover, context menu)
- `ext/chrome/video-overlay.js` — content script (pill video/audio)
- `ext/chrome/popup.js` — popup UI

## Protocol (Daemon Socket)

Line-delimited JSON. Clients MUST terminate each message dengan `\n` —
tanpa itu daemon tidak pernah reply. Balasan dicocokkan berdasarkan
`url`/`t`, bukan urutan.

## Gotchas

- Duplicate top-level identifier di satu file JS membunuh file itu saja
  (silent). Check dengan `node --check crates/rdm-ui/ui/<file>.js`.
- `pkill -f rdm-ui` bisa kill calling shell; gunakan `pkill -x rdm-ui`.
- gallery-dl `--dump-json` prints ONE JSON array `[[idx, meta], ...]`
  (index -1 = error), bukan json-lines; parser menerima keduanya.
- CI (fmt + clippy `-D warnings` + test) ditahan soal PAT (backup
  `/tmp/opencode/ci.yml.bak`); remote URL embed PAT — rotate saat sempat.

Detail keputusan desain (background offers, monotonic progress, speed
consistency, video detection, threads.com, delete): lihat `AGENTS.md`
bagian Key decisions — itu sumber kebenaran, jangan diduplikasi di sini.
