//! Daemon: JSON-lines API over a 0600 Unix socket.
//!
//! Messages (one JSON object per line, both directions):
//! - in:  {"t":"offer","url":..,"filename":..,"cookie":..,"referer":..,"ua":..,"dir":..,"segments":N}
//!        {"t":"list"} | {"t":"pause","id":N} | {"t":"resume","id":N} | {"t":"cancel","id":N}
//!        {"t":"restart","id":N} | {"t":"delete","id":N}
//! - out: {"t":"accepted","id":N} | {"t":"started",..} | {"t":"progress",..} | {"t":"detail",..}
//!        | {"t":"done",..} | {"t":"failed",..} | {"t":"cancelled",..} | {"t":"jobs",..}

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, COOKIE, REFERER, USER_AGENT};
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc, watch};

use rdm_core::{download, job_from_url, Job, Limits, Progress};

pub mod ytdlp;

pub fn socket_path() -> PathBuf {
    if let Ok(x) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(x).join("rdm.sock");
    }
    let mut p = std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."));
    p.push(".rdm");
    p.push("rdm.sock");
    p
}

fn db_path() -> PathBuf {
    if let Ok(p) = std::env::var("RDM_DB") {
        return PathBuf::from(p);
    }
    let mut p = std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."));
    p.push(".local");
    p.push("share");
    p.push("rdm");
    p.push("rdm.db");
    p
}

fn default_out_dir() -> PathBuf {
    let mut p = std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."));
    p.push("Downloads");
    if p.is_dir() {
        p
    } else {
        PathBuf::from(".")
    }
}

struct JobCtl {
    kill: watch::Sender<bool>,
    intent: &'static str, // status recorded when the engine reports cancellation
}

struct App {
    db: Mutex<rusqlite::Connection>,
    events: broadcast::Sender<String>,
    next_id: AtomicU64,
    ctl: Mutex<HashMap<u64, JobCtl>>,
}

impl App {
    fn open(db_path: PathBuf) -> Result<Self> {
        let path = db_path;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).context("create data dir")?;
        }
        let db = rusqlite::Connection::open(&path).context("open db")?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS jobs(
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                url TEXT NOT NULL,
                filename TEXT NOT NULL,
                out_path TEXT NOT NULL,
                size INTEGER,
                done INTEGER NOT NULL DEFAULT 0,
                status TEXT NOT NULL,
                error TEXT,
                created INTEGER NOT NULL,
                updated INTEGER NOT NULL,
                spec TEXT
            );",
        )
        .context("init schema")?;
        // migrate old db without spec column
        let cols: Vec<String> = {
            let mut stmt = db.prepare("PRAGMA table_info(jobs)")?;
            let names = stmt
                .query_map([], |r| r.get::<_, String>(1))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            names
        };
        if !cols.iter().any(|c| c == "spec") {
            db.execute_batch("ALTER TABLE jobs ADD COLUMN spec TEXT;")?;
        }
        let (events, _) = broadcast::channel(256);
        let mut next = 1u64;
        {
            let max: Option<i64> = db
                .query_row("SELECT MAX(id) FROM jobs", [], |r| r.get(0))
                .unwrap_or(None);
            if let Some(m) = max {
                next = m as u64 + 1;
            }
        }
        Ok(App { db: Mutex::new(db), events, next_id: AtomicU64::new(next), ctl: Mutex::new(HashMap::new()) })
    }

    fn db_update(&self, id: u64, done: u64, status: &str, error: Option<&str>, size: Option<u64>) {
        self.db_exec(move |db| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            db.execute(
                "UPDATE jobs SET done=?1, status=?2, error=?3, updated=?4, size=COALESCE(?5,size) WHERE id=?6",
                rusqlite::params![done as i64, status, error, now, size.map(|s| s as i64), id as i64],
            )?;
            Ok(())
        });
    }

    fn db_exec<F>(&self, f: F)
    where
        F: FnOnce(&rusqlite::Connection) -> rusqlite::Result<()>,
    {
        if let Ok(db) = self.db.lock() {
            if let Err(e) = f(&db) {
                tracing::warn!("db: {e}");
            }
        }
    }

    fn db_get_spec(&self, id: u64) -> Option<String> {
        let db = self.db.lock().ok()?;
        db.query_row("SELECT spec FROM jobs WHERE id=?1", [id as i64], |r| r.get::<_, Option<String>>(0))
            .ok()
            .flatten()
    }
}

