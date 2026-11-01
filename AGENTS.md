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
  `/tmp/opencode/ci.yml.bak`). Remote URL embeds the PAT (same token as
  `~/www/31ciptasolusi-landing`), so plain `git push` works — rotate that
  token when convenient and update both URLs.

## State (2026-10-20)

Pushed to https://github.com/muliaabdi/rdm (main). Extension v0.7.0.
Tag `v0.1.0` after CI lands.

Backlog: tray speed toggle, clipboard import, auto-retry, missing-file badge,
batch add, named queues/scheduler.

## Clean-Room

Tidak copy code dari IDM (Internet Download Manager). Hanya observasi black-box: run aplikasi, gunakan UI dengan synthetic workbooks, ambil screenshot (by window id) stored hanya di `plan/excel/screenshots/`. Tidak baca, disassemble, atau copy apa pun di dalam Office bundle, tidak commit files produced oleh Excel, tidak copy GPL/AGPL/LGPL code (LibreOffice, Gnumeric…). Function behaviour comes dari public documentation dan observation.

Dokumentasi feature parity dengan IDM (daftar fitur yang sudah ada vs belum) di `docs/feature-parity.md`.

## Never-Crash

People trust RDM dengan numbers mereka; crash loses their work. Ini outranks feature work.

- **No panics in non-test code:** no `unwrap()`, `expect()`, `panic!`, `unreachable!`, `todo!`, `unimplemented!`, `unsafe`
- **Errors are `Result<T, E>`** through crate's error type dan `?`
- **Input-derived numbers are hostile** (files, formulas, commands, MCP/control params): validate indexes dan ranges sebelum access, use checked access when validity is not established, use checked/saturating arithmetic, cap input-sized allocations (huge ranges, `REPT`, `SEQUENCE`, whole-column references).
- **Bound recursion** (formula nesting, dependency chains).
- **Last-resort guard:** Session::execute catches escaped panics dan restores workbook. Ini safety net, bukan lisensi.
- **Prove it:** every crash fix lands dengan regression test (`tests::every_command_survives_empty_params` fuzzes every command; `crates/engine/tests/it/command_sequence_fuzz.rs` fuzzes random command sequences against undo/redo, XLSX round-trip dan full-recalc invariants; soak dengan `GRIDCRAFT_FUZZ_SEEDS`/`GRIDCRAFT_FUZZ_STEPS`, replay dengan `GRIDCRAFT_FUZZ_SEED`).
- **Crates carry** `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]`.

## Non-Negotiables

- **Clean-room.** Tidak copy code dari IDM, hanya observasi black-box: run Excel, gunakan UI dengan synthetic workbooks, ambil screenshot (by window id) stored hanya di `plan/excel/screenshots/`. Tidak baca, disassemble, atau copy apa pun di dalam Office bundle, tidak commit files produced oleh Excel, tidak copy GPL/AGPL/LGPL code (LibreOffice, Gnumeric…). Function behaviour comes dari public documentation dan observation.
- **Fonts live di** [`storytold/craft-fonts`](https://github.com/storytold/craft-fonts), never di repo ini. Ini optional build input (`CRAFT_FONTS_DIR`); tanpa ini app uses system fonts dan egui's defaults.
- **Test corpora** (real Excel-authored files) live di repo terpisah, fetched pinned dan sha256-verified. Never commit large binary fixtures; build test workbooks in code.
- **Everything is a command.** User-visible behaviour = command di `crates/engine/src/cmd/*` (id, label, ribbon path, shortcut, params doc, `enabled`, `run`) + tests. Ids follow Excel's ribbon (`home.bold`, `insert.chart`, `data.sortAscending`) plus primitives (`cell.set`, `selection.set`, `range.setValues`). UI-only commands live di `crates/ui-egui`. Control channel, CLI dan MCP reach semua command.
- **Programmatic calls never open dialogs:** command dengan empty params runs dengan defaults atau returns error; hanya menu-style invocation opens dialog (`UiRequest::Dialog`).
- **Layering** is enforced oleh `cargo xtask layers`: `core`/`numfmt` (L0) → `formula`/`functions` (L1) → `model`/`calc`/`xlsx` (L2) → `chart` (L3) → `engine` (L5) → `ui-egui`/`mcp` (L6) → `apps/*` (L7). Nothing below L6 depends pada egui/eframe/winit/rfd. **The UI crate is swappable.**
- **The UI is thin:** panels read engine state dan act through `app.run(id, params)`. Colours come dari `theme::Tokens`.
- **Time goes through** `chrono` (workspace dependency). Never hand-roll UTC/calendar conversion (days-to-date algorithms, leap-year math). Crate owns hanya output format string. One exception adalah range extension: past years chrono can represent, shift date oleh whole 400-year Gregorian cycles (146,097 days, after which calendar repeats), let chrono compute month dan day, dan add cycles back ke year (splitting seconds ke hours, minutes dan seconds adalah plain arithmetic).
- **Rust only** (no handwritten JS/TS). **Never break wasm** (`cargo xtask wasm`).
- **Quality gates** before every commit: `cargo xtask ci` (fmt, clippy -D warnings, tests, assets, layers, wasm). One task id per commit (`M2.1: borders gallery`).
