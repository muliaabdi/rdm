# RDM — Architecture Documentation

## Overview
RDM (Rust Download Manager) adalah aplikasi download manager bergaya IDM yang terdiri dari beberapa crate Rust yang terpisah.

## Layering Structure

```
L0: rdm-core (numfmt)
    ↓ L1: formula/functions
L1: rdm-core (formula, functions)
    ↓ L2: model/calc/xlsx, chart, engine
L2: rdm-daemon (model, calc, xlsx, chart, engine)
    ↓ L3: rdm-ui (MVP)
L3: rdm-ui (Tauri webview)
    ↓ L4: rdm-host (native messaging)
L4: rdm-host (browser extension host)
```

## Crates

### rdm-core
- Segmented HTTP download engine
- HLS (HTTP Live Streaming) support
- Resume state management
- Basic utilities (numfmt)

### rdm-daemon
- Unix socket daemon (`$XDG_RUNTIME_DIR/rdm.sock`)
- Job database + settings (`~/.local/share/rdm/`)
- Extraction logic (yt-dlp, gallery-dl)
- Protocol handler (JSON lines)

### rdm-ui
- Tauri v2 shell
- Vanilla JS di single `ui/index.html` (no bundler; `withGlobalTauri`)
- Main window, tray menu, detail popup

### rdm-host
- Browser native-messaging host (`ext/install-host.sh`)
- Communicates dengan extension

## Extension (MV3)

### Files
- `ext/chrome/manifest.json` — background SW + content script
- `ext/chrome/background.js` — background service worker
- `ext/chrome/video-overlay.js` — content script untuk video metadata
- `ext/chrome/inject.js` — script untuk inject extension ke website

### Features
- Background SW untuk logging & caching
- Content script untuk mendeteksi video & extract metadata
- Context menu untuk download video/audio
- Chrome extension popup untuk settings

## Protocol (Daemon Socket)

Line-delimited JSON. Clients MUST terminate each message dengan `n` — tanpa itu daemon tidak reply.

## Extractor Ladder

1. **yt-dlp** — utama untuk video/audio dari YouTube, TikTok, dll
2. **gallery-dl** — fallback untuk gambar/galeri dari situs lain

Gallery jobs = one folder, resume skips files dengan size>0, rename `{:04}_{name}` when >1 file.

## Key Decisions

### Background Offers
Pill/context menu never raise main window: daemon `show-job` → Rust `show_detail_window()` opens/focuses compact always-on-top detail webview (label "detail", question-less; JS asks `detail_current` karena emit bisa race page load). Tray job items juga open hanya this popup.

### yt-dlp JavaScript Challenge (EJS)
Daemon needs Node in PATH. Node is nvm-only, so systemd drop-in sets it: `~/.config/systemd/user/rdm-daemon.service.d/runtime.conf` dengan `Environment=PATH=$HOME/.nvm/versions/node/<ver>/bin:...`. Tanpa ini: "n challenge solving failed" / "The page needs to be reloaded".

### Monotonic Progress
Page jobs set `size = filesize(video)+filesize(audio)` upfront (DB + phases carry `other_size`) jadi percent never wobbles 100→97→100 across mux phases.

### Speed Consistency
Graph bands + card use same smoothed series (`curSpeed()`; EMA per segment). Graph label = current, not peak.

### Extension UX
Menu renders Best+MP3 immediately (no waiting on extract), prefetches `page-formats` on pill appear, caches 120s success / 30s failure, de-dupes in-flight, matches replies dengan `url` (not FIFO).

### Video Detection
Walks `e.composedPath()` + shadow roots (Reddit faceplate/shreddit); `contextualPageUrl()` prefers `shreddit-post` `permalink` attribute / enclosing `/comments/` link over `location.href`.

### threads.com
Rewritten ke threads.net di `gallery.rs`; unsupported oleh both extractors sejak yt-dlp 2026.08.19 / gallery-dl 1.32.17.

### Delete Job
Permanent `remove_file`/`remove_dir_all`. RDM never uses Trash (any `~/.local/share/Trash/info/*.trashinfo` came dari outside RDM).

## Gotchas

- `ui/index.html` punya satu big `<script>`: duplicate top-level identifiers kill whole script silently ("connecting…" forever). Check dengan `awk '/<script>/{f=1;next}/<\/script>/{f=0}f' index.html > /tmp/x.js && node --check /tmp/x.js`.
- `pkill -f rdm-ui` bisa kill calling shell (pattern matches itself); gunakan `pkill -x rdm-ui`.
- gallery-dl `--dump-json` prints ONE JSON array [[idx, meta], ...] (index -1 = error), not json-lines; parser accepts kedua-duanya.
- CI (fmt + clippy `-D warnings` + test) is held: pushing `.github/workflows/ci.yml` needs PAT dengan `workflow` scope (backup: `/tmp/opencode/ci.yml.bak`). Remote URL contains PAT (same token sebagai `~/www/31ciptasolusi-landing`), jadi plain `git push` works — rotate that token saat convenient dan update kedua URL.

## Time Handling

All time operations go through `chrono` (workspace dependency). Never hand-roll UTC/calendar conversion (days-to-date algorithms, leap-year math). Crate owns hanya output format string.

## Rust Constraints

- No panics in non-test code: `unwrap()`, `expect()`, `panic!`, `unreachable!`, `todo!`, `unimplemented!` forbidden
- Errors expressed sebagai `Result<T, E>` through crate's error type dan `?`
- Input-derived numbers are hostile: validate indexes dan ranges sebelum access
- Bound recursion (formula nesting, dependency chains)
- Every crash fix includes regression test
- Crates carry `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]`