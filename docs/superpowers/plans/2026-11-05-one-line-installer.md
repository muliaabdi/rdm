# Standalone One-Line Web Installer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Provide a one-line command (`bash <(curl -fsSL https://raw.githubusercontent.com/muliaabdi/rdm/main/scripts/install.sh)`) that downloads and installs prebuilt RDM binaries on Linux without needing Rust toolchains.

**Architecture:** A lightweight POSIX/Bash standalone installer script that queries GitHub Releases for the latest prebuilt assets, unpacks binaries into `~/.local/bin`, installs icons and desktop entries, registers browser native-messaging manifests, and starts the systemd user service. Paired with a GitHub Actions workflow that automatically builds release tarballs upon pushing version tags (`v*`).

**Tech Stack:** POSIX Shell / Bash, GitHub Actions, systemd user services, Linux FreeDesktop standards.

**Spec:** User request: `bash <(curl -fsSL https://raw.githubusercontent.com/muliaabdi/rdm/main/scripts/install.sh)` equivalent to AB Download Manager's installation flow.

## Global Constraints
- No root / sudo required: all installations strictly user-local (`~/.local/bin`, `~/.local/share`, `~/.config/systemd/user`).
- No Rust/Cargo build dependencies on the target client machine.
- Safe execution when piped directly into `bash` via `curl`.
- Safe replacement of running daemon without `ETXTBSY` or systemd restart loops.

---

### Task 1: Create standalone web installer script

**Files:**
- Create: `scripts/install.sh`

**Interfaces:**
- Input: Executed via `bash <(curl ...)` with optional arguments (e.g. extension ID).
- Output: Installed `rdm`, `rdm-daemon`, `rdm-ui`, `rdm-host` in `~/.local/bin/`, running systemd user service, desktop entry, and native messaging manifests.

- [ ] **Step 1: Write `scripts/install.sh`**
  - Detect CPU architecture (`x86_64` / `aarch64`).
  - Check required utilities (`curl` or `wget`, `tar`, `systemctl`).
  - Fetch latest release tag and download `rdm-linux-${ARCH}.tar.gz` from GitHub Releases.
  - Safely stop existing `rdm-daemon`.
  - Extract binaries into `~/.local/bin/` with permissions `0755`.
  - Install icons to `~/.local/share/icons/hicolor/` and desktop file to `~/.local/share/applications/rdm.desktop`.
  - Write and register native messaging manifests for Chrome/Brave/Chromium/Firefox.
  - Install and reload `rdm-daemon.service` under `~/.config/systemd/user/` and start it.

- [ ] **Step 2: Test script syntax and local mock extraction**
  - Run `bash -n scripts/install.sh` to ensure syntax is clean.
  - Verify executable permissions (`chmod +x scripts/install.sh`).

- [ ] **Step 3: Commit installer script**
  - `git add scripts/install.sh`
  - `git commit -m "feat: add standalone web installer script (scripts/install.sh)"`

---

### Task 2: Create GitHub Actions Release Workflow

**Files:**
- Create: `.github/workflows/release.yml`

**Interfaces:**
- Trigger: Tag pushes matching `v*` (e.g., `v0.1.0`).
- Artifacts: Release assets `rdm-linux-x86_64.tar.gz` containing binaries, desktop entry, icons, and checksum `SHA256SUMS.txt`.

- [ ] **Step 1: Write `.github/workflows/release.yml`**
  - Runner: `ubuntu-latest`.
  - Install Linux build dependencies: `libwebkit2gtk-4.1-dev`, `build-essential`, `curl`, `wget`, `file`, `libxdo-dev`, `libssl-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev`.
  - Build workspace in release mode (`cargo build --release`).
  - Bundle binaries + icons + desktop file + helper manifests into `rdm-linux-x86_64.tar.gz`.
  - Generate `SHA256SUMS.txt`.
  - Publish release draft/release using `softprops/action-gh-release` with release assets.

- [ ] **Step 2: Validate workflow YAML syntax**
  - Check file formatting and structure.

- [ ] **Step 3: Commit release workflow**
  - `git add .github/workflows/release.yml`
  - `git commit -m "ci: add GitHub Actions release workflow for prebuilt binaries"`

---

### Task 3: Update Documentation & Verification

**Files:**
- Modify: `README.md`
- Modify: `plan/STATUS.md`

- [ ] **Step 1: Update README.md with one-line install command**
  - Add "Installation (Linux)" section at the top of README with `bash <(curl -fsSL https://raw.githubusercontent.com/muliaabdi/rdm/main/scripts/install.sh)`.

- [ ] **Step 2: Update STATUS.md and commit**
  - Commit documentation updates.
