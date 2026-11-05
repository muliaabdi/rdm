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
- Batch add live: dialog Add terima multi-baris (dedupe + validasi) dan
  `rdm add -` dari stdin; CLI kini klien daemon penuh
  (add/list/pause/resume/restart/delete)

## Next Task

**yt-dlp playlist support** — URL playlist → satu job per entry.

## Backlog Priorities

### High Priority
1. yt-dlp playlist support (satu URL → banyak job)

### Medium Priority
2. Time-based scheduler (queue start/stop pada jam tertentu)
3. Firefox extension port (WebExtension dari basis MV3)

### Low Priority
4. Search/filter job list
5. Multi-source/mirror per job

## Catatan

- docs/gaps.md dan docs/feature-parity.md adalah sumber status backlog;
  README.md bagian Features adalah sumber status fitur yang sudah jalan.
- Update ketiganya setiap fitur selesai (satu sumber kebenaran per topik).
