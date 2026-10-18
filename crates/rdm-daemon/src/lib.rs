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

async fn handle_client(app: Arc<App>, stream: UnixStream) {
    let (r, mut w) = stream.into_split();
    let mut lines = BufReader::new(r).lines();
    let mut evrx = app.events.subscribe();
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
        _ => bail!("unknown message type"),
    }
}

async fn accept_job(app: Arc<App>, offer: Offer) -> Result<u64> {
    let id = app.next_id.fetch_add(1, Ordering::SeqCst);
    let spec = serde_json::to_string(&offer_json(&offer))?;
    let (job, out_path, filename) = build_job(&offer)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    {
        let db = app.db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
        db.execute(
            "INSERT INTO jobs(id,url,filename,out_path,size,done,status,error,created,updated,spec) \
             VALUES(?1,?2,?3,?4,NULL,0,'running',NULL,?5,?5,?6)",
            rusqlite::params![id as i64, offer.url, filename, out_path, now, spec],
        )?;
    }
    spawn_job(app, id, job).await;
    Ok(id)
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

async fn spawn_job(app: Arc<App>, id: u64, job: Job) {
    let (kill_tx, kill_rx) = watch::channel(false);
    app.ctl.lock().unwrap().insert(id, JobCtl { kill: kill_tx, intent: "cancelled" });
    let app2 = app.clone();
    tokio::spawn(async move {
        run_job(app2, id, job, kill_rx).await;
    });
}

async fn stop_job(app: &Arc<App>, id: u64, intent: &'static str) -> Result<()> {
    let mut ctl = app.ctl.lock().unwrap();
    if let Some(c) = ctl.get_mut(&id) {
        c.intent = intent;
        let _ = c.kill.send(true);
        drop(ctl);
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
        bail!("job {id} masih berjalan");
    }
    let Some(spec) = app.db_get_spec(id) else {
        bail!("spec untuk job {id} tidak ada");
    };
    let offer: Offer = serde_json::from_str(&spec).context("spec rusak")?;
    if restart {
        let (_, out_path, _) = build_job(&offer)?;
        let _ = std::fs::remove_file(format!("{out_path}.rdm-part.json"));
        app.db_update(id, 0, "running", None, None);
    }
    let (job, _, _) = build_job(&offer)?;
    app.db_update(id, 0, "running", None, None);
    spawn_job(app.clone(), id, job).await;
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

async fn run_job(app: Arc<App>, id: u64, job: Job, kill_rx: watch::Receiver<bool>) {
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
