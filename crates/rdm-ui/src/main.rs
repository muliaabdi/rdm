#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! RDM desktop UI: thin Tauri shell over the rdm-daemon socket.
//! Auto-spawns rdm-daemon (sibling binary) when the socket is absent.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tauri::{Emitter, Manager, State};
use tauri::{WebviewUrl, WebviewWindowBuilder};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

struct Ctx {
    tx: Mutex<Option<mpsc::Sender<String>>>,
}

/// Id of the last background offer shown in the compact detail popup.
static DETAIL_ID: AtomicU64 = AtomicU64::new(0);

fn settings_path() -> PathBuf {
    rdm_daemon::settings_path()
}

fn read_settings() -> serde_json::Value {
    std::fs::read(settings_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| json!({}))
}

#[tauri::command]
fn set_settings(ctx: State<'_, Arc<Ctx>>, v: serde_json::Value) -> Result<(), String> {
    // route through the daemon: it persists AND broadcasts to the extension
    send_line(&ctx, json!({"t":"set-settings","v":v}).to_string())
}

#[tauri::command]
fn get_settings() -> serde_json::Value {
    read_settings()
}

/// XDG autostart entry for this desktop (KDE/GNOME read the same file).
fn autostart_file() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config")
        });
    base.join("autostart").join("rdm.desktop")
}

#[tauri::command]
fn get_autostart() -> bool {
    autostart_file().is_file()
}

/// Create or remove the login autostart entry. The entry launches this very
/// binary, so rebuilding/moving the app only needs a settings save to refresh.
#[tauri::command]
fn set_autostart(enabled: bool) -> Result<(), String> {
    let p = autostart_file();
    if !enabled {
        return match std::fs::remove_file(&p) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        };
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let icon = exe
        .parent()
        .map(|d| d.join("icons/icon.png"))
        .filter(|i| i.is_file())
        .map(|i| i.to_string_lossy().to_string())
        .unwrap_or_else(|| "rdm".into());
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=RDM\nComment=Download manager\nExec={}\nIcon={}\nTerminal=false\nStartupNotify=false\nX-GNOME-Autostart-enabled=true\n",
        exe.display(),
        icon
    );
    std::fs::write(&p, entry).map_err(|e| e.to_string())
}

fn send_line(ctx: &Ctx, line: String) -> Result<(), String> {
    let guard = ctx.tx.lock().map_err(|e| e.to_string())?;
    match &*guard {
        Some(tx) => tx.try_send(line).map_err(|e| e.to_string()),
        None => Err("not connected to the daemon".into()),
    }
}

#[tauri::command]
fn list_jobs(ctx: State<'_, Arc<Ctx>>) -> Result<(), String> {
    send_line(&ctx, json!({"t":"list"}).to_string())
}

#[tauri::command]
fn add_url(ctx: State<'_, Arc<Ctx>>, url: String) -> Result<(), String> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("URL must be http(s)".into());
    }
    let s = read_settings();
    let mut offer = json!({"t":"offer","url":url,"segments":8});
    if let Some(dir) = s.get("dir").and_then(|d| d.as_str()) {
        offer["dir"] = json!(dir);
    }
    if let Some(n) = s.get("segments").and_then(|d| d.as_u64()) {
        offer["segments"] = json!(n);
    }
    send_line(&ctx, offer.to_string())
}

#[tauri::command]
fn probe_url(ctx: State<'_, Arc<Ctx>>, url: String) -> Result<(), String> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("URL must be http(s)".into());
    }
    send_line(&ctx, json!({"t":"extract","url":url}).to_string())
}

/// Open a finished file (folder=false) or the folder containing it.
#[tauri::command]
fn open_path(path: String, folder: bool) -> Result<(), String> {
    use std::process::{Command, Stdio};
    eprintln!("open_path: path={path:?} folder={folder}");
    let p = std::path::PathBuf::from(&path);
    let target = if folder {
        if p.is_dir() {
            p
        } else {
            p.parent().map(|x| x.to_path_buf()).unwrap_or(p)
        }
    } else {
        p
    };
    eprintln!("open_path: target={:?} exists={}", target, target.exists());
    if !target.exists() {
        return Err(if folder {
            "folder not found".into()
        } else {
            "file not found".into()
        });
    }
    let t = target.to_string_lossy().to_string();
    // Plasma 6 ships `kioclient`; plain `xdg-open` can exit 0 without opening
    // anything on this desktop, so it stays the last resort.
    let kde = std::env::var("XDG_CURRENT_DESKTOP")
        .map(|v| v.to_uppercase().contains("KDE"))
        .unwrap_or(false)
        || std::env::var("KDE_FULL_SESSION").is_ok();
    let mut openers: Vec<(&str, Vec<String>)> = Vec::new();
    if kde {
        openers.push(("kioclient", vec!["exec".into(), t.clone()]));
        openers.push(("kde-open", vec![t.clone()]));
    }
    openers.push(("gio", vec!["open".into(), t.clone()]));
    openers.push(("xdg-open", vec![t.clone()]));
    for (bin, args) in openers {
        match Command::new(bin)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
        {
            Ok(st) => {
                eprintln!("open_path: {bin} -> {st}");
                if st.success() {
                    return Ok(());
                }
            }
            Err(e) => eprintln!("open_path: {bin} spawn failed: {e}"),
        }
    }
    Err("no opener worked (xdg-open/gio/kioclient)".into())
}