#[derive(Deserialize, Clone)]
struct Offer {
    url: String,
    filename: Option<String>,
    cookie: Option<String>,
    referer: Option<String>,
    ua: Option<String>,
    dir: Option<String>,
    segments: Option<u64>,
    /// true: url is a video page; resolve via yt-dlp (composite job)
    page: Option<bool>,
    /// preferred max height for page downloads
    height: Option<u64>,
    audio_only: Option<bool>,
    /// true: create the job in `confirming` state and wait for the UI
    /// (used for intercepted browser downloads)
    confirm: Option<bool>,
}

pub async fn run(path: PathBuf) -> Result<()> {
    run_at(path, db_path()).await
}

pub async fn run_at(path: PathBuf, db_path: PathBuf) -> Result<()> {
    if path.exists() {
        if UnixStream::connect(&path).await.is_ok() {
            bail!("rdm-daemon already listening at {}", path.display());
        }
        std::fs::remove_file(&path).context("remove stale socket")?;
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let listener = UnixListener::bind(&path).context("bind socket")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .context("chmod socket 0600")?;
    }
    tracing::info!("listening at {}", path.display());
    let app = Arc::new(App::open(db_path)?);
    loop {
        let (stream, _) = listener.accept().await?;
        let app = app.clone();
        tokio::spawn(async move {
            handle_client(app, stream).await;
        });
    }
}

/// Shared settings file, also written by rdm-ui (dir/segments/lang/intercept_ext).
fn settings_path() -> PathBuf {
    socket_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default()
        .join("settings.json")
}

fn settings_json() -> serde_json::Value {
    std::fs::read(settings_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| json!({}))
}

async fn handle_client(app: Arc<App>, stream: UnixStream) {
    let (r, mut w) = stream.into_split();
    let mut lines = BufReader::new(r).lines();
    let mut evrx = app.events.subscribe();
    // greet new clients with current settings so the extension can refresh
    // its takeover allow-list without polling
    let _ = write_line(&mut w, &json!({"t":"settings","settings":settings_json()}).to_string()).await;
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Ok(Some(line)) = line else { break };
                if line.trim().is_empty() { continue; }
                let resp = match handle_line(app.clone(), &line).await {
                    Ok(Some(s)) => s,
                    Ok(None) => continue,
                    Err(e) => json!({"t":"error","error":e.to_string()}).to_string(),
                };
                if write_line(&mut w, &resp).await.is_err() { break; }
            }
            ev = evrx.recv() => {
                match ev {
                    Ok(s) => { if write_line(&mut w, &s).await.is_err() { break; } }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        }
    }
}

async fn write_line(w: &mut tokio::net::unix::OwnedWriteHalf, s: &str) -> std::io::Result<()> {
    w.write_all(s.as_bytes()).await?;
    w.write_all(b"\n").await?;
    w.flush().await
}

