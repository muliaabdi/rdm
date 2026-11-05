# RDM — Feature Parity dengan IDM

## Overview

Perbandingan fitur RDM vs IDM (Internet Download Manager). Kolom ✅ = ada,
❌ = tidak ada. Status diverifikasi dari kode per 2026-11-05.

## Summary

| Fitur | RDM | IDM | Catatan |
|-------|-----|-----|---------|
| Download file dari URL | ✅ | ✅ | Parity; RDM open-source |
| Segmented download | ✅ | ✅ | RDM: work-stealing + dynamic splitting |
| Resume | ✅ (segmented HTTP + HLS) | ✅ | Parity praktis |
| HLS | ✅ native | ✅ via plugin | RDM unggul |
| Gallery (gallery-dl) | ✅ | ❌ | RDM unggul |
| Video sites (yt-dlp) | ✅ | sebagian | RDM unggul (1.700+ site) |
| Browser integration | ✅ MV3 (Chrome family) | ✅ | IDM lebih luas (Firefox dll) |
| Auto-retry | ✅ 3x backoff | ✅ | Parity |
| Clipboard monitoring | ✅ | ✅ | Parity |
| Missing-file detection | ✅ badge | ✅ | Parity |
| Tray speed display | ✅ | ✅ | Parity |
| Global speed limit | ✅ | ✅ | Parity |
| Per-job speed cap | ✅ (DB v0.8) | ✅ | Parity |
| Named queues | ✅ | ✅ | Parity |
| Priority ordering | ✅ | ✅ | Parity |
| Time-based scheduler | ❌ | ✅ | IDM unggul — backlog medium |
| Batch add URLs | ✅ dialog multi-baris + `rdm add -` | ✅ | Parity |
| Playlist (multi-video) | ❌ | ❌ | Belum ada di keduanya |
| Firefox extension | ❌ | ✅ | IDM unggul — backlog medium |
| Category folders | ✅ | ✅ | Parity |
| Proxy support | ❌ belum diverifikasi | ✅ | Cek `reqwest` proxy wiring sebelum klaim |
| Checksum verify | ✅ sha256 opsional | ✅ | Parity |

## Skor

Parity / unggul RDM: 17 fitur. IDM unggul: 3 (scheduler, Firefox, proxy).
Prioritas penutupan gap ada di `docs/gaps.md`.

## Metodologi

Clean-room: status IDM dari public documentation + black-box observation
(jangan copy code IDM). Status RDM diverifikasi langsung dari kode, bukan
dari docs lain — docs lama pernah saling kontradiksi.