#[tauri::command]
fn add_page(
    ctx: State<'_, Arc<Ctx>>,
    url: String,
    height: u64,
    audio_only: bool,
) -> Result<(), String> {
    let s = read_settings();
    let mut offer = json!({
        "t":"offer-page","url":url,"page":true,
        "height":height,"audio_only":audio_only,
    });
    if let Some(dir) = s.get("dir").and_then(|d| d.as_str()) {
        offer["dir"] = json!(dir);
    }
    if let Some(n) = s.get("segments").and_then(|d| d.as_u64()) {
        offer["segments"] = json!(n);
    }
    send_line(&ctx, offer.to_string())
}

fn spawn_daemon() {
    // Prefer the systemd user unit when it exists: the daemon stays
    // supervised (Restart=on-failure, journal) and the UI never parents (and
    // zombies) the process. Direct spawn is the fallback for non-systemd
    // setups (plain `cargo run`, containers).
    let null = std::process::Stdio::null;
    let enabled = std::process::Command::new("systemctl")
        .args(["--user", "is-enabled", "--quiet", "rdm-daemon"])
        .stdin(null())
        .stdout(null())
        .stderr(null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if enabled {
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "start", "rdm-daemon"])
            .stdin(null())
            .stdout(null())
            .stderr(null())
            .status();
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(dir) = exe.parent() else { return };
    for name in ["rdm-daemon", "../debug/rdm-daemon", "../rdm-daemon"] {
        let bin = dir.join(name);
        if !bin.is_file() {
            continue;
        }
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        if let Ok(mut child) = Command::new(&bin)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
        {
            // reap when it eventually exits, otherwise it lingers as a zombie
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        return;
    }
}

async fn connect_daemon() -> anyhow::Result<UnixStream> {
    for attempt in 0..15 {
        if let Ok(s) = UnixStream::connect(rdm_daemon::socket_path()).await {
            return Ok(s);
        }
        if attempt == 0 {
            spawn_daemon();
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    anyhow::bail!(
        "cannot connect to rdm-daemon at {}",
        rdm_daemon::socket_path().display()
    )
}

/// Compact "Download details" popup window for background offers (video pill,
/// browser context menu). Creating/focusing it must never raise the main
/// window — the user keeps browsing.
fn build_detail(a: &tauri::AppHandle, visible: bool) -> tauri::Result<tauri::WebviewWindow> {
    WebviewWindowBuilder::new(a, "detail", WebviewUrl::App("index.html".into()))
        .title("RDM")
        .inner_size(470.0, 400.0)
        .min_inner_size(390.0, 320.0)
        .resizable(true)
        .always_on_top(true)
        .skip_taskbar(false)
        .visible(visible)
        .build()
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn show_detail_window(app: &tauri::AppHandle, id: u64) {
    DETAIL_ID.store(id, Ordering::SeqCst);
    // cancels any pending idle reaper (stale generation = do nothing)
    DETAIL_GEN.fetch_add(1, Ordering::SeqCst);
    let a = app.clone();
    let res = app.run_on_main_thread(move || {
        if let Some(w) = a.get_webview_window("detail") {
            let _ = w.emit("detail-open", json!({"id": id}));
            let _ = w.show();
            let _ = w.set_focus();
        } else {
            // first open after start (or after the idle reaper destroyed it):
            // building the webview costs a few hundred ms
            match build_detail(&a, true) {
                Ok(w) => {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
                Err(e) => eprintln!("rdm-ui: detail window failed: {e}"),
            }
        }
        eprintln!("rdm-ui: [{}] detail popup shown for job {id}", now_ms());
    });
    if let Err(e) = res {
        eprintln!("rdm-ui: detail window dispatch failed: {e}");
    }
}

/// Generation counter for the detail popup lifecycle: bumped on every show
/// and every hide, so an idle reaper that wakes up with a stale value knows
/// the popup was used again in the meantime and must be kept.
static DETAIL_GEN: AtomicU64 = AtomicU64::new(0);

/// Destroy the hidden detail popup after `DETAIL_IDLE` of not being used.
/// A hidden WebKitGTK webview still holds a whole renderer process
/// (~150 MB RSS); destroying it frees that, and `show_detail_window`
/// rebuilds the window when the popup is next needed.
const DETAIL_IDLE: std::time::Duration = std::time::Duration::from_secs(180);

fn reap_detail_later(app: &tauri::AppHandle) {
    DETAIL_GEN.fetch_add(1, Ordering::SeqCst);
    let gen = DETAIL_GEN.load(Ordering::SeqCst);
    let a = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(DETAIL_IDLE);
        if DETAIL_GEN.load(Ordering::SeqCst) != gen {
            return; // shown (or re-hidden) again meanwhile
        }
        let a2 = a.clone();
        let _ = a.run_on_main_thread(move || {
            if let Some(w) = a2.get_webview_window("detail") {
                if !w.is_visible().unwrap_or(true) {
                    let _ = w.destroy();
                    eprintln!("rdm-ui: detail popup idle, destroyed (webkit renderer freed)");
                }
            }
        });
    });
}

/// Job id the detail popup should render; the webview asks for it once it is
/// ready (an emitted event could race the page load).
#[tauri::command]
fn detail_current() -> u64 {
    DETAIL_ID.load(Ordering::SeqCst)
}

async fn run_socket(app: tauri::AppHandle, ctx: Arc<Ctx>) -> anyhow::Result<()> {
    loop {
        // never give up: the daemon may be down for a while (updates, tests,
        // crashes) and the UI must re-attach as soon as it returns
        let stream = match connect_daemon().await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("rdm-ui: daemon unreachable ({e:#}); retrying");
                let _ = app.emit("rdm", json!({"t":"disconnected"}));
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        let (r, w) = stream.into_split();
        let (tx, mut rx) = mpsc::channel::<String>(64);
        *ctx.tx.lock().unwrap() = Some(tx.clone());

        // keep the tray's active-downloads section fresh (snapshot every 2s)
        let tick_tx = tx.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(2));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                if tick_tx.send(json!({"t":"list"}).to_string()).await.is_err() {
                    return;
                }
            }
        });

        let mut reader = BufReader::new(r);
        let writer = tokio::spawn(async move {
            let mut w = w;
            while let Some(line) = rx.recv().await {
                if w.write_all(line.as_bytes()).await.is_err() || w.write_all(b"\n").await.is_err()
                {
                    return;
                }
                let _ = w.flush().await;
            }
        });

        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    let v: serde_json::Value =
                        serde_json::from_str(line.trim()).unwrap_or_else(|_| json!({"t":"raw"}));
                    if v["t"].as_str() == Some("jobs") {
                        let jobs = v["jobs"].as_array().cloned().unwrap_or_default();
                        update_tray_jobs(&app, &jobs);
                    }
                    if v["t"].as_str() == Some("show-job") {
                        // background offer: pop the compact detail window, never
                        // the full app; the main window still gets the event for
                        // its selection state
                        if let Some(id) = v["id"].as_u64() {
                            show_detail_window(&app, id);
                        }
                    }
                    let _ = app.emit("rdm", v);
                }
                Err(_) => break,
            }
        }
        writer.abort();
        *ctx.tx.lock().unwrap() = None;
        let _ = app.emit("rdm", json!({"t":"disconnected"}));
        tokio::time::sleep(Duration::from_millis(800)).await; // daemon restart backoff
    }
}

