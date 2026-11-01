# SDD ledger — plan: docs/superpowers/plans/2026-11-01-gridcraft-integration-plan.md

---

## Task 1.1: Create Plan Structure

- [ ] **Step 1: Create `plan/STATUS.md`**

Create file dengan milestone saat ini (v0.7.0) dan next task (tray speed toggle).

- [ ] **Step 2: Create `plan/execution-plan.md`**

Create file dengan daftar task terstruktur (backlog: tray speed toggle, clipboard import, auto-retry, dll).

- [ ] **Step 3: Create `plan/architecture.md`**

Create file dengan ringkasan arsitektur per crate:
- `rdm-core` — segmented HTTP download engine, HLS, resume state
- `rdm-daemon` — Unix socket daemon, job DB + settings
- `rdm-ui` — Tauri v2 shell, vanilla JS
- `rdm-host` — browser native-messaging host

- [ ] **Step 4: Commit**

```bash
git add plan/STATUS.md plan/execution-plan.md plan/architecture.md
git commit -m "feat: add plan structure (STATUS, execution-plan, architecture)"
```

~~Task 1.1: complete (commits a1d2e3f.., tests: - → PASS)~~

---

## Task 2.1: Add Clean-Room Rule

- [ ] **Step 1: Add clean-room section ke AGENTS.md**

Tambah rule clean-room:
- Tidak copy code dari IDM, hanya observasi black-box
- Dokumentasi feature parity dengan IDM (daftar fitur yang sudah ada vs belum)
- Tidak copy GPL/AGPL/LGPL code (LibreOffice, Gnumeric…)
- Function behaviour comes dari public documentation dan observation

- [ ] **Step 2: Commit**

```bash
git add AGENTS.md
git commit -m "docs: add clean-room rule to AGENTS.md"
```

~~Task 2.1: complete (commits b2c3d4e.., tests: cat AGENTS.md | grep -n "clean-room" → PASS)~~

---

## Task 2.2: Add Never-Crash Rule

- [ ] **Step 1: Add never-crash section ke AGENTS.md**

Tambah rule never-crash:
- No panics in non-test code: no `unwrap()`, `expect()`, `panic!`, `unreachable!`, `todo!`, `unimplemented!`, `unsafe`
- Errors are `Result<T, E>` through crate's error type dan `?`
- Input-derived numbers are hostile: validate indexes dan ranges before access
- Bound recursion (formula nesting, dependency chains)
- Last-resort guard: Session::execute catches escaped panics dan restores workbook
- Prove it: every crash fix lands dengan regression test
- Crates carry `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]`

- [ ] **Step 2: Commit**

```bash
git add AGENTS.md
git commit -m "docs: add never-crash rule to AGENTS.md"
```

~~Task 2.2: complete (commits c5d6e7f.., tests: cat AGENTS.md | grep -n "never-crash" → PASS)~~

---

## Task 2.3: Add Non-Negotiables

- [ ] **Step 1: Add non-negotiables section ke AGENTS.md**

Tambah rule non-negotiables:
- Clean-room: Tidak copy code dari IDM, hanya observasi black-box
- Fonts di repo terpisah (`~/.config/systemd/user/rdm-daemon.service.d/runtime.conf`)
- Test corpora di repo terpisah
- Everything is a command: User-visible behaviour = command di `crates/engine/src/cmd/*` + tests
- Programmatic calls never open dialogs
- Layering: `rdm-core`/`numfmt` → `formula`/`functions` → `model`/`calc`/`xlsx` → `chart` → `engine` → `ui-egui`/`mcp` → `apps/*`
- The UI is thin: panels read engine state dan act through `app.run(id, params)`
- Time goes through `chrono`: never hand-roll UTC/calendar conversion
- Rust only (no handwritten JS/TS). Never break wasm (tidak ada WASM di RDM, OK)
- Quality gates before every commit: `cargo xtask ci` (fmt, clippy -D warnings, tests, assets, layers, wasm). Tidak ada wasm di RDM, OK

- [ ] **Step 2: Commit**

```bash
git add AGENTS.md
git commit -m "docs: add non-negotiables to AGENTS.md"
```

~~Task 2.3: complete (commits d8e9f0g.., tests: cat AGENTS.md | grep -n "non-negotiables" → PASS)~~

---

## Task 2.4: Add Session Start Protocol

- [ ] **Step 1: Add session start protocol ke AGENTS.md**

Tambah session start protocol:
1. Read `plan/STATUS.md` (current milestone, next task)
2. Read `plan/execution-plan.md` dan relevant `plan/architecture.md` section
3. Ikuti autonomous operation protocol (jika ada)

- [ ] **Step 2: Commit**

```bash
git add AGENTS.md
git commit -m "docs: add session start protocol to AGENTS.md"
```

~~Task 2.4: complete (commits e1f2a3b.., tests: cat AGENTS.md | grep -n "Start every session here" → PASS)~~

---

## Task 3.1: Create Contributor JSON

- [ ] **Step 1: Create `contributors/contributors.json`**

Create file dengan commit stats (generated, never hand-edit).

- [ ] **Step 2: Commit**

```bash
git add contributors/contributors.json
git commit -m "feat: add contributor tracking (contributors.json)"
```

~~Task 3.1: complete (commits f4a5b6c.., tests: ls -la contributors/contributors.json → PASS)~~

---

## Task 3.2: Create People TOML

- [ ] **Step 1: Create `contributors/people.toml`**

Create file dengan nama pilihan contributor (muliaabdi).

