//! Daemon: JSON-lines API over a 0600 Unix socket.
//!
//! Messages (one JSON object per line, both directions):
//! - in:  {"t":"offer","url":..,"filename":..,"cookie":..,"referer":..,"ua":..,"dir":..,"segments":N}
//! - in:  {"t":"list"}
//! - out: {"t":"accepted","id":N} | {"t":"started",..} | {"t":"progress",..}
//!        | {"t":"done",..} | {"t":"failed",..} | {"t":"cancelled",..} | {"t":"jobs",..}

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
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

struct App {
    db: Mutex<rusqlite::Connection>,
    events: broadcast::Sender<String>,
    next_id: AtomicU64,
}

impl App {
    fn open() -> Result<Self> {
        let path = db_path();
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
                updated INTEGER NOT NULL
            );",
        )
        .context("init schema")?;
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
        Ok(App { db: Mutex::new(db), events, next_id: AtomicU64::new(next) })
    }

    fn db_update(&self, id: u64, done: u64, status: &str, error: Option<&str>, size: Option<u64>) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if let Ok(db) = self.db.lock() {
            let _ = db.execute(
                "UPDATE jobs SET done=?1, status=?2, error=?3, updated=?4, size=COALESCE(?5,size) WHERE id=?6",
                rusqlite::params![done as i64, status, error, now, size.map(|s| s as i64), id as i64],
            );
        }
    }
}

#[derive(Deserialize)]
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
    // Refuse double-start; clear stale socket.
    if path.exists() {
        if UnixStream::connect(&path).await.is_ok() {
            anyhow::bail!("rdm-daemon already listening at {}", path.display());
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
    let app = Arc::new(App::open()?);
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
        _ => anyhow::bail!("unknown message type"),
    }
}

async fn accept_job(app: Arc<App>, offer: Offer) -> Result<u64> {
    let id = app.next_id.fetch_add(1, Ordering::SeqCst);
    let dir = offer.dir.map(PathBuf::from).unwrap_or_else(default_out_dir);
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

    let filename = job.out.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let out_path = job.out.to_string_lossy().to_string();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    if let Ok(db) = app.db.lock() {
        db.execute(
            "INSERT INTO jobs(id,url,filename,out_path,size,done,status,error,created,updated) VALUES(?1,?2,?3,?4,NULL,0,'running',NULL,?5,?5)",
            rusqlite::params![id as i64, offer.url, filename, out_path, now],
        )?;
    }

    let app2 = app.clone();
    tokio::spawn(async move {
        run_job(app2, id, job).await;
    });
    Ok(id)
}

async fn run_job(app: Arc<App>, id: u64, job: Job) {
    let (tx, mut rx) = mpsc::channel::<Progress>(512);
    let (_kt, kr) = watch::channel(false);
    let handle = tokio::spawn(download(job, tx, kr));
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
            Progress::Bytes { total_done, size: s, .. } => {
                done = *total_done;
                size = *s;
                if last.elapsed() >= Duration::from_millis(100) {
                    line = Some(json!({"t":"progress","id":id,"done":done,"size":size}).to_string());
                    last = Instant::now();
                }
            }
            Progress::Retrying { index, attempt, error } => {
                line = Some(json!({"t":"retrying","id":id,"segment":index,"attempt":attempt,"error":error}).to_string());
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
                line = Some(json!({"t":"cancelled","id":id,"done":done}).to_string());
                terminal = Some(("cancelled", None));
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
}

fn list_jobs(app: &App) -> Result<serde_json::Value> {
    let db = app.db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    let mut stmt = db.prepare("SELECT id,url,filename,out_path,size,done,status,error FROM jobs ORDER BY id DESC LIMIT 100")?;
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
