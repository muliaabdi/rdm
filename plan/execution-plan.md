# RDM — Execution Plan

## Overview

Daftar task terstruktur untuk development RDM (Rust Download Manager) dengan prioritas jelas.

## High Priority Tasks

### Tray Speed Toggle
- Add speed indicator to system tray icon
- Add context menu item to show/hide speed
- Add keyboard shortcut (Ctrl+Shift+S)
- Test with multiple concurrent downloads

### Clipboard Import
- Add "Paste URL from clipboard" button in main UI
- Add keyboard shortcut (Ctrl+V)
- Parse various URL formats (YouTube, direct links, etc.)
- Test with different clipboard contents

### Auto-Retry
- Add retry count to job metadata
- Auto-retry failed downloads up to N times
- Configurable retry delay
- Test with network failures

## Medium Priority Tasks

### Missing-File Badge
- Show badge on job card when file is missing
- Add "Locate file" button
- Scan download directory for missing files
- Test with partial downloads

### Batch Add
- Add "Add multiple URLs" dialog
- Support CSV/JSON input
- Parse and validate URLs
- Test with 10+ URLs

## Low Priority Tasks

### Named Queues/Scheduler
- Add queue creation dialog
- Assign jobs to queues
- Schedule queue execution (time-based)
- Test with multiple queues

## Task Dependencies

- Tray speed toggle → independent
- Clipboard import → independent
- Auto-retry → independent
- Missing-file badge → independent
- Batch add → independent
- Named queues/scheduler → independent

## Success Criteria

- All high priority tasks completed
- All tests passing
- Manual testing completed
- Documentation updated
- No breaking changes to existing functionality
