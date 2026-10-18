# RDM — Rust Download Manager, Pengganti Penuh IDM

Desain v0.1 · 2026-10-18 · basis: hasil reverse engineering IDM 6.43.15
(~/idm-re/LAPORAN.md)

## Intent

Pengganti IDM yang lintas platform: engine Rust + ekstensi browser sendiri.
Interaksi utama via ekstensi (klik/kanan/deteksi otomatis) — paste URL hanya
cadangan. Bukan clone protokol IDM: protokol dan ekstensi baru, bersih.

## Goals

1. Unduhan tersegmentasi multi-koneksi + resume (Range/ETag), HTTPS modern.
2. Ekstensi Chrome (MV3) + Firefox (MV2) yang mengambil alih unduhan:
   deteksi content-type/size, kanan-klik link/media, halaman video.
3. Cookie + header + POST body diteruskan dari browser ke engine (otentikasi
   sesi), seperti IDM.
4. Antrean persisten (SQLite), kategori, batas kecepatan, penjadwalan.
5. UI desktop (Tauri v2) + CLI.

## Non-goals

- Kompatibel dengan ekstensi IDM sendiri / protokol `MSG#` IDM.
- Injekeksi ke proses browser (SetWindowsHookEx ala IDMNetMon) — ditolak.
- Driver kernel (WFP/TDI) — ditolak.
- Site-specific bypass DRM (youtube-dl territory) — hanya sniff generik
  (MIME/ekstensi/size), tidak ada parser situs tertanam di binary.

## Anti-goals

- Server localhost tanpa autentikasi (celah audit IDM: Origin-spoofable).
- Eksekusi kode arbitrer dari page world (fitur patch-global IDM).
- Telemetri diam-diam ke server vendor.

## Arsitektur

```
┌─ Ekstensi (TS, MV3 Chrome / MV2 FF) ─────────────────┐
│ content script (ringan) + service worker             │
│ webRequest pipeline → kandidat unduhan               │
│ cookies.getAll(url) → header                          │
│ DNR redirect → halaman "captured"                     │
└──────────────┬───────────────────────────────────────┘
               │ native messaging stdio (JSON-lines,
               │ manifest com.rdm.host, allowed_origins ketat)
┌──────────────▼───────────────┐   ┌──────────────────┐
│ rdm-host (bridge)            │◄──┤ rdm-daemon (core)│
│ - validasi origin (dipaksa   │   │ - tokio engine   │
│   oleh browser, bukan header)│   │ - scheduler      │
│ - framing 4-byte LE          │   │ - SQLite queue   │
└──────────────────────────────┘   │ - HTTP API lokal │
                                   │  (127.0.0.1, token│
                                   │  bearer per-sesi) │
                                   └───────┬──────────┘
                                           │
                              ┌────────────▼────────────┐
                              │ rdm-ui (Tauri v2) + CLI │
                              └─────────────────────────┘
```

Poin keamanan (pelajaran audit IDM):
- Kendali penuh hanya lewat native messaging (browser yang memvalidasi origin)
  atau API localhost dengan token yang di-generate daemon per sesi, di-share
  via file 0600 — bukan Origin header.
- Pipe/Unix socket: DACL ketat / mode 0600.
- Pesan dari page world (via content script) dibatasi whitelist tipe + rate
  limit; tidak ada eksekusi fungsi/patch global.

## Protokol host ↔ ekstensi (JSON lines)

```jsonc
// ext → host
{"t":"offer","url":"https://…","page":"https://…","cookies":"a=b; c=d",
 "ua":"…","ct":"application/zip","size":123456,"filename":"x.zip",
 "method":"POST","body_b64":"…","tab":12,"frame":0,"kind":"link|media|dl"}
{"t":"menu","action":"link|all|video","tab":12}
{"t":"ping"}

// host → ext
{"t":"panel","url":"…","name":"x.zip","size":…,"id":42}   // tampilkan download panel
{"t":"grab","tab":12,"patterns":["…"]}                     // minta URL media di tab
{"t":"cfg","intercept":true,"minSize":1048576,"types":["zip","mp4",…]}
{"t":"pong"}
```

## Keputusan teknis

| Bagian | Pilihan | Alasan |
|---|---|---|
| Runtime | tokio + hyper/reqwest + rustls | standar, audited |
| Segmented download | crate sendiri di atas hyper | kontrol Range penuh |
| Queue | SQLite via sqlx | persisten, resume |
| IPC | native messaging stdio | browser validasi origin; tak ada port terbuka |
| Ekstensi | TypeScript, satu source dua manifest (MV3/MV2) | pola IDM (IDMGCExt/idmmzcc3) |
| UI | Tauri v2 | Rust backend, kecil |
| Distribusi | cargo + tauri bundler; ekstensi via web store/dev-mode | — |

## Roadmap (milestone)

M0 — Engine inti (2–3 minggu)
- crate `rdm-core`: segmented downloader, resume, speed limit, checksum,
  event channel. CLI `rdm add <url>`. Uji: unduhan 8 segmen, kill -9, resume.

M1 — Daemon + native host + ekstensi minimal (2–3 minggu)
- `rdm-daemon` (queue SQLite, API internal), `rdm-host` (native messaging),
  ekstensi: tombol + context menu "Unduh dengan RDM" → offer → panel.
- Uji: klik kanan link di Chrome → file terunduh dengan cookie sesi.

M2 — Intersepsi otomatis (3–4 minggu)
- webRequest pipeline (onHeadersReceived → keputusan tipe/ukuran),
  download panel di halaman, DNR captured.html untuk video page,
  downloads.onDeterminingFilename takeover, konfigurasi tipe file.
- Uji: unduh .zip dari browser → RDM mengambil alih tanpa klik.

M3 — UI Tauri + fitur (3–4 minggu)
- daftar unduhan, kategori, scheduler, speed limiter, proxy per-download,
  shrimp: import dari IDM (baca folder DwnlData? opsional).

M4 — Matang
- Firefox build, pembaruan otomatis, installer (MSI/NSIS/deb/AppImage),
- video sniff generik (media element + fetch/XHR pattern seperti IDM
  document.js TAPI tanpa patch global — cukup performance observer +
  webRequest), dokumentasi.

## Yang dibuat builder, keputusan terbuka

- Bahasa UI front-end dalam Tauri (saran: Svelte atau plain TS).
- Format penyimpanan partial: `.rdm-part` + SQLite metadata vs satu file sparse.
- Struktur workspace crate final.
- Nama & branding ekstensi (store policy Chrome: satu ekstensi per akun dev).
