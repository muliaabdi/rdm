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
fn set_settings(v: serde_json::Value) -> Result<(), String> {
    let p = settings_path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&p, serde_json::to_vec_pretty(&v).unwrap_or_default()).map_err(|e| e.to_string())
}

#[tauri::command]
fn get_settings() -> serde_json::Value {
    read_settings()
}

fn send_line(ctx: &Ctx, line: String) -> Result<(), String> {
    let guard = ctx.tx.lock().map_err(|e| e.to_string())?;
    match &*guard {
        Some(tx) => tx.try_send(line).map_err(|e| e.to_string()),
        None => Err("belum terhubung ke daemon".into()),
    }
}

#[tauri::command]
fn list_jobs(ctx: State<'_, Arc<Ctx>>) -> Result<(), String> {
    send_line(&ctx, json!({"t":"list"}).to_string())
}

#[tauri::command]
fn add_url(ctx: State<'_, Arc<Ctx>>, url: String) -> Result<(), String> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("URL harus http(s)".into());
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
        "tidak bisa terhubung ke rdm-daemon di {}",
        rdm_daemon::socket_path().display()
    )
}

async fn run_socket(app: tauri::AppHandle, ctx: Arc<Ctx>) -> anyhow::Result<()> {
    loop {
        let stream = connect_daemon().await?;
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

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let ctx: Arc<Ctx> = Arc::new(Ctx { tx: Mutex::new(None) });
            app.manage(ctx.clone());
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = run_socket(handle, ctx).await {
                    eprintln!("rdm-ui socket loop: {e}");
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![list_jobs, add_url, get_settings, set_settings])
        .run(tauri::generate_context!())
        .expect("error while running rdm-ui");
}