#[tauri::command]
fn job_action(
    ctx: State<'_, Arc<Ctx>>,
    action: String,
    id: u64,
    files: Option<bool>,
) -> Result<(), String> {
    const ALLOWED: [&str; 6] = ["pause", "resume", "cancel", "restart", "delete", "list"];
    if !ALLOWED.contains(&action.as_str()) {
        return Err(format!("unknown action: {action}"));
    }
    send_line(
        &ctx,
        json!({"t":action,"id":id,"files":files.unwrap_or(true)}).to_string(),
    )
}

/// Desktop notification (Linux: notify-send) for finished downloads.
#[tauri::command]
fn notify(title: String, body: String) -> Result<(), String> {
    let s = read_settings();
    if s["notify"].as_bool() == Some(false) {
        return Ok(());
    }
    let status = std::process::Command::new("notify-send")
        .arg("-a")
        .arg("RDM")
        .arg("--")
        .arg(&title)
        .arg(&body)
        .status();
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("notify-send exited {s}")),
        Err(e) => Err(format!("notify-send: {e}")),
    }
}

#[tauri::command]
fn diag(msg: String) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/opencode/ui-diag.log")
    {
        let _ = writeln!(f, "{msg}");
    }
}

static TRAY_OK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static TRAY_FP: Mutex<String> = Mutex::new(String::new());

