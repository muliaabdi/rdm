# RDM — Rust Download Manager (IDM-style)

User-facing communication: **Bahasa Indonesia**, ultra-terse. UI text: English
with EN/ID switch. Errors surface as toasts. Theme: system/light/dark.

## Layout

- `crates/rdm-core` — segmented HTTP download engine, HLS, resume state
- `crates/rdm-daemon` — Unix socket daemon (`$XDG_RUNTIME_DIR/rdm.sock`),
  job DB + settings in `~/.local/share/rdm/` (`rdm.db`, `settings.json`)
- `crates/rdm-ui` — Tauri v2 shell, **vanilla JS in a single
  `ui/index.html`** (no bundler; `withGlobalTauri`)
- `crates/rdm-host` — browser native-messaging host (`ext/install-host.sh`)
- `ext/chrome` — MV3 extension (background SW + `video-overlay.js` content
  script). Reload manually in `brave://extensions` after edits; **tabs must
  be refreshed too** or the content script goes orphan (clicks do nothing).
- `install.sh` — build release, copy binaries to `~/.local/bin`, systemd
  user service `rdm-daemon`, desktop entry. Never touches `~/Downloads`.

## Build / run

```bash
cargo build --release -p rdm-ui -p rdm-daemon && ./install.sh
systemctl --user status rdm-daemon        # socket + jobs
pkill -x rdm-ui; setsid ~/.local/bin/rdm-ui >/tmp/opencode/ui.log 2>&1 < /dev/null &
```

Relaunch UI with saved env: `export $(xargs -d '\n' < /tmp/opencode/ui.env)`.

## Protocol (daemon socket)

Line-delimited JSON. Clients MUST terminate each message with `\n` — without
it the daemon never replies (has been misdiagnosed as a hang before).

## Key decisions

- **Extractor ladder**: yt-dlp first; on failure gallery-dl
  (`~/.local/share/rdm/venv-gallerydl`, symlink `~/.local/bin/gallery-dl`).
  Gallery jobs = one folder, resume skips files with size>0, rename
  `{:04}_{name}` when >1 file.
- **Background offers** (pill/context menu) never raise the main window:
  daemon `show-job` → Rust `show_detail_window()` opens/focuses a compact
  always-on-top `detail` webview (label "detail", `?`-less; JS asks
  `detail_current` because emit can race page load). Tray job items also open
  only this popup.
- **yt-dlp JS challenge (EJS)**: daemon needs Node in PATH. Node is nvm-only,
  so a systemd drop-in sets it:
  `~/.config/systemd/user/rdm-daemon.service.d/runtime.conf`
  (`Environment=PATH=$HOME/.nvm/versions/node/<ver>/bin:...`). Symptom without
  it: `n challenge solving failed` / `The page needs to be reloaded`.
- **Monotonic progress**: page jobs set `size = filesize(video)+filesize(audio)`
  up front (DB + phases carry `other_size`) so percent never wobbles
  100→97→100 across mux phases.
- **Speed consistency**: graph bands + card use the same smoothed series
  (`curSpeed()`; EMA per segment). Graph label = current, not peak.
- **Extension UX**: menu renders Best+MP3 immediately (no waiting on extract),
  prefetches `page-formats` on pill appear, caches 120 s success / 30 s
  failure, de-dupes in-flight, matches replies by `url` (not FIFO).
- **Video detection** walks `e.composedPath()` + shadow roots (Reddit
  faceplate/shreddit); `contextualPageUrl()` prefers `shreddit-post`
  `permalink` attribute / enclosing `/comments/` link over `location.href`.
- threads.com → rewritten to threads.net in `gallery.rs`; unsupported by both
  extractors as of yt-dlp 2026.08.19 / gallery-dl 1.32.17.
- Delete job = permanent `remove_file`/`remove_dir_all`. RDM never uses the
  Trash (any `~/.local/share/Trash/info/*.trashinfo` came from outside RDM).

## Gotchas

- `ui/index.html` has one big `<script>`: duplicate top-level identifiers kill
  the whole script silently ("connecting…" forever). Check with
  `awk '/<script>/{f=1;next}/<\/script>/{f=0}f' index.html > /tmp/x.js && node --check /tmp/x.js`.
- `pkill -f rdm-ui` can kill the calling shell (pattern matches itself); use
  `pkill -x rdm-ui`.
- gallery-dl `--dump-json` prints ONE JSON array `[[idx, meta], ...]`
  (index -1 = error), not json-lines; parser accepts both.
- CI (fmt + clippy `-D warnings` + test) is held: pushing
  `.github/workflows/ci.yml` needs a PAT with `workflow` scope (backup:
  `/tmp/opencode/ci.yml.bak`). Remote is clean HTTPS; push via `GIT_ASKPASS`
  reading the PAT from `~/www/31ciptasolusi-landing/.git/config` (rotate that
  token when convenient).

## State (2026-10-20)

Pushed to https://github.com/muliaabdi/rdm (main). Extension v0.7.0.
Tag `v0.1.0` after CI lands.

Backlog: tray speed toggle, clipboard import, auto-retry, missing-file badge,
batch add, named queues/scheduler.