async fn handle_line(app: Arc<App>, line: &str) -> Result<Option<String>> {
    let v: serde_json::Value = serde_json::from_str(line).context("bad json")?;
    match v.get("t").and_then(|t| t.as_str()) {
        Some("offer") => {
            let offer: Offer = serde_json::from_value(v).context("bad offer")?;
            let id = accept_job(app, offer).await?;
            Ok(Some(json!({"t":"accepted","id":id}).to_string()))
        }
        Some("list") => {
            let jobs = list_jobs(&app)?;
            Ok(Some(json!({"t":"jobs","jobs":jobs}).to_string()))
        }
        Some("get-settings") => {
            Ok(Some(json!({"t":"settings","settings":settings_json()}).to_string()))
        }
        Some("pause") | Some("cancel") => {
            let id = v["id"].as_u64().context("id required")?;
            let paused = v["t"].as_str() == Some("pause");
            stop_job(&app, id, if paused { "paused" } else { "cancelled" }).await?;
            Ok(None)
        }
        Some("resume") | Some("restart") => {
            let id = v["id"].as_u64().context("id required")?;
            let restart = v["t"].as_str() == Some("restart");
            resume_job(&app, id, restart).await?;
            Ok(None)
        }
        Some("delete") => {
            let id = v["id"].as_u64().context("id required")?;
            delete_job(&app, id)?;
            Ok(None)
        }
        Some("extract") => {
            let url = v["url"].as_str().context("url required")?.to_string();
            let cookie = v["cookie"].as_str().map(str::to_string);
            let ua = v["ua"].as_str().map(str::to_string);
            let info = tokio::time::timeout(Duration::from_secs(120), ytdlp::extract(&url, cookie.as_deref(), ua.as_deref()))
                .await
                .context("extract timeout")??;
            Ok(Some(
                json!({
                    "t":"formats",
                    "url": info.webpage_url,
                    "title": info.title,
                    "duration": info.duration,
                    "formats": info.formats,
                })
                .to_string(),
            ))
        }
        Some("offer-page") => {
            let offer: Offer = serde_json::from_value(v).context("bad offer-page")?;
            let id = accept_job(app, offer).await?;
            Ok(Some(json!({"t":"accepted","id":id}).to_string()))
        }
        _ => bail!("unknown message type"),
    }
}

