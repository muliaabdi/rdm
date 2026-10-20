# RDM — Rust Download Manager

Download manager untuk Linux desktop: engine tersegmentasi di Rust, daemon
dengan antrean persisten, UI Tauri, dan ekstensi browser yang mengambil alih
unduhan (takeover) + deteksi video di halaman.

![status](https://img.shields.io/badge/status-beta-orange)

## Fitur

- **Unduhan tersegmentasi** multi-koneksi dengan *dynamic splitting* /
  work-stealing (koneksi nganggur mencuri sisa byte koneksi lambat),
  resume lintas URL (Range/ETag), dan pemilihan jumlah segmen per job.
- **Antrean persisten** (SQLite) + batas unduhan paralel; pause/resume instan;
  reconnect otomatis; cleanup berkas sebagian saat hapus.
- **Batas kecepatan global** (KiB/s, 0 = tanpa batas) yang berlaku untuk semua
  unduhan aktif sekaligus, termasuk segmen HLS dan video yt-dlp; bisa diubah
  saat unduhan berjalan.
- **Folder kategori**: tiap kategori (images/videos/audio/documents/apps/
  archives/others) bisa punya folder sendiri; unduhan otomatis diarahkan
  berdasarkan ekstensi (path relatif = terhadap folder utama).
- **Ekstensi Chrome/Brave/Edge/Chromium (MV3)**:
  - takeover unduhan native browser (cancel + dialog konfirmasi RDM),
  - filter tipe file per ekstensi (kategori + daftar kustom),
  - klik kanan link/media → unduh via RDM,
  - pill overlay di elemen `<video>`/`<audio>` (dengan opsi audio-only MP3),
  - cookie + User-Agent browser diteruskan ke engine (unduhan butuh login),
  - fallback untuk URL tanpa ekstensi (mis. Google Drive): keputusan ditunda
    sampai nama file asli diketahui.
- **Situs video** via `yt-dlp` (1.700+ extractor): pilih resolusi/audio,
  tangga retry otomatis (cookie/UA → tanpa cookie → client web);
  self-update otomatis harian di latar (`yt-dlp -U`, bisa dimatikan di
  Settings; paket sistem hanya mencatat versi).
- **HLS** (`m3u8`): AES-128 (IV eksplisit / media-sequence), fMP4 multi-init,
  live polling; `.m3u8` → `.ts`.
- **UI desktop**: daftar + kategori + grafik per-segmen, detail dialog,
  tray icon dengan daftar unduhan aktif (klik = fokus + seleksi baris),
  notifikasi selesai/gagal, EN/ID.
- **Daemon** di systemd user service (auto-start + auto-restart); diakses via
  unix socket `$XDG_RUNTIME_DIR/rdm.sock` (mode 0600).

## Arsitektur

```
crates/
  rdm-core    engine tersegmentasi + HLS (rdm-core)
  rdm-daemon  antrean SQLite, protokol JSON-lines via unix socket, yt-dlp
  rdm-host    native messaging host (stdio <-> socket)
  rdm-ui      UI desktop (Tauri v2, UI web statis tanpa framework)
  rdm-cli     CLI (rdm add/list/pause/...)
ext/chrome    ekstensi browser (MV3, tanpa build step)
```

Data pengguna: `~/.local/share/rdm/` (`rdm.db`, `settings.json`).

## Instalasi

Kebutuhan: Rust (stable), GTK/webkit2gtk-4.1 (Tauri), `ffmpeg`, `yt-dlp`.

```sh
./install.sh            # build release + ~/.local/bin + systemd + .desktop
```

Lalu muat ekstensi: `brave://extensions` (atau `chrome://extensions`) →
Developer mode → **Load unpacked** → pilih folder `ext/chrome`.

Tanpa installer: `cargo build --release` lalu jalankan `rdm-daemon` dan
`rdm-ui`; manifest native host via `ext/install-host.sh`.

## Penggunaan

- Unduh dari browser: klik unduhan biasa → RDM mengambil alih (bisa
  dikonfirmasi via dialog); atau klik kanan → "Download with RDM".
- Video: pill di atas player, atau Add URL → Detect.
- CLI: `rdm add <url>`, `rdm list`, `rdm pause <id>`, `rdm resume <id>`.
- Settings (UI): folder simpan, folder kategori, segmen, unduhan paralel,
  batas kecepatan, tipe file yang di-takeover, bahasa, notifikasi, autostart,
  update yt-dlp otomatis.

## Batasan

- Satu video per URL (playlist belum didukung).
- HLS: `BYTERANGE`/`SAMPLE-AES` ditolak; resume live hanya melanjutkan segmen
  baru (segmen yang terlewat saat pause tidak bisa dipulihkan).
- Pause tidak bisa meng-interupt fase mux ffmpeg (< 2 detik umumnya).
- Cookie tersimpan pada job aktif untuk kebutuhan resume; dihapus saat job
  mencapai status terminal.
- Ekstensi MV3 (Chrome-family). Build Firefox belum.

## Pengembangan

```sh
cargo test --workspace   # 18 tes (engine, HLS, antrean)
cargo fmt
cargo clippy --workspace
```

## Lisensi

GPL-3.0-or-later.
