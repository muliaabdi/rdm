# RDM — Execution Plan

## Overview

Rencana eksekusi turun dari `plan/STATUS.md` (milestone aktif) dan
`docs/gaps.md` (backlog + detail fitur). File ini hanya daftar task berjalan —
jangan duplikasi isi gaps.md.

## Task Berjalan (2026-11-05)

### Struktur — SELESAI
1. ~~Bersih AGENTS.md dari kontaminasi GridCraft~~ ✅
2. ~~Sinkron STATUS/gaps/feature-parity dengan realita kode~~ ✅
3. ~~Split `crates/rdm-daemon/src/lib.rs` → modul~~ ✅
   (`settings`/`protocol`/`jobs`/`runners`/`uilaunch`; lib.rs 2291→372 baris;
   fmt+clippy+34 test hijau, daemon sudah direstart)
4. ~~Split `ui/index.html` single script → 5 file JS~~ ✅
   (`app-core`/`state`/`render`/`daemon-events`/`dialogs`; index.html
   2508→689 baris; verified di browser: render identik, dialog jalan,
   0 console error)

### Berikutnya (fitur)
5. Batch add (dialog + `rdm add -`)
6. yt-dlp playlist support

## Autonomous Operation Protocol

- Kerjakan task berurutan sesuai daftar di atas.
- Jangan stop untuk bertanya kecuali: pilihan yang mengubah perilaku user-facing,
  keputusan keamanan, atau destructive action di luar repo ini.
- Setiap task selesai: update file ini + `plan/STATUS.md`, commit dengan pesan
  konvensional.
