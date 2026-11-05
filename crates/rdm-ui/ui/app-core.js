const $ = (id) => document.getElementById(id);
// diag must never throw: it runs inside catch blocks, and __TAURI__ may not
// exist yet (injection race) or at all (file:// preview) — a crash here would
// abort the whole script and kill every button handler
const diag = (m) => {
  try { window.__TAURI__?.core?.invoke?.("diag", { msg: m })?.catch?.(() => {}); } catch {}
};
window.__rdmErrs = [];
window.addEventListener("error", (e) => { window.__rdmErrs.push(e.message); diag("onerror: " + e.message); });
let invoke = async () => { throw new Error("IPC not ready"); };
let listen = async () => () => {};
let detailWin = false;   // this webview is the compact "Download details" popup
// The Tauri global can appear after document-start on WebKitGTK; poll for it
// instead of binding once and dying when it is not there yet
function bindIpc() {
  const t = window.__TAURI__;
  if (t && t.core && t.core.invoke && t.event && t.event.listen) {
    invoke = t.core.invoke;
    listen = t.event.listen;
    try {
      if (t.window && t.window.getCurrentWindow) {
        detailWin = t.window.getCurrentWindow().label === "detail";
      }
    } catch {}
    if (detailWin) initDetailWindow();
    diag("boot ok");
    return true;
  }
  return false;
}
if (!bindIpc()) {
  let tries = 0;
  const iv = setInterval(() => {
    if (bindIpc() || ++tries > 200) clearInterval(iv);
  }, 50);
}

async function focusWindow() {
  try {
    const w = window.__TAURI__.window.getCurrentWindow();
    await w.show();
    await w.unminimize();
    // Wayland: a plain setFocus from a background app is often ignored;
    // pin briefly on top so the compositor raises the window, then unpin
    await w.setAlwaysOnTop(true);
    await w.setFocus();
    setTimeout(() => { w.setAlwaysOnTop(false).catch(() => {}); }, 400);
  } catch (e) { diag("focus: " + (e.message || e)); }
}

/* ---------- custom titlebar (undecorated window) ---------- */
(() => {
  const win = () => {
    try { return window.__TAURI__.window.getCurrentWindow(); } catch { return null; }
  };
  const guard = (p) => { if (p && p.catch) p.catch(() => {}); };
  const min = document.getElementById("w-min");
  const max = document.getElementById("w-max");
  const cls = document.getElementById("w-close");
  if (min) min.onclick = () => { const w = win(); if (w) guard(w.minimize()); };
  if (max) max.onclick = () => { const w = win(); if (w) guard(w.toggleMaximize()); };
  // close rides the CloseRequested handler: hides to tray instead of exiting
  if (cls) cls.onclick = () => { const w = win(); if (w) guard(w.close()); };
  const tb = document.getElementById("titlebar");
  if (tb) tb.addEventListener("dblclick", (e) => {
    // only the bare bar itself, never the buttons
    if (e.target.closest("button")) return;
    const w = win(); if (w) guard(w.toggleMaximize());
  });
})();

