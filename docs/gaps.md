# RDM — Gaps Documentation

## Overview

Daftar backlog RDM (Rust Download Manager) terurut prioritas. Status diverifikasi
langsung dari kode (bukan asumsi). Terakhir diperbarui 2026-11-05.

## Sudah Selesai (dihapus dari backlog)

| Fitur | Bukti di kode |
|---|---|
| Auto-retry 3x backoff | `rdm-daemon/src/lib.rs` (`AUTO_RETRY_MAX`, status `retrying`) |
| Clipboard import (offer card) | `clipboard_watch` di settings + offer card di UI |
| Missing-file badge | `j.missing` + `.st.missing` di UI, cek `out_path` di daemon |
| Tray speed display + submenu limit | `update_tray_jobs`, `tray_fmt_bps` di `rdm-ui` |
| Named queues + priority | DB v0.8 (`queue`, priority), queue picker di UI |

## High Priority

### 1. Batch Add
**Deskripsi:** Dialog "Add multiple URLs" + CLI stdin (`rdm add -`).

**Fitur:**
- Textarea multi-baris (satu URL per baris)
- Validasi + dedupe
- Satu job per baris, kategori otomatis jalan seperti biasa

**Status:** Pending

### 2. yt-dlp Playlist Support
**Deskripsi:** URL playlist → satu job per entry (loop `--flat-playlist`).

**Fitur:**
- Detect playlist vs single video
- Konfirmasi jumlah entry sebelum enqueue
- `--no-playlist` tetap default untuk URL single-video

**Status:** Pending (`ytdlp.rs` memakai `--no-playlist`)

## Medium Priority

### 3. Time-Based Scheduler
**Deskripsi:** Queue start/stop pada jam tertentu (seperti IDM scheduler).

**Status:** Pending (priority ordering sudah ada; hanya trigger waktu yang belum)

### 4. Firefox Extension Port
**Deskripsi:** Port `ext/chrome` ke WebExtension Firefox (manifest beda: native
messaging host manifest + background).

**Status:** Pending

## Low Priority

### 5. Search/Filter Job List
**Status:** Pending

### 6. Multi-Source/Mirror per Job
**Status:** Pending (engine sudah punya cross-URL resume sebagai fondasi)

## Task Dependencies

Batch add → independent. Playlist → independent. Scheduler → bergantung pada
named queues (sudah selesai). Semua lainnya independent.
