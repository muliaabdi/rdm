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
| Batch add (UI dialog + `rdm add -`) | `dialogs.js` (textarea multi-baris), `rdm-cli/src/client.rs` |
| yt-dlp playlist support | `ytdlp.rs` (`extract_playlist`, `--flat-playlist`), modal konfirmasi & batch queue |

## Medium Priority

### 1. Time-Based Scheduler
**Deskripsi:** Queue start/stop pada jam tertentu (seperti IDM scheduler).

**Status:** Pending (priority ordering sudah ada; hanya trigger waktu yang belum)

### 3. Firefox Extension Port
**Deskripsi:** Port `ext/chrome` ke WebExtension Firefox (manifest beda: native
messaging host manifest + background).

**Status:** Pending

## Low Priority

### 4. Search/Filter Job List
**Status:** Pending

### 5. Multi-Source/Mirror per Job
**Status:** Pending (engine sudah punya cross-URL resume sebagai fondasi)

## Task Dependencies

Playlist → independent. Scheduler → bergantung pada named queues (sudah
selesai). Semua lainnya independent.

## Catatan implementasi batch add & playlist (2026-11-05)

- Playlist: `ytdlp::looks_like_playlist` mendeteksi param/path playlist,
  `ytdlp::extract_playlist` memakai `--flat-playlist -J` untuk parsing cepat
  indeks video tanpa query format video per item; UI menampilkan modal
  konfirmasi jumlah item dan pilihan resolusi/audio, lalu enqueue satu per satu.
- UI: dialog Add pakai `<textarea>`; multi-baris = batch (dedupe, skip `#`
  dan non-http, hint jumlah tautan, Detect disembunyikan). Enter submit hanya
  saat single-line.
- CLI: `rdm add -` baca stdin; aturan parse sama (`client::parse_urls`).
- CLI kini klien daemon penuh (add/list/pause/resume/restart/delete), bukan
  lagi engine mandiri M0 yang mengabaikan queue/kategori/extractor.
