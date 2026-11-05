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
- yt-dlp playlist support live: flat-playlist detection & extraction,
  modal konfirmasi jumlah item + resolusi/audio, batch enqueue

## Next Task

**Time-based scheduler** — queue start/stop pada jam tertentu.

## Backlog Priorities

### Medium Priority
1. Time-based scheduler (queue start/stop pada jam tertentu)
2. Firefox extension port (WebExtension dari basis MV3)

### Low Priority
3. Search/filter job list
4. Multi-source/mirror per job

## Catatan

- docs/gaps.md dan docs/feature-parity.md adalah sumber status backlog;
  README.md bagian Features adalah sumber status fitur yang sudah jalan.
- Update ketiganya setiap fitur selesai (satu sumber kebenaran per topik).