/// One line in the tray's active-downloads section.
struct TrayJob {
    id: i64,
    label: String,
}

/// Rebuild the tray menu from a `{"t":"jobs"}` snapshot. Cheap no-op when
/// nothing visible changed (fingerprint of id + coarse progress).
fn update_tray_jobs(app: &tauri::AppHandle, jobs: &[serde_json::Value]) {
    let app = app.clone();
    let en = language();
    let active = ["queued", "resolving", "running", "paused"];
    let mut list: Vec<TrayJob> = Vec::new();
    let mut fp = String::new();
    for j in jobs {
        let status = j["status"].as_str().unwrap_or("");
        if !active.contains(&status) {
            continue;
        }
        let id = j["id"].as_i64().unwrap_or(0);
        let name = j["filename"].as_str().unwrap_or("?");
        let name: String = name.chars().take(42).collect();
        let done = j["done"].as_u64().unwrap_or(0);
        let size = j.get("size").and_then(|s| s.as_u64()).filter(|s| *s > 0);
        let text = match (status, size) {
            ("queued", _) => if en { "queued" } else { "antre" }.to_string(),
            ("paused", _) => if en { "paused" } else { "jeda" }.to_string(),
            ("resolving", _) => "…".into(),
            (_, Some(sz)) => format!("{}%", (done * 100 / sz).min(100)),
            _ => format!("{:.1} MB", done as f64 / 1e6),
        };
        let label = format!("{name} — {text}");
        fp.push_str(&format!("{id}:{status}:{text};"));
        list.push(TrayJob { id, label });
    }
    let n = list.len();
    let mut guard = TRAY_FP.lock().unwrap();
    if *guard == fp {
        return;
    }
    *guard = fp.clone();
    drop(guard);

    let handle = app.clone();
    let _ = handle.run_on_main_thread(move || {
        use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
        let Some(tray) = app.tray_by_id("rdm-tray") else {
            return;
        };
        let (m_show, m_quit, m_head) = if en {
            ("Show", "Quit", format!("Active downloads ({n})"))
        } else {
            ("Tampilkan", "Keluar", format!("Unduhan aktif ({n})"))
        };
        let mut items: Vec<Box<dyn tauri::menu::IsMenuItem<_>>> = Vec::new();
        if !list.is_empty() {
            let head = MenuItem::with_id(&app, "rdmj-head", m_head, false, None::<&str>)
                .ok()
                .map(|i| Box::new(i) as Box<dyn tauri::menu::IsMenuItem<_>>);
            let sep = PredefinedMenuItem::separator(&app)
                .ok()
                .map(|i| Box::new(i) as Box<dyn tauri::menu::IsMenuItem<_>>);
            if let Some(h) = head {
                items.push(h);
            }
            for j in &list {
                if let Ok(i) =
                    MenuItem::with_id(&app, format!("rdmj-{}", j.id), &j.label, true, None::<&str>)
                {
                    items.push(Box::new(i));
                }
            }
            if let Some(s) = sep {
                items.push(s);
            }
        }
        let show_i = MenuItem::with_id(&app, "show", m_show, true, None::<&str>).ok();
        let quit_i = MenuItem::with_id(&app, "quit", m_quit, true, None::<&str>).ok();
        // speed-limit quick toggle: writes through set-settings so the live
        // limiter, the Settings UI and the extension all stay in sync
        let kb = read_settings()["speed_limit"].as_u64().unwrap_or(0);
        let (m_lim, m_unlim) = if en {
            ("Speed limit", "Unlimited")
        } else {
            ("Batas kecepatan", "Tanpa batas")
        };
        let opts: [(u64, String); 5] = [
            (0, m_unlim.to_string()),
            (512, "512 KiB/s".into()),
            (1024, "1 MiB/s".into()),
            (2048, "2 MiB/s".into()),
            (4096, "4 MiB/s".into()),
        ];
        let mut checks: Vec<Box<dyn tauri::menu::IsMenuItem<_>>> = Vec::new();
        for (k, label) in opts {
            if let Ok(c) = tauri::menu::CheckMenuItem::with_id(
                &app,
                format!("spdl-{k}"),
                label,
                true,
                kb == k,
                None::<&str>,
            ) {
                checks.push(Box::new(c));
            }
        }
        let chk_refs: Vec<&dyn tauri::menu::IsMenuItem<_>> =
            checks.iter().map(|b| b.as_ref()).collect();
        if let Ok(sub) = tauri::menu::Submenu::with_items(&app, m_lim, true, &chk_refs) {
            items.push(Box::new(sub));
            if let Some(s) = PredefinedMenuItem::separator(&app)
                .ok()
                .map(|i| Box::new(i) as Box<dyn tauri::menu::IsMenuItem<_>>)
            {
                items.push(s);
            }
        }
        if let (Some(s), Some(q)) = (show_i, quit_i) {
            items.push(Box::new(s));
            items.push(Box::new(q));
        }
        let refs: Vec<&dyn tauri::menu::IsMenuItem<_>> = items.iter().map(|b| b.as_ref()).collect();
        if let Ok(menu) = Menu::with_items(&app, &refs) {
            let _ = tray.set_menu(Some(menu));
        }
        let tip = if n > 0 {
            format!("RDM — {n} {}", if en { "active" } else { "aktif" })
        } else {
            "RDM".into()
        };
        let _ = tray.set_tooltip(Some(tip));
    });
}

