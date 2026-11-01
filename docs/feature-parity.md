# RDM — Feature Parity dengan IDM

## Overview

Dokumentasi fitur RDM (Rust Download Manager) dibandingkan dengan IDM (Internet Download Manager).

## Download

- ✅ **RDM:** Mendukung download file dari URL
- ❌ **IDM:** Mendukung download file dari URL
- **Catatan:** Fitur dasar sama, RDM lebih open-source

## Resume

- ✅ **RDM:** Mendukung resume download (hanya untuk segmented HTTP)
- ❌ **IDM:** Mendukung resume download
- **Catatan:** RDM resume hanya untuk segmented HTTP, IDM mendukung semua tipe

## HLS (HTTP Live Streaming)

- ✅ **RDM:** Mendukung HLS (YouTube, TikTok, dll)
- ❌ **IDM:** Mendukung HLS (via plugin)
- **Catatan:** RDM native support, IDM via plugin

## Gallery-dl

- ✅ **RDM:** Mendukung gallery-dl untuk gambar/galeri
- ❌ **IDM:** Tidak ada support
- **Catatan:** Fitur unik RDM

## Scheduler

- ❌ **RDM:** Belum ada scheduler
- ❌ **IDM:** Mendukung scheduler (jadwal download)
- **Catatan:** Belum diimplementasikan (low priority)

## Tray Speed Toggle

- ❌ **RDM:** Belum ada
- ❌ **IDM:** Mendukung show/hide speed indicator di tray
- **Catatan:** High priority backlog

## Clipboard Import

- ❌ **RDM:** Belum ada
- ❌ **IDM:** Mendukung paste URL dari clipboard
- **Catatan:** High priority backlog

## Auto-Retry

- ❌ **RDM:** Belum ada
- ❌ **IDM:** Mendukung auto-retry (configurable)
- **Catatan:** High priority backlog

## Missing-File Badge

- ❌ **RDM:** Belum ada
- ❌ **IDM:** Mendukung badge jika file missing
- **Catatan:** Medium priority backlog

## Batch Add

- ❌ **RDM:** Belum ada
- ❌ **IDM:** Mendukung batch add multiple URLs
- **Catatan:** Medium priority backlog

## Named Queues/Scheduler

- ❌ **RDM:** Belum ada
- ❌ **IDM:** Mendukung named queues dengan schedule
- **Catatan:** Low priority backlog

## Summary

| Fitur | RDM | IDM | Status |
|-------|-----|-----|--------|
| Download | ✅ | ✅ | Parity |
| Resume | ✅ (segmented) | ✅ | Partial |
| HLS | ✅ | ✅ (plugin) | Parity |
| Gallery-dl | ✅ | ❌ | RDM Advantage |
| Scheduler | ❌ | ✅ | IDM Advantage |
| Tray Speed Toggle | ❌ | ✅ | IDM Advantage |
| Clipboard Import | ❌ | ✅ | IDM Advantage |
| Auto-Retry | ❌ | ✅ | IDM Advantage |
| Missing-File Badge | ❌ | ✅ | IDM Advantage |
| Batch Add | ❌ | ✅ | IDM Advantage |
| Named Queues | ❌ | ✅ | IDM Advantage |