# GridCraft Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans untuk implementasi ini task-by-task. Steps menggunakan syntax checkbox (`- [ ]`) untuk tracking.

**Goal:** Mengadopsi struktur & best practices dari GridCraft untuk dokumentasi & rules RDM (Rust Download Manager).

**Architecture:** Copy struktur plan/, dokumentasi rules (clean-room, never crash, non-negotiables), dan contributor tracking dari GridCraft.

**Tech Stack:** Git, Markdown

**Spec:** `docs/superpowers/specs/2026-11-01-gridcraft-integration-design.md`

---

## Global Constraints

- User-facing communication: Bahasa Indonesia, ultra-terse. UI text: English with EN/ID switch. Errors surface as toasts. Theme: system/light/dark.
- Layering: `rdm-core` → `rdm-daemon` → `rdm-ui` → `rdm-host`
- UI: vanilla JS di single `ui/index.html` (no bundler; `withGlobalTauri`)
- Extension: MV3 (background SW + `video-overlay.js` content script)
- CI: GitHub Actions dengan PAT workflow scope
- Never crash: No panics in non-test code, no `unwrap()`, `expect()`, `panic!`, `todo!`, `unreachable!`, `unsafe`
- Errors are `Result<T, E>` through crate's error type and `?`
- Input-derived numbers are hostile: validate indexes and ranges before access
- Bound recursion (formula nesting, dependency chains)
- Prove it: every crash fix lands with regression test
- Clippy deny list: `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]`
- Clean-room: Tidak copy code dari IDM, hanya observasi black-box
- Fonts di repo terpisah atau pakai SVG inline
- Test corpora di repo terpisah atau build test workbooks in code
- Everything is a command: User-visible behaviour = command di `crates/engine/src/cmd/*` + tests
- Time goes through `chrono` (workspace dependency). Never hand-roll UTC/calendar conversion.
- Rust only (no handwritten JS/TS). Never break wasm (tidak ada WASM di RDM, OK).
- Quality gates before every commit: `cargo xtask ci` (fmt, clippy -D warnings, tests, assets, layers, wasm). Tidak ada wasm di RDM, OK.

---

## Review Focus

1. **Socket connection failure** — Daemon tidak terhubung saat UI mencoba kirim command → error toasts, bukan crash
2. **Invalid JSON input** — Client mengirim JSON yang salah format → daemon reply error, bukan panic
3. **Corrupted DB** — `rdm.db` rusak → daemon recovery, bukan crash
4. **Empty params** — Command dengan empty params → default values atau error, bukan crash
5. **Huge ranges** — Range besar (SEQUENCE, REPT, whole-column) → saturating arithmetic, bukan overflow

---

## Phase 1: Plan Structure

### Task 1.1: Create Plan Structure

**Files:**
- Create: `plan/STATUS.md`
- Create: `plan/execution-plan.md`
- Create: `plan/architecture.md`

**Interfaces:**
- Consumes: N/A (initial structure)
- Produces: N/A (initial structure)

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

---

## Phase 2: Documentation Rules

### Task 2.1: Add Clean-Room Rule

**Files:**
- Modify: `AGENTS.md`

**Interfaces:**
- Consumes: N/A
- Produces: N/A

- [ ] **Step 1: Add clean-room section ke AGENTS.md**

Tambah rule clean-room:
- Tidak copy code dari IDM, hanya observasi black-box
- Dokumentasi feature parity dengan IDM (daftar fitur yang sudah ada vs belum)
- Tidak copy GPL/AGPL/LGPL code (LibreOffice, Gnumeric…)
- Function behaviour comes from public documentation dan observation

- [ ] **Step 2: Commit**

```bash
git add AGENTS.md
git commit -m "docs: add clean-room rule to AGENTS.md"
```

---

### Task 2.2: Add Never-Crash Rule

**Files:**
- Modify: `AGENTS.md`

**Interfaces:**
- Consumes: N/A
- Produces: N/A

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

---

### Task 2.3: Add Non-Negotiables

**Files:**
- Modify: `AGENTS.md`

**Interfaces:**
- Consumes: N/A
- Produces: N/A

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

---

### Task 2.4: Add Session Start Protocol

**Files:**
- Modify: `AGENTS.md`

**Interfaces:**
- Consumes: N/A
- Produces: N/A

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

---

## Phase 3: Contributor Tracking

### Task 3.1: Create Contributor JSON

**Files:**
- Create: `contributors/contributors.json`

**Interfaces:**
- Consumes: N/A
- Produces: N/A

- [ ] **Step 1: Create `contributors/contributors.json`**

Create file dengan commit stats (generated, never hand-edit).

- [ ] **Step 2: Commit**

```bash
git add contributors/contributors.json
git commit -m "feat: add contributor tracking (contributors.json)"
```

---

### Task 3.2: Create People TOML

**Files:**
- Create: `contributors/people.toml`

**Interfaces:**
- Consumes: N/A
- Produces: N/A

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

---

### Task 3.3: Add PR Check for Contributor

**Files:**
- Modify: `.github/workflows/pr-check.yml`

**Interfaces:**
- Consumes: N/A
- Produces: N/A

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

---

## Phase 4: Documentation

### Task 4.1: Create Feature Parity Documentation

**Files:**
- Create: `docs/feature-parity.md`

**Interfaces:**
- Consumes: N/A
- Produces: N/A

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

---

### Task 4.2: Create Gaps Documentation

**Files:**
- Create: `docs/gaps.md`

**Interfaces:**
- Consumes: N/A
- Produces: N/A

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

---

### Task 4.3: Create Architecture Documentation

**Files:**
- Create: `docs/architecture.md`

**Interfaces:**
- Consumes: N/A
- Produces: N/A

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

---

### Task 4.4: Create Extension Parity Documentation

**Files:**
- Create: `docs/extension-parity.md`

**Interfaces:**
- Consumes: N/A
- Produces: N/A

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

---

### Task 4.5: Create Scheduler Parity Documentation

**Files:**
- Create: `docs/scheduler-parity.md`

**Interfaces:**
- Consumes: N/A
- Produces: N/A

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

---

## Phase 5: Polish

### Task 5.1: Update AGENTS.md

**Files:**
- Modify: `AGENTS.md`

**Interfaces:**
- Consumes: N/A
- Produces: N/A

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

---

### Task 5.2: Final Review

**Files:**
- Review: Semua file yang diubah

**Interfaces:**
- Consumes: N/A
- Produces: N/A

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

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-11-01-gridcraft-integration-plan.md`. Please review the plan. Which execution approach would you prefer?

- **Subagent-driven** - A fresh subagent implements each task dan a fresh reviewer checks it before the next one starts, then a whole-branch review at the end. Most thorough; costs a fresh context per task dan per review.
- **Native** - I implement every task myself in this session, the way this harness runs work, then one fresh reviewer on the most capable model checks the whole branch. Cheapest dan fastest; no independent review until the end. Runs well with a mid-tier session model, since the plan carries the design.

**For this plan I recommend native, karena ini hanya dokumentasi & rules (tidak ada implementasi code), jadi bisa dikerjakan sendiri dalam satu sesi dengan review terakhir.**

Does the plan capture what you want?