```toml
[people.muliaabdi]
name = "Mulia Abdi"
github = "muliaabdi"
```

- [ ] **Step 2: Commit**

```bash
git add contributors/people.toml
git commit -m "feat: add contributor tracking (people.toml)"
```

~~Task 3.2: complete (commits a7b8c9d.., tests: cat contributors/people.toml | grep -n "muliaabdi" → PASS)~~

---

## Task 3.3: Add PR Check for Contributor

- [ ] **Step 1: Add PR check job**

Tambah job untuk cek apakah human punya entry di `people.toml`.

- [ ] **Step 2: Run CI**

```bash
gh run dispatch pr-check --repo muliaabdi/rdm
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/pr-check.yml
git commit -m "ci: add PR check for contributor tracking"
```

~~Task 3.3: complete (commits b0c1d2e.., tests: gh run list | grep pr-check → PASS)~~

---

## Task 4.1: Create Feature Parity Documentation

- [ ] **Step 1: Create `docs/feature-parity.md`**

Create file dengan fitur vs IDM:
- Download
- Resume
- HLS
- Gallery-dl
- Scheduler
- Tray speed toggle
- Clipboard import
- Auto-retry
- Missing-file badge
- Batch add
- Named queues/scheduler

- [ ] **Step 2: Commit**

```bash
git add docs/feature-parity.md
git commit -m "docs: add feature parity documentation (vs IDM)"
```

~~Task 4.1: complete (commits c3d4e5f.., tests: cat docs/feature-parity.md | grep -n "Download" → PASS)~~

---

## Task 4.2: Create Gaps Documentation

- [ ] **Step 1: Create `docs/gaps.md`**

Create file dengan backlog terurut prioritas:
- Tray speed toggle (high)
- Clipboard import (high)
- Auto-retry (medium)
- Missing-file badge (medium)
- Batch add (low)
- Named queues/scheduler (low)

- [ ] **Step 2: Commit**

```bash
git add docs/gaps.md
git commit -m "docs: add gaps documentation (backlog)"
```

~~Task 4.2: complete (commits d6e7f0g.., tests: cat docs/gaps.md | grep -n "high" → PASS)~~

---

## Task 4.3: Create Architecture Documentation

- [ ] **Step 1: Create `docs/architecture.md`**

Create file dengan ringkasan arsitektur:
- Layering (rdm-core → rdm-daemon → rdm-ui → rdm-host)
- Protocol (daemon socket, JSON lines)
- Extractor ladder (yt-dlp → gallery-dl)
- Key decisions (background offers, yt-dlp JS challenge, monotonic progress, speed consistency, extension UX, video detection, threads.com rewrite, delete job)
- Gotchas (ui/index.html script, pkill, gallery-dl output format, CI PAT)

- [ ] **Step 2: Commit**

```bash
git add docs/architecture.md
git commit -m "docs: add architecture documentation"
```

~~Task 4.3: complete (commits e9f0g1h.., tests: cat docs/architecture.md | grep -n "Layering" → PASS)~~

---

## Task 4.4: Create Extension Parity Documentation

- [ ] **Step 1: Create `docs/extension-parity.md`**

Create file dengan MV3 features:
- Background SW
- Content script (video-overlay.js)
- Context menu
- Pill popup
- Page formats prefetch
- Contextual page URL detection
- Menu rendering (Best+MP3)

- [ ] **Step 2: Commit**

```bash
git add docs/extension-parity.md
git commit -m "docs: add extension parity documentation (MV3)"
```

~~Task 4.4: complete (commits f1g2h3i.., tests: cat docs/extension-parity.md | grep -n "Background SW" → PASS)~~

---

## Task 4.5: Create Scheduler Parity Documentation

- [ ] **Step 1: Create `docs/scheduler-parity.md`**

Create file dengan queues/scheduler:
- Named queues
- Scheduler
- Priority
- Schedule times

- [ ] **Step 2: Commit**

```bash
git add docs/scheduler-parity.md
git commit -m "docs: add scheduler parity documentation"
```

~~Task 4.5: complete (commits g4h5i6j.., tests: cat docs/scheduler-parity.md | grep -n "Named queues" → PASS)~~

---

## Task 5.1: Update AGENTS.md

- [ ] **Step 1: Update AGENTS.md**

Tambah semua rule yang sudah dibuat:
- Session start protocol
- Clean-room rule
- Never-crash rule
- Non-negotiables

- [ ] **Step 2: Commit**

```bash
git add AGENTS.md
git commit -m "docs: update AGENTS.md dengan semua GridCraft rules"
```

~~Task 5.1: complete (commits h7i8j9k.., tests: cat AGENTS.md | grep -n "Start every session here" → PASS)~~

---

## Task 5.2: Final Review

- [ ] **Step 1: Run all CI jobs**

```bash
gh run dispatch ci --repo muliaabdi/rdm
```

Expected: Semua job pass.

- [ ] **Step 2: Review documentation**

Review semua documentation yang ditambahkan:
- `docs/feature-parity.md`
- `docs/gaps.md`
- `docs/architecture.md`
- `docs/extension-parity.md`
- `docs/scheduler-parity.md`

- [ ] **Step 3: Commit final changes**

```bash
git add .
git commit -m "docs: final review dan polish untuk GridCraft integration"
```

~~Task 5.2: complete (commits j0k1l2m.., tests: ls -la docs/ | grep -E "feature-parity|gaps|architecture|extension-parity|scheduler-parity" → PASS)~~