/// Bring the desktop UI to the front so the user can watch progress:
/// focus a running instance, or launch one when none is up.
fn bring_ui(app: &Arc<App>) {
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
    let has_gui = env.iter().any(|(k, _)| k == "DISPLAY" || k == "WAYLAND_DISPLAY")
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
            std::thread::spawn(move || { let _ = c.wait(); });
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
        let Ok(md) = std::fs::metadata(&path) else { continue };
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
            let Ok(kv) = std::str::from_utf8(kv) else { continue };
            let Some((k, v)) = kv.split_once('=') else { continue };
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

fn ui_running() -> bool {
    let Ok(rd) = std::fs::read_dir("/proc") else { return false };
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

async fn accept_job(app: Arc<App>, offer: Offer) -> Result<u64> {
    bring_ui(&app);
    let id = app.next_id.fetch_add(1, Ordering::SeqCst);
    let spec = serde_json::to_string(&offer_json(&offer))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let runner = if offer.page.unwrap_or(false) {
        let dir = offer.dir.clone().map(PathBuf::from).unwrap_or_else(default_out_dir);
        std::fs::create_dir_all(&dir).ok();
        RunnerSpec::Page(offer.clone(), false)
    } else {
        let (job, out_path, filename) = build_job(&offer)?;
        let confirming = offer.confirm.unwrap_or(false);
        let status = if confirming { "confirming" } else { "running" };
        app.db_exec({
            let url = offer.url.clone();
            let out_path = out_path.clone();
            let filename = filename.clone();
            let spec = spec.clone();
            move |db| {
                db.execute(
                    "INSERT INTO jobs(id,url,filename,out_path,size,done,status,error,created,updated,spec) \
                     VALUES(?1,?2,?3,?4,NULL,0,?7,NULL,?5,?5,?6)",
                    rusqlite::params![id as i64, url, filename, out_path, now, spec, status],
                )?;
                Ok(())
            }
        });
        if confirming {
            // intercepted browser download: let the user confirm in the UI first
            let _ = app.events.send(
                json!({"t":"confirm","id":id,"filename":filename,"url":offer.url,"out_path":out_path}).to_string(),
            );
            return Ok(id);
        }
        RunnerSpec::Direct(job)
    };
    if let RunnerSpec::Page(o, _) = &runner {
        let url = o.url.clone();
        let dir = o.dir.clone().map(PathBuf::from).unwrap_or_else(default_out_dir);
        let fname = format!("{} — {}", host_of(&o.url), rdm_core::engine::sanitize_filename(&o.url));
        let out_path = dir.to_string_lossy().to_string();
        app.db_exec({
            let spec = spec.clone();
            move |db| {
                db.execute(
                    "INSERT INTO jobs(id,url,filename,out_path,size,done,status,error,created,updated,spec) \
                     VALUES(?1,?2,?3,?4,NULL,0,'resolving',NULL,?5,?5,?6)",
                    rusqlite::params![id as i64, url, fname, out_path, now, spec],
                )?;
                Ok(())
            }
        });
    }
    spawn_job(app, id, runner).await;
    Ok(id)
}

fn host_of(url: &str) -> String {
    url.split("//").nth(1).unwrap_or(url).split('/').next().unwrap_or("page").to_string()
}

fn offer_json(o: &Offer) -> serde_json::Value {
    json!({
        "url": o.url,
        "filename": o.filename,
        "cookie": o.cookie,
        "referer": o.referer,
        "ua": o.ua,
        "dir": o.dir,
        "segments": o.segments,
        "page": o.page,
        "height": o.height,
        "audio_only": o.audio_only,
        "confirm": o.confirm,
    })
}

fn build_job(offer: &Offer) -> Result<(Job, String, String)> {
    let dir = offer.dir.clone().map(PathBuf::from).unwrap_or_else(default_out_dir);
    std::fs::create_dir_all(&dir).ok();

    let mut job: Job = job_from_url(&offer.url, Some(dir.clone()), offer.segments.unwrap_or(8), Limits::default());
    if let Some(name) = offer.filename.as_deref() {
        let clean = rdm_core::engine::sanitize_filename(name);
        if clean != "download" {
            job.out = dir.join(clean);
        }
    }
    let mut hdrs = HeaderMap::new();
    let put = |h: &mut HeaderMap, n: HeaderName, v: &str| {
        if !v.is_empty() {
            if let Ok(val) = HeaderValue::from_str(v) {
                h.insert(n, val);
            }
        }
    };
    put(&mut hdrs, COOKIE, offer.cookie.as_deref().unwrap_or(""));
    put(&mut hdrs, REFERER, offer.referer.as_deref().unwrap_or(""));
    put(&mut hdrs, USER_AGENT, offer.ua.as_deref().unwrap_or(""));
    job.headers = hdrs;

    let out_path = job.out.to_string_lossy().to_string();
    let filename = job.out.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    Ok((job, out_path, filename))
}

enum RunnerSpec {
    Direct(Job),
    Page(Offer, bool),
}

async fn spawn_job(app: Arc<App>, id: u64, runner: RunnerSpec) {
    let (kill_tx, kill_rx) = watch::channel(false);
    app.ctl.lock().unwrap().insert(id, JobCtl { kill: kill_tx, intent: "cancelled" });
    let app2 = app.clone();
    tokio::spawn(async move {
        run_job(app2, id, runner, kill_rx).await;
    });
}

/// Update status only - keeps `done`/`size` so a resumed job does not reset.
fn db_status(app: &Arc<App>, id: u64, status: &str) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    app.db_exec(move |db| {
        db.execute(
            "UPDATE jobs SET status=?1, error=NULL, updated=?2 WHERE id=?3",
            rusqlite::params![status, now, id as i64],
        )?;
        Ok(())
    });
}

async fn stop_job(app: &Arc<App>, id: u64, intent: &'static str) -> Result<()> {
    let mut ctl = app.ctl.lock().unwrap();
    if let Some(c) = ctl.get_mut(&id) {
        c.intent = intent;
        let _ = c.kill.send(true);
        drop(ctl);
        // flip the UI immediately; the engine's terminal event confirms after
        let _ = app.events.send(json!({"t":"status","id":id,"status":intent}).to_string());
        Ok(())
    } else {
        drop(ctl);
        app.db_exec(move |db| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            db.execute(
                "UPDATE jobs SET status=?1, updated=?2 WHERE id=?3",
                rusqlite::params![intent, now, id as i64],
            )?;
            Ok(())
        });
        Ok(())
    }
}

