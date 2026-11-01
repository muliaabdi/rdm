# RDM — Gaps Documentation

## Overview

Daftar backlog RDM (Rust Download Manager) terurut prioritas.

## High Priority

### 1. Tray Speed Toggle
**Deskripsi:** Add ability to show/hide current speed indicator di system tray icon, toggle via right-click context menu atau keyboard shortcut.

**Fitur:**
- Show/hide speed indicator di tray icon
- Context menu item untuk toggle
- Keyboard shortcut (Ctrl+Shift+S)
- Test dengan multiple concurrent downloads

**Status:** Pending

---

### 2. Clipboard Import
**Deskripsi:** Add "Paste URL from clipboard" button di main UI, dengan keyboard shortcut.

**Fitur:**
- "Paste URL from clipboard" button
- Keyboard shortcut (Ctrl+V)
- Parse various URL formats (YouTube, direct links, dll)
- Test dengan berbagai clipboard contents

**Status:** Pending

---

### 3. Auto-Retry
**Deskripsi:** Add retry count ke job metadata, auto-retry failed downloads up to N times.

**Fitur:**
- Retry count di job metadata
- Auto-retry failed downloads up to N times
- Configurable retry delay
- Test dengan network failures

**Status:** Pending

---

## Medium Priority

### 4. Missing-File Badge
**Deskripsi:** Show badge pada job card jika file missing, dengan "Locate file" button.

**Fitur:**
- Badge jika file missing
- "Locate file" button
- Scan download directory untuk missing files
- Test dengan partial downloads

**Status:** Pending

---

### 5. Batch Add
**Deskripsi:** Add "Add multiple URLs" dialog, support CSV/JSON input.

**Fitur:**
- "Add multiple URLs" dialog
- Support CSV/JSON input
- Parse dan validate URLs
- Test dengan 10+ URLs

**Status:** Pending

---

## Low Priority

### 6. Named Queues/Scheduler
**Deskripsi:** Add queue creation dialog, assign jobs ke queues, schedule queue execution (time-based).

**Fitur:**
- Queue creation dialog
- Assign jobs ke queues
- Schedule queue execution (time-based)
- Test dengan multiple queues

**Status:** Pending

---

## Task Dependencies

- Tray speed toggle → independent
- Clipboard import → independent
- Auto-retry → independent
- Missing-file badge → independent
- Batch add → independent
- Named queues/scheduler → independent

**Catatan:** Semua tasks independent, bisa dikerjakan secara paralel.

---

## Success Criteria

- All high priority tasks completed
- All tests passing
- Manual testing completed
- Documentation updated
- No breaking changes ke existing functionality