/* ---------- i18n ---------- */
const I18N = {
  en: {
    add_url: "Add URL", resume: "Resume", stop: "Stop", delete: "Delete",
    restart: "Restart", settings: "Settings", downloads: "Downloads",
    col_name: "Name", col_size: "Size", col_progress: "Progress", col_status: "Status",
    col_eta: "ETA", col_speed: "Speed", col_file: "File", col_category: "Category",
    col_downloaded: "Downloaded", speed_graph: "Speed graph",
    no_selection: "No download selected", items_zero: "0 items", total_zero: "0 B/s total",
    empty_title: "No downloads yet",
    details: "Details", details_title: "Download details", btn_close: "Close",
    ctx_open_file: "Open file", ctx_open_folder: "Open folder",
    connecting: "connecting…", connected: "connected", disconnected: "disconnected",
    cat_all: "All", cat_compressed: "Compressed", cat_documents: "Documents",
    cat_music: "Music", cat_video: "Video", cat_programs: "Programs", cat_other: "Other",
    empty_category: "No downloads in this category",
    empty_hint: "Grab links with the browser extension, or use Add URL above.",
    error_label: "Error", tt_lang: "Switch language",
    st_completed: "Done", st_failed: "Failed", st_paused: "Paused",
    st_running: "Running",
    st_cancelled: "Cancelled", st_queued: "Queued", st_missing: "Missing",
    st_retrying: "Retry {n}/{m}",
    fld_parallel: "Max parallel jobs", fld_notify: "Notify when done",
    fld_clipboard: "Offer to download copied links",
    del_title: "Delete download", del_hint: "Remove this job from the list.",
    del_files: "Also delete partial files", btn_delete: "Delete",
    del_will_file: "This file will be deleted permanently:", del_will_dir: "This folder and everything inside it will be deleted permanently:",
    del_kept: "The finished file stays on disk:", del_none: "No files will be removed, only the list entry.",
    items_fmt: "{n} items · {m} active", total_fmt: "{s} total",
    u_h: "h", u_m: "m", u_s: "s",
    dlg_add_title: "Add download URL",
    ph_url: "file link, or a video page (YouTube, etc.)",
    btn_detect: "Detect quality", btn_cancel: "Cancel", btn_download: "Download",
    opt_gallery: "{n} files (gallery)",
    hint_page: "Video page detected: click Detect.",
    hint_detecting: "Detecting… (a few seconds)",
    hint_fail: "Detection failed: {e}",
    dlg_quality_title: "Choose quality", opt_audio: "Audio only",
    dlg_settings_title: "Settings", fld_dir: "Folder", fld_segments: "Segments",
    ph_dir: "default: ~/Downloads", fld_lang: "Language", fld_theme: "Theme",
    opt_system: "System", opt_light: "Light", opt_dark: "Dark", btn_save: "Save",
    fld_speed: "Speed limit", ph_speed: "0 = unlimited",
    fld_cats: "Category folders",
    hint_cats: "By default every file goes straight into the main folder. Add a category to store it in its own subfolder (a name, or an absolute path).",
    ph_cat_dir: "folder name or /abs/path", ph_pick_cat: "+ Add category folder", btn_remove: "Remove",
    tab_downloads: "Downloads", tab_browser: "Browser", tab_interface: "Interface",
    fld_ytdlp: "Update yt-dlp automatically",
    err_url_http: "URL must be http(s)",
    err_not_connected: "not connected to the daemon",
    err_gone_file: "File no longer exists on disk", err_gone_folder: "Folder no longer exists",
    err_no_opener: "No app found to open this",
    fld_ftype: "File types", fld_extra: "Extra types",
    ft_all: "All files", ft_images: "Images", ft_videos: "Videos", ft_audio: "Audio",
    ft_docs: "Documents", ft_apps: "Apps", ft_arch: "Archives", ft_others: "Others",
    ft_empty_all: "No types selected: intercepting all files",
    ph_intercept: "custom, e.g. bin dat",
    fld_autostart: "Start with system",
    fld_dns: "DNS resolver", opt_dns_sys: "System default",
    opt_dns_cf: "Cloudflare DoH (1.1.1.1) [Bypass ISP]",
    opt_dns_gg: "Google DoH (8.8.8.8)", opt_dns_custom: "Custom DoH URL",
    ph_dns_custom: "https://your-doh-server/dns-query",
    sec_behavior: "Behavior", btn_browse: "Browse", btn_copy: "Copy", btn_copied: "Copied!",
    confirm_title: "Confirm download", confirm_hint: "RDM intercepted this browser download.",
    col_url: "URL", st_confirming: "Waiting", st_resolving: "Resolving…",
    ctx_limit: "Speed limit…", ctx_priority: "Priority", ctx_queue: "Move to queue…",
    sec_queues: "Queues", queue_all: "All", queue_main: "Main", q_btn: "Queue",
    q_move_title: "Move to queue", q_new_ph: "New queue name", q_move: "Move",
    q_moved: "Moved to {q}", q_resume_all: "Resume all", q_pause_all: "Pause all",
    q_rename: "Rename…", q_rename_title: "Rename queue",
    q_rename_hint: "Every job in this queue gets the new name.",
    q_to_main: "Move all to Main", q_moved_main: "{n} job(s) moved to Main",
    tt_prio_up: "Raise priority", tt_prio_dn: "Lower priority",
    ver_ok: "Verified", fld_sha256: "Record SHA-256 checksum", sha_lbl: "SHA-256",
    dlg_limit_title: "Per-job speed limit",
    dlg_limit_hint: "Cap this download only, in KiB/s.",
    dlg_limit_global: "0 follows the global speed limit setting.",
    dlg_prio_title: "Queue priority",
    pri_high: "High", pri_normal: "Normal", pri_low: "Low",
    pri_hint: "Higher priority starts first when a slot frees up.",
    dlg_queue_title: "Queue",
    dlg_queue_hint: "Label for organizing jobs. Order is global across queues.",
    lim_global: "global", lim_job: "this job",
  },
  id: {
    add_url: "Tambah URL", resume: "Lanjutkan", stop: "Stop", delete: "Hapus",
    restart: "Ulangi", settings: "Pengaturan", downloads: "Unduhan",
    col_name: "Nama", col_size: "Ukuran", col_progress: "Progres", col_status: "Status",
    col_eta: "ETA", col_speed: "Kecepatan", col_file: "Berkas", col_category: "Kategori",
    col_downloaded: "Terunduh", speed_graph: "Grafik kecepatan",
    no_selection: "Tidak ada unduhan dipilih", items_zero: "0 item", total_zero: "0 B/s total",
    empty_title: "Belum ada unduhan",
    details: "Rincian", details_title: "Rincian unduhan", btn_close: "Tutup",
    ctx_open_file: "Buka berkas", ctx_open_folder: "Buka folder",
    connecting: "menghubungkan…", connected: "terhubung", disconnected: "terputus…",
    cat_all: "Semua", cat_compressed: "Terkompresi", cat_documents: "Dokumen",
    cat_music: "Musik", cat_video: "Video", cat_programs: "Program", cat_other: "Lainnya",
    empty_category: "Tidak ada unduhan dalam kategori ini",
    empty_hint: "Ambil tautan lewat ekstensi browser, atau Tambah URL di atas.",
    error_label: "Galat", tt_lang: "Ganti bahasa",
    st_completed: "Selesai", st_failed: "Gagal", st_paused: "Dijeda",
    st_running: "Berjalan",
    st_cancelled: "Dibatalkan", st_queued: "Diantrekan", st_missing: "Hilang",
    st_retrying: "Ulang {n}/{m}",
    fld_parallel: "Maks job paralel", fld_notify: "Notifikasi saat selesai",
    fld_clipboard: "Tawarkan unduhan tautan yang disalin",
    del_title: "Hapus unduhan", del_hint: "Buang job ini dari daftar.",
    del_files: "Hapus juga berkas sebagian", btn_delete: "Hapus",
    del_will_file: "File ini akan dihapus permanen:", del_will_dir: "Folder ini beserta seluruh isinya akan dihapus permanen:",
    del_kept: "File hasil unduhan tetap di disk:", del_none: "Tidak ada berkas yang dihapus, hanya entri daftarnya.",
    items_fmt: "{n} item · {m} aktif", total_fmt: "{s} total",
    u_h: "j", u_m: "m", u_s: "s",
    dlg_add_title: "Tambah URL unduhan",
    ph_url: "tautan berkas, atau halaman video (YouTube, dll.)",
    btn_detect: "Deteksi kualitas", btn_cancel: "Batal", btn_download: "Unduh",
    opt_gallery: "{n} berkas (galeri)",
    hint_page: "Terdeteksi halaman video: klik Deteksi.",
    hint_detecting: "Mendeteksi… (beberapa detik)",
    hint_fail: "Gagal mendeteksi: {e}",
    dlg_quality_title: "Pilih kualitas", opt_audio: "Audio saja",
    dlg_settings_title: "Pengaturan", fld_dir: "Folder", fld_segments: "Segmen",
    ph_dir: "bawaan: ~/Downloads", fld_lang: "Bahasa", fld_theme: "Tema",
    opt_system: "Sistem", opt_light: "Terang", opt_dark: "Gelap", btn_save: "Simpan",
    fld_speed: "Batas kecepatan", ph_speed: "0 = tanpa batas",
    fld_cats: "Folder kategori",
    hint_cats: "Bawaannya semua berkas langsung ke folder utama. Tambahkan kategori untuk menyimpannya di subfolder tersendiri (nama folder atau path absolut).",
    ph_cat_dir: "nama folder atau /path/absolut", ph_pick_cat: "+ Tambah folder kategori", btn_remove: "Hapus",
    tab_downloads: "Unduhan", tab_browser: "Browser", tab_interface: "Antarmuka",
    fld_ytdlp: "Perbarui yt-dlp otomatis",
    err_url_http: "URL harus http(s)",
    err_not_connected: "belum terhubung ke daemon",
    err_gone_file: "Berkas sudah tidak ada di disk", err_gone_folder: "Folder sudah tidak ada",
    err_no_opener: "Tidak ada aplikasi untuk membuka ini",
    fld_ftype: "Jenis berkas", fld_extra: "Jenis tambahan",
    ft_all: "Semua berkas", ft_images: "Gambar", ft_videos: "Video", ft_audio: "Audio",
    ft_docs: "Dokumen", ft_apps: "Aplikasi", ft_arch: "Arsip", ft_others: "Lainnya",
    ft_empty_all: "Tidak ada jenis dipilih: semua berkas diintersep",
    ph_intercept: "tambahan, mis. bin dat",
    fld_autostart: "Mulai saat login",
    fld_dns: "Resolver DNS", opt_dns_sys: "Sistem (Bawaan)",
    opt_dns_cf: "Cloudflare DoH (1.1.1.1) [Bypass Blokir ISP]",
    opt_dns_gg: "Google DoH (8.8.8.8)", opt_dns_custom: "Kustom DoH URL",
    ph_dns_custom: "https://server-doh-anda/dns-query",
    sec_behavior: "Perilaku", btn_browse: "Pilih", btn_copy: "Salin", btn_copied: "Tersalin!",
    confirm_title: "Konfirmasi unduhan", confirm_hint: "Unduhan browser ini dicegat oleh RDM.",
    col_url: "URL", st_confirming: "Menunggu", st_resolving: "Menyelesaikan…",
    ctx_limit: "Batas kecepatan…", ctx_priority: "Prioritas", ctx_queue: "Pindah ke antrean…",
    sec_queues: "Antrean", queue_all: "Semua", queue_main: "Utama", q_btn: "Antrean",
    q_move_title: "Pindah ke antrean", q_new_ph: "Nama antrean baru", q_move: "Pindah",
    q_moved: "Dipindah ke {q}", q_resume_all: "Lanjutkan semua", q_pause_all: "Jeda semua",
    q_rename: "Ganti nama…", q_rename_title: "Ganti nama antrean",
    q_rename_hint: "Semua job di antrean ini memakai nama baru.",
    q_to_main: "Pindahkan semua ke Utama", q_moved_main: "{n} job dipindah ke Utama",
    tt_prio_up: "Naikkan prioritas", tt_prio_dn: "Turunkan prioritas",
    ver_ok: "Terverifikasi", fld_sha256: "Catat checksum SHA-256", sha_lbl: "SHA-256",
    dlg_limit_title: "Batas kecepatan per-job",
    dlg_limit_hint: "Batasi unduhan ini saja, dalam KiB/s.",
    dlg_limit_global: "0 mengikuti pengaturan batas kecepatan global.",
    dlg_prio_title: "Prioritas antrean",
    pri_high: "Tinggi", pri_normal: "Normal", pri_low: "Rendah",
    pri_hint: "Prioritas lebih tinggi mulai duluan saat slot kosong.",
    dlg_queue_title: "Antrean",
    dlg_queue_hint: "Label untuk mengelompokkan job. Urutan tetap global lintas antrean.",
    lim_global: "global", lim_job: "job ini",
  },
};
let lang = "en";

