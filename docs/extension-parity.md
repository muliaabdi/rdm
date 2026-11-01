# RDM — Extension Parity Documentation

## Overview

Dokumentasi fitur extension RDM (Chrome MV3) dibandingkan dengan IDM extension.

## Features

### Background SW
- ✅ **RDM:** Mendukung background service worker
- ❌ **IDM:** Mendukung background script
- **Catatan:** RDM native MV3, IDM legacy extension

### Content Script
- ✅ **RDM:** Mendukung content script untuk video metadata (`video-overlay.js`)
- ❌ **IDM:** Mendukung content script
- **Catatan:** RDM lebih fokus ke video extraction

### Context Menu
- ✅ **RDM:** Mendukung context menu untuk download video/audio
- ❌ **IDM:** Mendukung context menu
- **Catatan:** RDM context menu untuk "Download Best+MP3"

### Pill Popup
- ✅ **RDM:** Mendukung pill popup untuk background offers
- ❌ **IDM:** Tidak ada
- **Catatan:** Fitur unik RDM untuk notifikasi background

### Page Formats Prefetch
- ✅ **RDM:** Prefetch `page-formats` saat pill appear
- ❌ **IDM:** Tidak ada
- **Catatan:** RDM prefetch untuk performa

### Contextual Page URL Detection
- ✅ **RDM:** Mendeteksi video di Reddit faceplate/shreddit
- ❌ **IDM:** Tidak ada
- **Catatan:** RDM lebih advanced untuk Reddit

### Menu Rendering
- ✅ **RDM:** Menu renders "Best+MP3" immediately (no waiting on extract)
- ❌ **IDM:** Tidak ada
- **Catatan:** RDM lebih responsive

### Extension Popup
- ✅ **RDM:** Chrome extension popup untuk settings
- ❌ **IDM:** Mendukung extension popup
- **Catatan:** RDM popup untuk settings dan status

## Implementation Details

### Files
- `ext/chrome/manifest.json` — MV3 manifest
- `ext/chrome/background.js` — background service worker
- `ext/chrome/video-overlay.js` — content script untuk video detection
- `ext/chrome/inject.js` — script untuk inject extension ke website
- `ext/chrome/inject.js` (content script) — UI injection

### Permissions
- `activeTab` — untuk context menu
- `scripting` — untuk content script injection
- `storage` — untuk caching page formats

### Caching
- Page formats cached 120s success / 30s failure
- Prefetch page formats saat pill appear
- De-dupes in-flight requests

## Testing

### Manual Testing
- Reload extension di `brave://extensions` setelah edits
- Refresh tabs untuk content script update
- Test context menu di video pages
- Test pill popup appearance

### Auto Testing
- Unit tests untuk content script logic
- Integration tests untuk extension communication
- E2E tests untuk full flow

## Limitations

- **Manual reload required:** Extension changes require manual reload di `brave://extensions`
- **Tab refresh required:** Content script tidak otomatis reload saat tab refresh
- **Reddit detection:** Hanya support Reddit faceplate/shreddit

## Future Enhancements

- Auto-reload extension di background
- Support untuk lebih banyak platform (Instagram, TikTok, dll)
- Multi-language support di extension
- Dark mode support di extension popup