//! Bringing the desktop UI to the front: focus a running instance, or launch
//! one (borrowing a graphical environment from the user's session).

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;

use crate::App;

/// Focus a running instance, or launch one when none is up.
pub(crate) fn bring_ui(app: &Arc<App>) {
    if std::env::var_os("RDM_NO_UI").is_some() {
        tracing::info!("bring_ui: disabled (RDM_NO_UI)");
        return;
    }
    if ui_running() {
        tracing::info!("bring_ui: focusing running UI");
        let _ = app.events.send(json!({"t":"show"}).to_string());
        return;
    }
    let Some(bin) = ui_binary() else {
        tracing::warn!("bring_ui: rdm-ui binary not found next to rdm-daemon");
        return;
    };
    // the daemon may have been started without a graphical environment
    // (systemd unit, headless shell); borrow one from the user's session.
    let env = session_env();
    let has_gui = env
        .iter()
        .any(|(k, _)| k == "DISPLAY" || k == "WAYLAND_DISPLAY")
        || std::env::var_os("DISPLAY").is_some()
        || std::env::var_os("WAYLAND_DISPLAY").is_some();
    if !has_gui {
        tracing::warn!("bring_ui: no graphical session found, not launching UI");
        return;
    }
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(&bin);
    cmd.envs(env)
        .stdin(Stdio::null())
        .stdout(ui_log())
        .stderr(ui_log())
        .process_group(0);
    match cmd.spawn() {
        Ok(mut c) => {
            tracing::info!("bring_ui: launched {} pid {}", bin.display(), c.id());
            // reap exactly this child; a forgotten wait leaves a zombie in the daemon
            std::thread::spawn(move || {
                let _ = c.wait();
            });
        }
        Err(e) => tracing::warn!("bring_ui: spawn failed: {e}"),
    }
}

/// Append-mode sink for the UI's stdout/stderr so startup failures are diagnosable.
fn ui_log() -> std::process::Stdio {
    let path = std::env::var("HOME")
        .map(|h| PathBuf::from(h).join(".local/share/rdm"))
        .unwrap_or_else(|_| PathBuf::from("/tmp"));
    let _ = std::fs::create_dir_all(&path);
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path.join("ui.log"))
    {
        Ok(f) => f.into(),
        Err(_) => std::process::Stdio::null(),
    }
}

/// Collect DISPLAY/WAYLAND_DISPLAY and friends from a process of our own
/// session that certainly has them (compositor or browser).
fn session_env() -> Vec<(String, String)> {
    use std::os::unix::fs::MetadataExt;
    const KEYS: [&str; 16] = [
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XAUTHORITY",
        "DBUS_SESSION_BUS_ADDRESS",
        "XDG_RUNTIME_DIR",
        "XDG_SESSION_TYPE",
        "XDG_CURRENT_DESKTOP",
        "XDG_SESSION_DESKTOP",
        "XDG_DATA_DIRS",
        "XDG_CONFIG_HOME",
        "HOME",
        "PATH",
        "GDK_BACKEND",
        "QT_QPA_PLATFORM",
        "LANG",
        "TERM",
    ];
    const PREFERRED: [&str; 11] = [
        "Hyprland",
        "sway",
        "gnome-shell",
        "kwin_wayland",
        "plasmashell",
        "Xorg",
        "Xwayland",
        "brave",
        "chrome",
        "firefox",
        "chromium",
    ];
    let Ok(me) = std::fs::metadata("/proc/self").map(|m| m.uid()) else {
        return Vec::new();
    };
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut pids: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .bytes()
                .all(|b| b.is_ascii_digit())
        })
        .map(|e| e.path())
        .collect();
    pids.sort();
    let mut fallback: Option<Vec<(String, String)>> = None;
    for path in pids {
        let Ok(md) = std::fs::metadata(&path) else {
            continue;
        };
        if md.uid() != me {
            continue;
        }
        let comm = std::fs::read_to_string(path.join("comm")).unwrap_or_default();
        let comm = comm.trim().to_string();
        let Ok(raw) = std::fs::read(path.join("environ")) else {
            continue;
        };
        let mut found = Vec::new();
        let mut has_gui = false;
        for kv in raw.split(|b| *b == 0) {
            let Ok(kv) = std::str::from_utf8(kv) else {
                continue;
            };
            let Some((k, v)) = kv.split_once('=') else {
                continue;
            };
            if k == "DISPLAY" || k == "WAYLAND_DISPLAY" {
                has_gui = true;
            }
            if KEYS.contains(&k) {
                found.push((k.to_string(), v.to_string()));
            }
        }
        if !has_gui {
            continue;
        }
        if PREFERRED.contains(&comm.as_str()) {
            return found;
        }
        if fallback.is_none() {
            fallback = Some(found);
        }
    }
    fallback.unwrap_or_default()
}

pub(crate) fn ui_running() -> bool {
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return false;
    };
    for e in rd.flatten() {
        let name = e.file_name();
        let s = name.to_string_lossy();
        if !s.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        if let Ok(comm) = std::fs::read_to_string(format!("/proc/{s}/comm")) {
            if comm.trim() == "rdm-ui" {
                return true;
            }
        }
    }
    false
}

fn ui_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("RDM_UI") {
        return Some(PathBuf::from(p));
    }
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    for name in ["rdm-ui", "../rdm-ui"] {
        let b = dir.join(name);
        if b.is_file() {
            return Some(b);
        }
    }
    None
}