/// Show, unminimize and focus the main window (used by the tray and the daemon).
fn show_main(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn build_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    // tray on by default; disable with RDM_NOTRAY=1 (desktops without a
    // StatusNotifier host can abort while creating the indicator)
    if std::env::var_os("RDM_NOTRAY").is_some() {
        eprintln!("rdm-ui tray: off (RDM_NOTRAY)");
        return Ok(());
    }
    let id = language();
    let (m_show, m_quit, tip) = if id {
        ("Show", "Quit", "RDM")
    } else {
        ("Tampilkan", "Keluar", "RDM")
    };
    let show_i = MenuItem::with_id(app, "show", m_show, true, None::<&str>)?;
    let quit_i = MenuItem::with_id(app, "quit", m_quit, true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show_i, &quit_i])?;

    TrayIconBuilder::with_id("rdm-tray")
        .icon(tauri::image::Image::from_bytes(include_bytes!(
            "../icons/tray.png"
        ))?)
        .tooltip(tip)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, e| {
            let id = e.id().as_ref().to_string();
            if let Some(jid) = id.strip_prefix("rdmj-") {
                // job line: only the compact detail popup, never the full app
                if let Ok(n) = jid.parse::<u64>() {
                    show_detail_window(app, n);
                }
                return;
            }
            if let Some(kb) = id.strip_prefix("spdl-") {
                if let Ok(k) = kb.parse::<u64>() {
                    let mut s = read_settings();
                    s["speed_limit"] = json!(k);
                    let ctx = app.state::<Arc<Ctx>>();
                    let _ = send_line(&ctx, json!({"t":"set-settings","v":s}).to_string());
                }
                return;
            }
            match id.as_str() {
                "show" => show_main(app),
                "quit" => app.exit(0),
                _ => {}
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;
    TRAY_OK.store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

/// true => English, false => Indonesian (from settings.json `lang`).
fn language() -> bool {
    read_settings()
        .get("lang")
        .and_then(|l| l.as_str())
        .map(|l| l != "id")
        .unwrap_or(true)
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let ctx: Arc<Ctx> = Arc::new(Ctx {
                tx: Mutex::new(None),
            });
            app.manage(ctx.clone());
            // never let a tray/DE quirk take the whole app down
            let tray =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| build_tray(app.handle())));
            match tray {
                Ok(Ok(())) => {}
                Ok(Err(e)) => eprintln!("rdm-ui tray: {e}"),
                Err(_) => eprintln!("rdm-ui tray: panicked; continuing without tray"),
            }
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = run_socket(handle, ctx).await {
                    eprintln!("rdm-ui socket loop: {e}");
                }
            });
            // the detail popup is built lazily: an idle WebKitGTK renderer
            // costs ~150 MB RSS, so it must not sit hidden from startup
            Ok(())
        })
        // closing the window keeps RDM available in the tray; quit from there
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "detail" {
                    // hide now, destroy after the idle timeout if unused
                    api.prevent_close();
                    let _ = window.hide();
                    reap_detail_later(window.app_handle());
                } else if TRAY_OK.load(std::sync::atomic::Ordering::Relaxed) {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            list_jobs,
            add_url,
            probe_url,
            open_path,
            add_page,
            get_settings,
            set_settings,
            get_autostart,
            set_autostart,
            job_action,
            notify,
            detail_current,
            diag
        ])
        .run(tauri::generate_context!())
        .expect("error while running rdm-ui");
}
