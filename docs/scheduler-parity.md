# RDM — Scheduler Parity Documentation

## Overview

Dokumentasi scheduler RDM (Rust Download Manager) dibandingkan dengan IDM scheduler.

## Current State

- ❌ **RDM:** Belum ada scheduler
- ❌ **IDM:** Mendukung scheduler (jadwal download)

## Planned Features

### Named Queues
**Deskripsi:** Add queue creation dialog, assign jobs ke queues.

**Fitur:**
- Queue creation dialog
- Assign jobs ke queues
- Queue name, description, color
- Queue priority

**Status:** Low priority backlog

---

### Scheduler
**Deskripsi:** Schedule queue execution (time-based).

**Fitur:**
- Schedule queue at specific time
- Recurring schedules (daily, weekly, monthly)
- Schedule constraints (weekdays, hours)
- Manual trigger

**Status:** Low priority backlog

---

### Priority
**Deskripsi:** Priority untuk jobs/queues.

**Fitur:**
- Job priority (low, medium, high, critical)
- Queue priority
- Priority-based scheduling
- Priority override

**Status:** Low priority backlog

---

### Schedule Times
**Deskripsi:** Time-based scheduling untuk downloads.

**Fitur:**
- Schedule download at specific time
- Recurring schedules
- Schedule constraints
- Timezone support

**Status:** Low priority backlog

---

## Task Dependencies

- Named queues → Scheduler
- Scheduler → Priority
- Priority → Schedule Times

---

## Success Criteria

- Named queues implemented
- Scheduler dengan basic scheduling
- Priority support
- Time-based scheduling
- All tests passing
- Manual testing completed

---

## Future Enhancements

- Queue dependencies
- Conditional scheduling
- Event-based scheduling
- Shared queues
- Queue templates