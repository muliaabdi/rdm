# RDM — Status

## Milestone

**v0.8** — Named queues + priority, daemon/UI polish

## Current State

- Extension v0.7.0 released
- Daemon stable, socket protocol + SQLite queue (schema v0.8: queue label,
  dispatch priority, per-job speed cap)
- yt-dlp + gallery-dl extractors working
- Auto-retry (3x backoff), clipboard import, missing-file badge, tray speed
  display + speed-limit submenu — semua sudah live
- Named queues + priority ordering sudah live (UI + DB)

## Next Task

**Batch add** — dialog multi-URL (newline/CSV), validasi, satu job per baris.
Sinergis dengan CLI `rdm add -`.

## Backlog Priorities

### High Priority
1. Batch add (dialog + CLI stdin)
2. yt-dlp playlist support (satu URL → banyak job)

### Medium Priority
3. Time-based scheduler (queue start/stop pada jam tertentu)
4. Firefox extension port (WebExtension dari basis MV3)

### Low Priority
5. Search/filter job list
6. Multi-source/mirror per job

## Catatan

- docs/gaps.md dan docs/feature-parity.md adalah sumber status backlog;
  README.md bagian Features adalah sumber status fitur yang sudah jalan.
- Update ketiganya setiap fitur selesai (satu sumber kebenaran per topik).