// ---- theme ----
// Preference order: settings.json (pushed via setTheme on load/save) with a
// localStorage mirror so the pre-paint script in <head> can apply it too.
const mqDark = matchMedia("(prefers-color-scheme: dark)");
let lastGraph = null; // [canvas, samples] to repaint when colors flip
function setTheme(pref) {
  const eff = pref === "dark" || (pref === "system" && mqDark.matches) ? "dark" : "light";
  const el = document.documentElement;
  el.dataset.theme = eff;
  el.style.colorScheme = eff;
  localStorage.rdm_theme = pref;
  if (lastGraph) paintGraph(...lastGraph);
}
mqDark.addEventListener?.("change", () => {
  if ((localStorage.rdm_theme || "system") === "system") setTheme("system");
});

function t(key, vars) {
  let s = (I18N[lang] || I18N.en)[key] ?? I18N.en[key] ?? key;
  if (vars) for (const [k, v] of Object.entries(vars)) s = s.split("{" + k + "}").join(v);
  return s;
}

function applyI18n() {
  document.documentElement.lang = lang;
  document.querySelectorAll("[data-i18n],[data-i18n-title],[data-i18n-aria]").forEach((el) => {
    if (el.dataset.i18n) el.textContent = t(el.dataset.i18n);
    if (el.dataset.i18nTitle) el.title = t(el.dataset.i18nTitle);
    if (el.dataset.i18nAria) el.setAttribute("aria-label", t(el.dataset.i18nAria));
  });
  renderCats(); renderRows(); renderStatus();
  setConn(connState);
}

function trErr(e) {
  const s = String(e).replace(/^Error:\s*/, "");
  const map = {
    "URL must be http(s)": "err_url_http",
    "not connected to the daemon": "err_not_connected",
    "file not found": "err_gone_file",
    "folder not found": "err_gone_folder",
    "no opener worked (xdg-open/gio/kioclient)": "err_no_opener",
  };
  return map[s] ? t(map[s]) : s;
}

/* in-app toast replaces the ugly webview alert() */
let toastTimer = null;
function toast(msg) {
  let el = $("toast");
  if (!el) {
    el = document.createElement("div");
    el.id = "toast"; el.setAttribute("role", "status");
    document.body.appendChild(el);
  }
  el.textContent = msg;
  el.classList.add("on");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => el.classList.remove("on"), 3800);
}

async function setLang(next) {
  lang = next === "id" ? "id" : "en";
  applyI18n();
  try {
    const s = (await invoke("get_settings")) || {};
    s.lang = lang;
    await invoke("set_settings", { v: s });
  } catch (e) { diag("lang persist: " + e); }
}

