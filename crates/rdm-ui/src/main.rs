#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! RDM desktop UI: thin Tauri shell over the rdm-daemon socket.
//! Auto-spawns rdm-daemon (sibling binary) when the socket is absent.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tauri::{Emitter, Manager, State};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

struct Ctx {
    tx: Mutex<Option<mpsc::Sender<String>>>,
}

fn settings_path() -> PathBuf {
    rdm_daemon::socket_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default()
        .join("settings.json")
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
        if p.is_dir() { p } else { p.parent().map(|x| x.to_path_buf()).unwrap_or(p) }
    } else {
        p
    };
    eprintln!("open_path: target={:?} exists={}", target, target.exists());
    if !target.exists() {
        return Err(if folder { "folder not found".into() } else { "file not found".into() });
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
fn add_page(ctx: State<'_, Arc<Ctx>>, url: String, height: u64, audio_only: bool) -> Result<(), String> {
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
    let Ok(exe) = std::env::current_exe() else { return };
    let Some(dir) = exe.parent() else { return };
    for name in ["rdm-daemon", "../debug/rdm-daemon", "../rdm-daemon"] {
        let bin = dir.join(name);
        if !bin.is_file() {
            continue;
        }
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        let _ = Command::new(&bin)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn();
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
        *ctx.tx.lock().unwrap() = Some(tx);

        let mut reader = BufReader::new(r);
        let writer = tokio::spawn(async move {
            let mut w = w;
            while let Some(line) = rx.recv().await {
                if w.write_all(line.as_bytes()).await.is_err() || w.write_all(b"\n").await.is_err() {
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
fn job_action(ctx: State<'_, Arc<Ctx>>, action: String, id: u64) -> Result<(), String> {
    const ALLOWED: [&str; 6] = ["pause", "resume", "cancel", "restart", "delete", "list"];
    if !ALLOWED.contains(&action.as_str()) {
        return Err(format!("unknown action: {action}"));
    }
    send_line(&ctx, json!({"t":action,"id":id}).to_string())
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
        .icon(tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?)
        .tooltip(tip)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, e| match e.id().as_ref() {
            "show" => show_main(app),
            "quit" => app.exit(0),
            _ => {}
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
        .setup(|app| {
            let ctx: Arc<Ctx> = Arc::new(Ctx { tx: Mutex::new(None) });
            app.manage(ctx.clone());
            // never let a tray/DE quirk take the whole app down
            let tray = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| build_tray(app.handle())));
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
            Ok(())
        })
        // closing the window keeps RDM available in the tray; quit from there
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if TRAY_OK.load(std::sync::atomic::Ordering::Relaxed) {
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
            diag
        ])
        .run(tauri::generate_context!())
        .expect("error while running rdm-ui");
}
