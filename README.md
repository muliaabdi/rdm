# RDM — Rust Download Manager

Download manager for the Linux desktop: a segmented Rust engine, a daemon
with a persistent queue, a Tauri UI, and a browser extension that takes
over browser downloads and detects videos on the page.

![status](https://img.shields.io/badge/status-beta-orange)

## Features

- **Segmented downloads** over multiple connections with dynamic splitting /
  work-stealing (idle connections steal remaining bytes from slow ones),
  cross-URL resume (Range/ETag), and per-job segment count.
- **Persistent queue** (SQLite) with a parallel-download limit; instant
  pause/resume; automatic reconnect; partial-file cleanup on delete.
- **Auto-retry** of transient failures (3x, 5s/15s/45s backoff): connection
  resets, short reads, HTTP 408/425/429/5xx — permanent errors such as 403/404
  fail immediately. Retries resume from the saved partial state.
- **Global speed limit** (KiB/s, 0 = unlimited) applied to all active
  downloads at once, including HLS segments and yt-dlp media; adjustable
  while downloads are running (UI or tray menu).
- **Category folders**: each category (images/videos/audio/documents/apps/
  archives/others) can have its own folder; downloads are routed
  automatically by file extension (relative paths resolve against the main
  folder).
- **Chrome/Brave/Edge/Chromium extension (MV3)**:
  - native browser download takeover (cancel + RDM confirmation dialog),
  - per-extension file-type filters (categories + custom list),
  - right-click a link/media → download via RDM,
  - overlay pill on `<video>`/`<audio>` elements (with an audio-only MP3
    option),
  - browser cookies + User-Agent forwarded to the engine (downloads that
    need a login keep working),
  - fallback for URLs without an extension (e.g. Google Drive): the decision
    is deferred until the real filename is known.
- **Video sites** via `yt-dlp` (1,700+ extractors): pick resolution/audio,
  automatic retry ladder (cookie+UA → no cookie → web client); daily
  self-update in the background (`yt-dlp -U`, can be disabled in Settings;
  system packages only record the version).
- **Galleries** via `gallery-dl` (pixiv, danbooru, imgur albums, …): one
  folder per job, resume skips non-empty files, `{:04}_{name}` renaming
  when there is more than one file.
- **HLS** (`m3u8`): master playlists (highest-bandwidth variant, nested up
  to 4 levels), AES-128 (explicit IV / media-sequence), multi-init fMP4,
  live polling; `.m3u8` → `.ts`.
- **Clipboard import**: watched in the background; a copied download or
  video-page URL pops an offer card (toggle in Settings).
- **Desktop UI**: list + categories + per-segment graph, detail dialog with
  exact-file deletion info, missing-file badges on completed jobs whose file
  disappeared, tray icon with active downloads (click = focus + select
  row) and a speed-limit submenu, completion/failure notifications, EN/ID.
- **Daemon** as a systemd user service (auto-start + auto-restart),
  reachable over the unix socket `$XDG_RUNTIME_DIR/rdm.sock` (mode 0600).

## Architecture

```
crates/
  rdm-core    segmented engine + HLS
  rdm-daemon  SQLite queue, JSON-lines protocol over a unix socket, yt-dlp
  rdm-host    native messaging host (stdio <-> socket)
  rdm-ui      desktop UI (Tauri v2, static framework-less web UI)
  rdm-cli     CLI (rdm add/list/pause/...)
ext/chrome    browser extension (MV3, no build step)
```

User data lives in `~/.local/share/rdm/` (`rdm.db`, `settings.json`).

## Install

Requirements: Rust (stable), GTK/webkit2gtk-4.1 (Tauri), `ffmpeg`, `yt-dlp`.

```sh
./install.sh            # release build + ~/.local/bin + systemd + .desktop
```

Then load the extension: `brave://extensions` (or `chrome://extensions`) →
Developer mode → **Load unpacked** → pick the `ext/chrome` folder.

Without the installer: `cargo build --release`, then run `rdm-daemon` and
`rdm-ui`; register the native host manifest via `ext/install-host.sh`.

## Usage

- From the browser: click a normal download → RDM takes over (optionally
  confirmed via dialog); or right-click → "Download with RDM".
- Videos: the pill above the player, or Add URL → Detect.
- CLI: `rdm add <url>`, `rdm list`, `rdm pause <id>`, `rdm resume <id>`.
- Settings (UI): save folder, category folders, segments, parallel
  downloads, speed limit, file types to take over, clipboard watching,
  language, notifications, autostart, automatic yt-dlp updates.

## Limitations

- One video per URL (playlists are not supported yet).
- HLS: `BYTERANGE`/`SAMPLE-AES` are rejected; resuming a live stream only
  continues with new segments (segments missed while paused cannot be
  recovered). DRM (Widevine) content is out of scope.
- Pause cannot interrupt the ffmpeg mux phase (usually < 2 seconds).
- Cookies are kept on active jobs for resume purposes and removed once the
  job reaches a terminal state.
- MV3 extension (Chrome family). No Firefox build yet.

## Development

```sh
cargo test --workspace   # 26 tests (engine, HLS, queue)
cargo fmt
cargo clippy --workspace
```

## License

GPL-3.0-or-later.