async fn resume_job(app: &Arc<App>, id: u64, restart: bool) -> Result<()> {
    // a just-paused job may still be tearing down; wait briefly for it
    for _ in 0..30 {
        if !app.ctl.lock().unwrap().contains_key(&id) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if app.ctl.lock().unwrap().contains_key(&id) {
        bail!("job {id} is still running");
    }
    let Some(spec) = app.db_get_spec(id) else {
        bail!("no spec for job {id}");
    };
    let offer: Offer = serde_json::from_str(&spec).context("corrupt spec")?;
    if restart {
        let (_, out_path, _) = build_job(&offer)?;
        let _ = std::fs::remove_file(format!("{out_path}.rdm-part.json"));
        app.db_update(id, 0, "running", None, None);
    }
    let runner = if offer.page.unwrap_or(false) {
        RunnerSpec::Page(offer, restart)
    } else {
        let (mut job, _, _) = build_job(&offer)?;
        job.fresh = restart;
        RunnerSpec::Direct(job)
    };
    if !restart {
        // keep the paused progress on screen until fresh bytes arrive
        db_status(app, id, "running");
        let _ = app.events.send(json!({"t":"status","id":id,"status":"running"}).to_string());
    }
    spawn_job(app.clone(), id, runner).await;
    Ok(())
}

fn delete_job(app: &Arc<App>, id: u64) -> Result<()> {
    {
        let ctl = app.ctl.lock().unwrap();
        if let Some(c) = ctl.get(&id) {
            let _ = c.kill.send(true);
        }
    }
    app.ctl.lock().unwrap().remove(&id);
    let id2 = id;
    app.db_exec(move |db| db.execute("DELETE FROM jobs WHERE id=?1", [id2 as i64]).map(|_| ()));
    let _ = app.events.send(json!({"t":"deleted","id":id}).to_string());
    Ok(())
}

async fn run_job(app: Arc<App>, id: u64, runner: RunnerSpec, kill_rx: watch::Receiver<bool>) {
    match runner {
        RunnerSpec::Direct(job) => run_direct(app, id, job, kill_rx).await,
        RunnerSpec::Page(offer, restart) => run_page(app, id, offer, restart, kill_rx).await,
    }
}

/// Download one media URL with the engine, emitting aggregated progress.
struct PhaseStat {
    done: u64,
    size: Option<u64>,
}

impl Default for PhaseStat {
    fn default() -> Self {
        PhaseStat { done: 0, size: None }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_media(
    app: &Arc<App>,
    id: u64,
    job: Job,
    kill_rx: &mut watch::Receiver<bool>,
    other_done: u64,
    other_size: Option<u64>,
) -> Result<PhaseStat> {
    let (tx, mut rx) = mpsc::channel::<Progress>(512);
    let handle = tokio::spawn(download(job, tx, kill_rx.clone()));
    let mut st = PhaseStat::default();
    let mut last_emit = Instant::now() - Duration::from_secs(10);
    while let Some(p) = rx.recv().await {
        match p {
            Progress::Bytes { total_done, size, .. } => {
                st.done = total_done;
                st.size = size;
            }
            Progress::Detail { segments } => {
                let _ = app.events.send(
                    json!({"t":"detail","id":id,"segments":segments}).to_string(),
                );
            }
            Progress::Retrying { index, attempt, error } => {
                let _ = app.events.send(
                    json!({"t":"retrying","id":id,"segment":index,"attempt":attempt,"error":error}).to_string(),
                );
            }
            Progress::Started { size, .. } => st.size = size,
            Progress::Completed { size, .. } => {
                st.done = size;
                st.size = Some(size);
            }
            Progress::Failed { error } => return Err(anyhow::anyhow!(error)),
            Progress::Cancelled { .. } => return Err(anyhow::anyhow!("__cancelled__")),
            _ => {}
        }
        if last_emit.elapsed() >= Duration::from_millis(150) {
            last_emit = Instant::now();
            let done = st.done + other_done;
            // size is the sum of every phase's size known so far; it grows as
            // later phases (audio, mux) start, like IDM's dynamic total.
            let size = st.size.map(|s| s + other_size.unwrap_or(0)).or(other_size);
            let _ = app.events.send(json!({"t":"progress","id":id,"done":done,"size":size}).to_string());
        }
    }
    match handle.await {
        Ok(Ok(())) => Ok(st),
        _ => Err(anyhow::anyhow!("engine failed")),
    }
}

fn job_for(url: &str, out: PathBuf, offer: &Offer, segments: u64) -> Job {
    let mut job = job_from_url(url, Some(out.clone().parent().map(|p| p.to_path_buf()).unwrap_or_default()), segments, Limits::default());
    job.out = out;
    let mut hdrs = HeaderMap::new();
    let put = |h: &mut HeaderMap, n: HeaderName, v: &str| {
        if !v.is_empty() {
            if let Ok(val) = HeaderValue::from_str(v) {
                h.insert(n, val);
            }
        }
    };
    put(&mut hdrs, COOKIE, offer.cookie.as_deref().unwrap_or(""));
    put(&mut hdrs, REFERER, offer.referer.as_deref().unwrap_or(""));
    put(&mut hdrs, USER_AGENT, offer.ua.as_deref().unwrap_or(""));
    job.headers = hdrs;
    job
}

async fn ffmpeg_mux(video: &std::path::Path, audio: Option<&std::path::Path>, out: &std::path::Path) -> Result<()> {
    use std::process::Stdio;
    let mut cmd = tokio::process::Command::new("ffmpeg");
    cmd.arg("-y").arg("-i").arg(video);
    if let Some(a) = audio {
        cmd.arg("-i").arg(a);
    }
    cmd.arg("-c").arg("copy").arg(out).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    let out_err = cmd.stderr(Stdio::piped());
    let mut child = out_err.spawn().context("spawn ffmpeg")?;
    let mut err = String::new();
    if let Some(mut se) = child.stderr.take() {
        use tokio::io::AsyncReadExt;
        let _ = se.read_to_string(&mut err).await;
    }
    let st = tokio::time::timeout(Duration::from_secs(600), child.wait()).await.context("ffmpeg timeout")??;
    if !st.success() {
        let tail = err.lines().rev().take(3).collect::<Vec<_>>().join(" | ");
        bail!("ffmpeg: {tail}");
    }
    Ok(())
}

async fn run_page(app: Arc<App>, id: u64, offer: Offer, restart: bool, mut kill_rx: watch::Receiver<bool>) {
    let finish = |status: &str, err: Option<String>| {
        let _ = app.events.send(json!({"t":status,"id":id,"done":0,"error":err}).to_string());
    };
    let _ = app.events.send(json!({"t":"started","id":id,"size":null}).to_string());
    let _ = app.events.send(json!({"t":"status","id":id,"status":"resolving"}).to_string());

    let info = {
        // abortable: pause/stop must not wait out a slow extraction
        let mut kill_extract = kill_rx.clone();
        let extraction = ytdlp::extract(&offer.url, offer.cookie.as_deref(), offer.ua.as_deref());
        tokio::pin!(extraction);
        tokio::select! {
            res = &mut extraction => res,
            _ = kill_extract.changed() => {
                cancel_page(&app, id).await;
                return;
            }
        }
    };
    let info = match info {
        Ok(i) => i,
        Err(e) => {
            app.db_update(id, 0, "failed", Some(&format!("{e:#}")), None);
            finish("failed", Some(format!("{e:#}")));
            app.ctl.lock().unwrap().remove(&id);
            return;
        }
    };
    if *kill_rx.borrow() {
        cancel_page(&app, id).await;
        return;
    }
    let (v, a) = ytdlp::pick(&info, offer.height.unwrap_or(1080), offer.audio_only.unwrap_or(false));
    let dir = offer.dir.clone().map(PathBuf::from).unwrap_or_else(default_out_dir);
    let base = rdm_core::engine::sanitize_filename(&info.title);
    let vpath = dir.join(format!("{base}.v.{}", v.ext));
    let apath = a.as_ref().map(|af| dir.join(format!("{base}.a.{}", af.ext)));
    let final_path = if a.is_some() {
        dir.join(format!("{base}.mp4"))
    } else {
        dir.join(format!("{base}.{}", v.ext))
    };
    {
        let fname = final_path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or(base.clone());
        let outp = final_path.to_string_lossy().to_string();
        app.db_exec(move |db| {
            db.execute(
                "UPDATE jobs SET filename=?1, out_path=?2 WHERE id=?3",
                rusqlite::params![fname, outp, id as i64],
            )?;
            Ok(())
        });
    }
    // resolved: real filename + format known, switch from resolving to running
    // (status only - keep `done` so a resumed job does not flash back to 0)
    db_status(&app, id, "running");
    let _ = app.events.send(json!({"t":"status","id":id,"status":"running"}).to_string());

    // phase 1: video
    let gv: PhaseStat;
    let mut ga = PhaseStat::default();

    let mut vjob = job_for(&v.url, vpath.clone(), &offer, offer.segments.unwrap_or(8));
    vjob.fresh = restart;
    match run_media(&app, id, vjob, &mut kill_rx, 0, None).await {
        Ok(st) => gv = st,
        Err(e) => return page_fail(&app, id, e).await,
    }
    if *kill_rx.borrow() {
        cancel_page(&app, id).await;
        return;
    }

    // phase 2: audio
    if let (Some(af), Some(ap)) = (a.as_ref(), apath.as_ref()) {
        let mut ajob = job_for(&af.url, ap.clone(), &offer, offer.segments.unwrap_or(4));
        ajob.fresh = restart;
        match run_media(&app, id, ajob, &mut kill_rx, gv.done, gv.size).await {
            Ok(st) => ga = st,
            Err(e) => return page_fail(&app, id, e).await,
        }
        if *kill_rx.borrow() {
            cancel_page(&app, id).await;
            return;
        }
    }

    // phase 3: mux / rename
    let muxed = if let Some(ap) = apath.as_ref() {
        let _ = app.events.send(json!({"t":"progress","id":id,"done":gv.done + ga.done,"size":gv.size.map(|v| v + ga.size.unwrap_or(0))}).to_string());
        ffmpeg_mux(&vpath, Some(ap), &final_path).await
    } else {
        std::fs::rename(&vpath, &final_path).map_err(|e| anyhow::anyhow!("rename: {e}"))
    };
    let _ = std::fs::remove_file(format!("{}.rdm-part.json", vpath.display()));
    if let Some(ap) = apath.as_ref() {
        let _ = std::fs::remove_file(format!("{}.rdm-part.json", ap.display()));
    }
    match muxed {
        Ok(()) => {
            let size = final_path.metadata().map(|m| m.len()).unwrap_or(gv.done + ga.done);
            let _ = std::fs::remove_file(&vpath);
            if let Some(ap) = apath.as_ref() {
                let _ = std::fs::remove_file(ap);
            }
            let _ = app.events.send(json!({"t":"done","id":id,"size":size,"sha256":null}).to_string());
            app.db_update(id, size, "completed", None, Some(size));
        }
        Err(e) => {
            let msg = format!("{e:#}");
            let _ = app.events.send(json!({"t":"failed","id":id,"error":msg}).to_string());
            app.db_update(id, gv.done + ga.done, "failed", Some(&msg), None);
        }
    }
    app.ctl.lock().unwrap().remove(&id);
}

async fn page_fail(app: &Arc<App>, id: u64, e: anyhow::Error) {
    let msg = format!("{e:#}");
    if msg.contains("__cancelled__") {
        cancel_page(app, id).await;
    } else {
        let _ = app.events.send(json!({"t":"failed","id":id,"error":msg}).to_string());
        app.db_update(id, 0, "failed", Some(&msg), None);
        app.ctl.lock().unwrap().remove(&id);
    }
}

async fn cancel_page(app: &Arc<App>, id: u64) {
    let intent = app.ctl.lock().unwrap().get(&id).map(|c| c.intent).unwrap_or("cancelled");
    let _ = app.events.send(json!({"t":intent,"id":id,"done":0}).to_string());
    app.db_update(id, 0, intent, None, None);
    app.ctl.lock().unwrap().remove(&id);
}

async fn run_direct(app: Arc<App>, id: u64, job: Job, kill_rx: watch::Receiver<bool>) {
    let (tx, mut rx) = mpsc::channel::<Progress>(512);
    let handle = tokio::spawn(download(job, tx, kill_rx));
    let mut last = Instant::now() - Duration::from_secs(10);
    let mut done = 0u64;
    let mut size: Option<u64> = None;
    while let Some(p) = rx.recv().await {
        let mut line: Option<String> = None;
        let mut terminal: Option<(&'static str, Option<String>)> = None;
        match &p {
            Progress::Started { size: s, .. } => {
                size = *s;
                line = Some(json!({"t":"started","id":id,"size":s}).to_string());
            }
            Progress::Detail { segments } => {
                line = Some(json!({"t":"detail","id":id,"segments":segments}).to_string());
            }
            Progress::Bytes { total_done, size: s, .. } => {
                done = *total_done;
                size = *s;
                if last.elapsed() >= Duration::from_millis(100) {
                    line = Some(json!({"t":"progress","id":id,"done":done,"size":size}).to_string());
                    last = Instant::now();
                }
            }
            Progress::Retrying { index, attempt, error } => {
                line = Some(
                    json!({"t":"retrying","id":id,"segment":index,"attempt":attempt,"error":error}).to_string(),
                );
            }
            Progress::Completed { size: s, sha256 } => {
                done = *s;
                size = Some(*s);
                line = Some(json!({"t":"done","id":id,"size":s,"sha256":sha256}).to_string());
                terminal = Some(("completed", None));
            }
            Progress::Failed { error } => {
                line = Some(json!({"t":"failed","id":id,"error":error}).to_string());
                terminal = Some(("failed", Some(error.clone())));
            }
            Progress::Cancelled { total_done } => {
                done = *total_done;
                let intent = app
                    .ctl
                    .lock()
                    .unwrap()
                    .get(&id)
                    .map(|c| c.intent)
                    .unwrap_or("cancelled");
                line = Some(json!({"t":intent,"id":id,"done":done}).to_string());
                terminal = Some((intent, None));
            }
            Progress::SegmentDone { .. } => {}
        }
        if let Some(s) = line {
            let _ = app.events.send(s);
        }
        if let Some((status, error)) = terminal {
            app.db_update(id, done, status, error.as_deref(), size);
        } else if last.elapsed() >= Duration::from_secs(1) {
            app.db_update(id, done, "running", None, size);
        }
    }
    let _ = handle.await;
    app.ctl.lock().unwrap().remove(&id);
}

fn list_jobs(app: &App) -> Result<serde_json::Value> {
    let db = app.db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    let mut stmt =
        db.prepare("SELECT id,url,filename,out_path,size,done,status,error FROM jobs ORDER BY id DESC LIMIT 200")?;
    let rows = stmt.query_map([], |r| {
        Ok(json!({
            "id": r.get::<_, i64>(0)?,
            "url": r.get::<_, String>(1)?,
            "filename": r.get::<_, String>(2)?,
            "out_path": r.get::<_, String>(3)?,
            "size": r.get::<_, Option<i64>>(4)?,
            "done": r.get::<_, i64>(5)?,
            "status": r.get::<_, String>(6)?,
            "error": r.get::<_, Option<String>>(7)?,
        }))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(serde_json::Value::Array(out))
}
