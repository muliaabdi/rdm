//! Daemon: JSON-lines API over a 0600 Unix socket.
//!
//! Messages (one JSON object per line, both directions):
//! - in:  {"t":"offer","url":..,"filename":..,"cookie":..,"referer":..,"ua":..,"dir":..,"segments":N}
//!   {"t":"list"} | {"t":"pause","id":N} | {"t":"resume","id":N} | {"t":"cancel","id":N}
//!   {"t":"restart","id":N} | {"t":"delete","id":N}
//! - out: {"t":"accepted","id":N} | {"t":"started",..} | {"t":"progress",..} | {"t":"detail",..}
//!   | {"t":"done",..} | {"t":"failed",..} | {"t":"cancelled",..} | {"t":"jobs",..}

use std::collections::HashMap;
use std::path::{Path, PathBuf};
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

pub mod gallery;
pub mod packer;
pub mod ytdlp;

pub fn socket_path() -> PathBuf {
    if let Ok(x) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(x).join("rdm.sock");
    }
    let mut p = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    p.push(".rdm");
    p.push("rdm.sock");
    p
}

fn db_path() -> PathBuf {
    if let Ok(p) = std::env::var("RDM_DB") {
        return PathBuf::from(p);
    }
    data_dir().join("rdm.db")
}

/// Persistent per-user data dir (db, settings); survives reboots unlike
/// XDG_RUNTIME_DIR (tmpfs).
pub fn data_dir() -> PathBuf {
    let mut p = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    p.push(".local");
    p.push("share");
    p.push("rdm");
    p
}

fn default_out_dir() -> PathBuf {
    let mut p = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    p.push("Downloads");
    if p.is_dir() {
        return p;
    }
    // recreate the standard folder instead of dumping files into the cwd
    if std::fs::create_dir_all(&p).is_ok() {
        p
    } else {
        PathBuf::from(".")
    }
}

struct JobCtl {
    kill: watch::Sender<bool>,
    intent: &'static str, // status recorded when the engine reports cancellation
    /// Per-job speed bucket; shared with the running engine so `set-job`
    /// retunes a live download without a restart.
    bucket: Option<std::sync::Arc<rdm_core::Limiter>>,
}

struct App {
    db: Mutex<rusqlite::Connection>,
    events: broadcast::Sender<String>,
    next_id: AtomicU64,
    ctl: Mutex<HashMap<u64, JobCtl>>,
    // latest in-flight progress; the DB row only gets `done` on terminal or
    // pause, so snapshots (`list`) read live values from here instead
    live: Mutex<HashMap<u64, (i64, Option<i64>)>>,
    // process-wide token bucket so the speed cap covers every active job
    limiter: std::sync::Arc<rdm_core::Limiter>,
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
        // v0.8: queue label, dispatch priority, per-job speed cap (KiB/s)
        for (col, ddl) in [
            (
                "queue",
                "ALTER TABLE jobs ADD COLUMN queue TEXT NOT NULL DEFAULT 'main';",
            ),
            (
                "priority",
                "ALTER TABLE jobs ADD COLUMN priority INTEGER NOT NULL DEFAULT 0;",
            ),
            (
                "speed_limit",
                "ALTER TABLE jobs ADD COLUMN speed_limit INTEGER NOT NULL DEFAULT 0;",
            ),
            ("sha256", "ALTER TABLE jobs ADD COLUMN sha256 TEXT;"),
            (
                "verified",
                "ALTER TABLE jobs ADD COLUMN verified INTEGER NOT NULL DEFAULT 0;",
            ),
        ] {
            if !cols.iter().any(|c| c == col) {
                db.execute_batch(ddl)?;
            }
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
        Ok(App {
            db: Mutex::new(db),
            events,
            next_id: AtomicU64::new(next),
            ctl: Mutex::new(HashMap::new()),
            live: Mutex::new(HashMap::new()),
            limiter: std::sync::Arc::new(rdm_core::Limiter::new(speed_limit_bytes())),
        })
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
            self.live.lock().unwrap().remove(&id);
            // terminal states drop the browser cookie from the stored spec:
            // resume no longer needs it and plaintext session cookies should
            // not live in rdm.db forever (paused keeps it for resume)
            if matches!(status, "completed" | "failed" | "cancelled") {
                let spec: Option<String> = db
                    .query_row(
                        "SELECT spec FROM jobs WHERE id=?1",
                        [id as i64],
                        |r| r.get(0),
                    )
                    .ok();
                if let Some(spec) = spec {
                    if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&spec) {
                        if v.get("cookie").is_some() {
                            if let Some(obj) = v.as_object_mut() {
                                obj.remove("cookie");
                            }
                            let _ = db.execute(
                                "UPDATE jobs SET spec=?1 WHERE id=?2",
                                rusqlite::params![v.to_string(), id as i64],
                            );
                        }
                    }
                }
            }
            Ok(())
        });
    }

    /// Mark a job as size-verified at completion, optionally storing its
    /// SHA-256 (post-download integrity record).
    fn db_verify(&self, id: u64, sha256: Option<&str>) {
        self.db_exec(move |db| {
            db.execute(
                "UPDATE jobs SET verified=1, sha256=?1 WHERE id=?2",
                rusqlite::params![sha256, id as i64],
            )?;
            Ok(())
        });
    }

    fn note_live(&self, id: u64, done: u64, size: Option<u64>) {
        self.live
            .lock()
            .unwrap()
            .insert(id, (done as i64, size.map(|s| s as i64)));
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
        db.query_row("SELECT spec FROM jobs WHERE id=?1", [id as i64], |r| {
            r.get::<_, Option<String>>(0)
        })
        .ok()
        .flatten()
    }

    /// Current (filename, out_path) row, for detecting engine-side renames.
    fn db_get_name(&self, id: u64) -> Option<(String, String)> {
        let db = self.db.lock().ok()?;
        db.query_row(
            "SELECT filename, out_path FROM jobs WHERE id=?1",
            [id as i64],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .ok()
    }

    /// Newest non-terminal job id already downloading `url`, if any.
    fn db_active_by_url(&self, url: &str) -> Option<u64> {
        let db = self.db.lock().ok()?;
        db.query_row(
            "SELECT id FROM jobs WHERE url=?1 AND status IN \
             ('running','queued','resolving','confirming','paused','retrying') \
             ORDER BY id DESC LIMIT 1",
            [url],
            |r| r.get::<_, i64>(0),
        )
        .ok()
        .map(|id| id as u64)
    }

    /// (status, out_path) of a job row, used by delete cleanup.
    fn db_job_row(&self, id: u64) -> Option<(String, Option<String>)> {
        let db = self.db.lock().ok()?;
        db.query_row(
            "SELECT status, out_path FROM jobs WHERE id=?1",
            [id as i64],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
        )
        .ok()
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
    /// named queue label ("main" by default)
    queue: Option<String>,
    /// dispatch order for queued jobs; higher starts first
    priority: Option<i64>,
    /// per-job speed cap in KiB/s (0 = follow the global setting)
    speed_limit: Option<u64>,
}

fn sanitize_queue(q: &str) -> String {
    let s: String = q
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .take(32)
        .collect();
    if s.is_empty() {
        "main".to_string()
    } else {
        s
    }
}

/// Always create a per-job bucket (unlimited by default): `set-job` can then
/// apply a cap to a running job instantly instead of waiting for a resume.
fn job_bucket(kbps: u64) -> std::sync::Arc<rdm_core::Limiter> {
    let max = match kbps {
        0 => None,
        k => Some(k.saturating_mul(1024)),
    };
    std::sync::Arc::new(rdm_core::Limiter::new(max))
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
    tokio::spawn(ytdlp_updater());
    // a previous daemon may have died mid-job: nothing is running now, and a
    // stale `running`/`resolving` row would look hung forever
    app.db_exec(|db| {
        db.execute(
            "UPDATE jobs SET status='paused' WHERE status IN ('running','resolving')",
            [],
        )?;
        Ok(())
    });
    loop {
        let (stream, _) = listener.accept().await?;
        let app = app.clone();
        tokio::spawn(async move {
            handle_client(app, stream).await;
        });
    }
}

/// Shared settings file, also written by rdm-ui (dir/segments/lang/intercept_ext).
/// Lives in the persistent data dir; falls back to the legacy location next to
/// the socket (tmpfs) when only that exists, so nothing is lost on upgrade.
pub fn settings_path() -> PathBuf {
    data_dir().join("settings.json")
}

fn settings_json() -> serde_json::Value {
    let p = settings_path();
    let b = std::fs::read(&p).ok().or_else(|| {
        let legacy = socket_path().parent().map(|d| d.join("settings.json"))?;
        std::fs::read(legacy).ok()
    });
    b.and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| json!({}))
}

/// `speed_limit` setting in KiB/s (0/absent = unlimited), as bytes/s.
fn speed_limit_bytes() -> Option<u64> {
    match settings_json()["speed_limit"].as_u64().unwrap_or(0) {
        0 => None,
        k => Some(k.saturating_mul(1024)),
    }
}

/// Whether finished downloads should get a SHA-256 checksum recorded.
fn sha256_on() -> bool {
    settings_json()["sha256"].as_bool().unwrap_or(false)
}

/// Active DoH resolver from settings. Defaults to Cloudflare (1.1.1.1) to avoid
/// ISP poisoning (NXDOMAIN) on common stream/embed hosts.
pub fn dns_resolver_from_settings() -> Option<std::sync::Arc<rdm_core::DohResolver>> {
    let s = settings_json();
    let mode = s
        .get("dns_mode")
        .and_then(|v| v.as_str())
        .unwrap_or("cloudflare");
    let custom = s.get("dns_custom").and_then(|v| v.as_str());
    let cfg = rdm_core::DnsConfig::from_settings(mode, custom);
    cfg.endpoint()
        .map(|ep| std::sync::Arc::new(rdm_core::DohResolver::new(ep)))
}

fn expand_path(p: &str) -> PathBuf {
    let p = p.trim();
    let home = || {
        std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."))
    };
    if let Some(rest) = p.strip_prefix("~/") {
        return home().join(rest);
    }
    if let Some(rest) = p.strip_prefix("$HOME/") {
        return home().join(rest);
    }
    if p == "~" || p == "$HOME" {
        return home();
    }
    PathBuf::from(p)
}

/// Base folder: the `dir` setting, else ~/Downloads.
fn base_dir(settings: &serde_json::Value) -> PathBuf {
    settings["dir"]
        .as_str()
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(expand_path)
        .unwrap_or_else(default_out_dir)
}

/// Destination folder for a job: explicit `offer.dir` wins, then the
/// per-category folder from settings, then the base folder.
fn dir_for(offer_dir: Option<&str>, filename: &str) -> PathBuf {
    if let Some(d) = offer_dir.map(str::trim).filter(|d| !d.is_empty()) {
        return expand_path(d);
    }
    let s = settings_json();
    let base = base_dir(&s);
    let cat = rdm_core::category_of(filename);
    let Some(p) = s["categories"][cat]
        .as_str()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    else {
        return base;
    };
    let p = expand_path(p);
    if p.is_absolute() {
        p
    } else {
        base.join(p)
    }
}

/// Keep yt-dlp fresh: extractors go stale within weeks. Runs `yt-dlp -U` in
/// the background (once a day, first check shortly after start). System
/// packages are not writable, so `-U` just logs a hint there.
async fn ytdlp_updater() {
    tokio::time::sleep(std::time::Duration::from_secs(45)).await;
    loop {
        if settings_json()["ytdlp_update"].as_bool().unwrap_or(true) {
            update_ytdlp().await;
        }
        tokio::time::sleep(std::time::Duration::from_secs(24 * 3600)).await;
    }
}

async fn update_ytdlp() {
    let bin = ytdlp::yt_dlp_bin();
    let version = |bin: std::path::PathBuf| async move {
        tokio::process::Command::new(bin)
            .arg("--version")
            .output()
            .await
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    };
    let before = version(bin.clone()).await;
    match tokio::process::Command::new(&bin).arg("-U").output().await {
        Ok(o) if o.status.success() => {
            let after = version(bin).await;
            if after.is_empty() || after == before {
                tracing::info!("yt-dlp {before}: already up to date");
            } else {
                tracing::info!("yt-dlp updated: {before} -> {after}");
            }
        }
        Ok(o) => {
            let msg = String::from_utf8_lossy(&o.stderr);
            let first = msg.trim().lines().next().unwrap_or("no output");
            tracing::info!("yt-dlp {before}: self-update unavailable ({first})");
        }
        Err(e) => tracing::warn!("yt-dlp self-update failed: {e}"),
    }
}

async fn handle_client(app: Arc<App>, stream: UnixStream) {
    let (r, w) = stream.into_split();
    let mut lines = BufReader::new(r).lines();
    let mut evrx = app.events.subscribe();
    // single writer so replies from spawned handlers interleave safely with
    // loop replies and broadcast events
    let (wtx, mut wrx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let mut w = w;
    let writer = tokio::spawn(async move {
        while let Some(s) = wrx.recv().await {
            if write_line(&mut w, &s).await.is_err() {
                break;
            }
        }
        let _ = w.shutdown().await;
    });
    // greet new clients with current settings so the extension can refresh
    // its takeover allow-list without polling
    let _ = wtx.send(json!({"t":"settings","settings":settings_json()}).to_string());
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Ok(Some(line)) = line else { break };
                if line.trim().is_empty() { continue; }
                // yt-dlp extraction can take tens of seconds (YouTube JS
                // challenges). The pill prefetches formats as soon as a page
                // loads, and the click that follows (offer-page) would queue
                // behind that on this connection — the popup must not wait.
                // Replies are matched by url/t, never by order.
                let is_extract = serde_json::from_str::<serde_json::Value>(&line)
                    .ok()
                    .and_then(|v| v["t"].as_str().map(|t| t == "extract"))
                    .unwrap_or(false);
                if is_extract {
                    let app = app.clone();
                    let wtx = wtx.clone();
                    tokio::spawn(async move {
                        let resp = match handle_line(app, &line).await {
                            Ok(Some(s)) => s,
                            Ok(None) => return,
                            Err(e) => json!({"t":"error","error":e.to_string()}).to_string(),
                        };
                        let _ = wtx.send(resp);
                    });
                    continue;
                }
                let resp = match handle_line(app.clone(), &line).await {
                    Ok(Some(s)) => s,
                    Ok(None) => continue,
                    Err(e) => json!({"t":"error","error":e.to_string()}).to_string(),
                };
                let _ = wtx.send(resp);
            }
            ev = evrx.recv() => {
                match ev {
                    Ok(s) => { let _ = wtx.send(s); }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        }
    }
    drop(wtx);
    // drain late extract replies before the write half goes away
    let _ = writer.await;
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
            tracing::info!(target: "ext", "offer arrived: {}", offer.url);
            let id = accept_job(app, offer).await?;
            Ok(Some(json!({"t":"accepted","id":id}).to_string()))
        }
        Some("list") => {
            let jobs = list_jobs(&app)?;
            Ok(Some(json!({"t":"jobs","jobs":jobs}).to_string()))
        }
        Some("get-settings") => Ok(Some(
            json!({"t":"settings","settings":settings_json()}).to_string(),
        )),
        Some("log") => {
            // browser-extension diagnostics; lands in the journal
            // (journalctl --user -u rdm-daemon)
            tracing::info!(target: "ext", "{}", v["msg"].as_str().unwrap_or("?"));
            Ok(None)
        }
        Some("set-settings") => {
            // single writer: persist and push to every connected client (the
            // browser extension refreshes its intercept allow-list from this)
            let val = v["v"].clone();
            let p = settings_path();
            if let Some(dir) = p.parent() {
                std::fs::create_dir_all(dir).context("create settings dir")?;
            }
            std::fs::write(
                &p,
                serde_json::to_vec_pretty(&val).context("encode settings")?,
            )
            .context("write settings")?;
            // apply the new speed cap to the live bucket immediately
            let kbps = val["speed_limit"].as_u64().unwrap_or(0);
            app.limiter.set_max(match kbps {
                0 => None,
                k => Some(k.saturating_mul(1024)),
            });
            let _ = app
                .events
                .send(json!({"t":"settings","settings":val}).to_string());
            // raising max_parallel should pull queued jobs in right away
            try_start_queued(&app).await;
            Ok(None)
        }
        Some("set-job") => {
            // live job tuning: speed cap / priority / queue label
            let id = v["id"].as_u64().context("id required")?;
            let mut sets: Vec<(&str, String)> = Vec::new(); // (col, literal)
            if let Some(k) = v["speed_limit"].as_u64() {
                let k = k.min(1 << 30);
                sets.push(("speed_limit", k.to_string()));
            }
            if let Some(p) = v["priority"].as_i64() {
                sets.push(("priority", p.clamp(-100, 100).to_string()));
            }
            if let Some(q) = v["queue"].as_str() {
                let q = sanitize_queue(q);
                // bound the literal: names are alphanumeric-only after sanitize
                sets.push(("queue", format!("'{q}'")));
            }
            if sets.is_empty() {
                bail!("nothing to set");
            }
            let assigns = sets
                .iter()
                .map(|(c, lit)| format!("{c}={lit}"))
                .collect::<Vec<_>>()
                .join(",");
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            {
                let db = app.db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
                let n = db.execute(
                    &format!("UPDATE jobs SET {assigns}, updated={now} WHERE id={id}"),
                    [],
                )?;
                if n == 0 {
                    bail!("no job {id}");
                }
            }
            // retune a running job's bucket instantly
            if let Some(k) = v["speed_limit"].as_u64() {
                let k = k.min(1 << 30);
                let max = match k {
                    0 => None,
                    k => Some(k * 1024),
                };
                if let Some(c) = app.ctl.lock().unwrap().get(&id) {
                    if let Some(b) = c.bucket.as_ref() {
                        b.set_max(max);
                        tracing::info!(target: "ext", "job {id}: speed limit -> {k} KiB/s (live)");
                    }
                }
            }
            // priority/queue changed the queue order: give the scheduler a nudge
            try_start_queued(&app).await;
            let _ = app.events.send(
                json!({"t":"job","id":id,"speed_limit":v["speed_limit"],"priority":v["priority"],"queue":v["queue"]})
                    .to_string(),
            );
            Ok(Some(json!({"t":"ok"}).to_string()))
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
            let files = v["files"].as_bool().unwrap_or(true);
            delete_job(&app, id, files)?;
            try_start_queued(&app).await;
            Ok(None)
        }
        Some("extract") => {
            let url = v["url"].as_str().context("url required")?.to_string();
            let cookie = v["cookie"].as_str().map(str::to_string);
            let ua = v["ua"].as_str().map(str::to_string);
            let media = tokio::time::timeout(
                Duration::from_secs(120),
                ytdlp::extract(&url, cookie.as_deref(), ua.as_deref()),
            )
            .await;
            match media {
                Ok(Ok(info)) => Ok(Some(
                    json!({
                        "t":"formats",
                        "url": info.webpage_url,
                        "title": info.title,
                        "duration": info.duration,
                        "formats": info.formats,
                    })
                    .to_string(),
                )),
                media_err => {
                    // yt-dlp failed: try a picture-gallery extractor before
                    // giving up, so galleries are first-class in Detect too
                    let g = if gallery::installed() {
                        Some(
                            tokio::time::timeout(
                                Duration::from_secs(120),
                                gallery::extract(&url, ua.as_deref()),
                            )
                            .await,
                        )
                    } else {
                        None
                    };
                    match g {
                        Some(Ok(Ok(g))) => Ok(Some(
                            json!({
                                "t":"formats",
                                "url": url,
                                "title": g.title,
                                "duration": null,
                                "gallery": true,
                                "files": g.items.len(),
                                "formats": [{
                                    "id":"gallery", "ext":"", "height":null,
                                    "vcodec":"none", "acodec":"none",
                                    "filesize":null, "progressive":true
                                }],
                            })
                            .to_string(),
                        )),
                        _ => {
                            // surface the yt-dlp failure; the gallery attempt
                            // was only a bonus
                            let msg = match media_err {
                                Ok(Err(e)) => format!("{e:#}"),
                                Err(_) => "extract timeout".to_string(),
                                Ok(Ok(_)) => "extraction failed".to_string(),
                            };
                            Err(anyhow::anyhow!("{msg}"))
                        }
                    }
                }
            }
        }
        Some("offer-page") => {
            let offer: Offer = serde_json::from_value(v).context("bad offer-page")?;
            tracing::info!(target: "ext", "offer-page arrived: {}", offer.url);
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

fn ui_running() -> bool {
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

async fn accept_job(app: Arc<App>, offer: Offer) -> Result<u64> {
    let t0 = std::time::Instant::now();
    // Repeated clicks (popup not up yet -> user clicks again) must not spawn
    // duplicate jobs: if this URL is already in flight, refocus its popup.
    if let Some(id) = app.db_active_by_url(&offer.url) {
        tracing::info!(target: "ext", "offer dedup: {} already active as job {id}", offer.url);
        if ui_running() {
            let _ = app.events.send(json!({"t":"show-job","id":id}).to_string());
        } else {
            bring_ui(&app);
        }
        return Ok(id);
    }
    let id = app.next_id.fetch_add(1, Ordering::SeqCst);
    let spec = serde_json::to_string(&offer_json(&offer))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let runner = if offer.page.unwrap_or(false) {
        // final output folder is resolved once yt-dlp reports the title
        let dir = offer
            .dir
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(expand_path)
            .unwrap_or_else(|| base_dir(&settings_json()));
        std::fs::create_dir_all(&dir).ok();
        RunnerSpec::Page(offer.clone(), false)
    } else {
        let (mut job, out_path, filename) = build_job(&offer)?;
        job.limits.limiter = Some(app.limiter.clone());
        job.limits.job_limiter = Some(job_bucket(offer.speed_limit.unwrap_or(0)));
        let confirming = offer.confirm.unwrap_or(false);
        let status = if confirming { "confirming" } else { "running" };
        app.db_exec({
            let url = offer.url.clone();
            let out_path = out_path.clone();
            let filename = filename.clone();
            let spec = spec.clone();
            let queue = sanitize_queue(offer.queue.as_deref().unwrap_or("main"));
            let prio = offer.priority.unwrap_or(0).clamp(-100, 100);
            let slim = offer.speed_limit.unwrap_or(0).min(1 << 30) as i64;
            move |db| {
                db.execute(
                    "INSERT INTO jobs(id,url,filename,out_path,size,done,status,error,created,updated,spec,queue,priority,speed_limit) \
                     VALUES(?1,?2,?3,?4,NULL,0,?7,NULL,?5,?5,?6,?8,?9,?10)",
                    rusqlite::params![id as i64, url, filename, out_path, now, spec, status, queue, prio, slim],
                )?;
                Ok(())
            }
        });
        if confirming {
            // intercepted browser download: the confirmation dialog must be
            // seen, so raise (or launch) the UI for this one
            bring_ui(&app);
            let _ = app.events.send(
                json!({"t":"confirm","id":id,"filename":filename,"url":offer.url,"out_path":out_path}).to_string(),
            );
            return Ok(id);
        }
        // intercepted-browser confirm path returned above; direct background
        // offers and page offers share the show-job tail below
        RunnerSpec::Direct(job)
    };
    if let RunnerSpec::Page(o, _) = &runner {
        let url = o.url.clone();
        let dir = o
            .dir
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(expand_path)
            .unwrap_or_else(|| base_dir(&settings_json()));
        let fname = format!(
            "{} — {}",
            host_of(&o.url),
            rdm_core::engine::sanitize_filename(&o.url)
        );
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
    // Background offers (video pill, context menu): open the job's detail
    // pane in place — never raise/steal focus while the user is browsing.
    // Only launch the UI when it is not running at all.
    if ui_running() {
        let _ = app.events.send(json!({"t":"show-job","id":id}).to_string());
        tracing::info!(target: "ext", "job {id}: show-job sent {}ms after offer", t0.elapsed().as_millis());
    } else {
        bring_ui(&app);
    }
    // queue when all download slots are busy (confirming jobs take no slot)
    if running_count(&app) >= max_parallel() {
        db_status(&app, id, "queued");
        let _ = app
            .events
            .send(json!({"t":"status","id":id,"status":"queued"}).to_string());
        return Ok(id);
    }
    spawn_job(app, id, runner);
    Ok(id)
}

fn host_of(url: &str) -> String {
    url.split("//")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or("page")
        .to_string()
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
        "queue": o.queue,
        "priority": o.priority,
        "speed_limit": o.speed_limit,
    })
}

fn build_job(offer: &Offer) -> Result<(Job, String, String)> {
    // resolve the name first: the category folder depends on the extension
    let clean = offer
        .filename
        .as_deref()
        .map(rdm_core::engine::sanitize_filename)
        .filter(|c| c != "download");
    let hint = clean
        .clone()
        .unwrap_or_else(|| rdm_core::engine::sanitize_filename(&offer.url));
    let dir = dir_for(offer.dir.as_deref(), &hint);
    std::fs::create_dir_all(&dir).ok();

    let mut job: Job = job_from_url(
        &offer.url,
        Some(dir.clone()),
        offer.segments.unwrap_or(8),
        Limits {
            dns_resolver: dns_resolver_from_settings(),
            sha256: sha256_on(),
            ..Limits::default()
        },
    );
    // HLS playlists are saved as the transport stream they carry
    if job
        .out
        .extension()
        .map(|e| e.eq_ignore_ascii_case("m3u8"))
        .unwrap_or(false)
    {
        job.out.set_extension("ts");
    }
    if let Some(clean) = clean {
        job.out = dir.join(clean);
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
    let filename = job
        .out
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    Ok((job, out_path, filename))
}

enum RunnerSpec {
    Direct(Job),
    Page(Offer, bool),
}

fn spawn_job(app: Arc<App>, id: u64, runner: RunnerSpec) {
    let (kill_tx, kill_rx) = watch::channel(false);
    // the same bucket the engine holds, so `set-job` can retune it live
    let bucket = match &runner {
        RunnerSpec::Direct(j) => j.limits.job_limiter.clone(),
        // page jobs build theirs inside run_page (phases share one bucket)
        RunnerSpec::Page(_, _) => None,
    };
    app.ctl.lock().unwrap().insert(
        id,
        JobCtl {
            kill: kill_tx,
            intent: "cancelled",
            bucket,
        },
    );
    let app2 = app.clone();
    tokio::spawn(async move {
        run_job(app2, id, runner, kill_rx).await;
    });
}

/* ---------- job queue: cap concurrent downloads, FIFO by id ---------- */

fn max_parallel() -> usize {
    settings_json()["max_parallel"]
        .as_u64()
        .unwrap_or(3)
        .clamp(1, 16) as usize
}

fn running_count(app: &Arc<App>) -> usize {
    app.ctl.lock().unwrap().len()
}

fn next_queued(app: &Arc<App>) -> Option<u64> {
    let ctl = app.ctl.lock().unwrap();
    app.db.lock().ok().and_then(|db| {
        db.query_row(
            // priority first (higher wins), FIFO inside the same priority
            "SELECT id FROM jobs WHERE status='queued' ORDER BY priority DESC, id LIMIT 1",
            [],
            |r| r.get::<_, i64>(0),
        )
        .ok()
        .and_then(|id| {
            let id = id as u64;
            if ctl.contains_key(&id) {
                None
            } else {
                Some(id)
            }
        })
    })
}

/// Pull queued jobs into free slots (called whenever a slot opens or may have).
async fn try_start_queued(app: &Arc<App>) {
    loop {
        if running_count(app) >= max_parallel() {
            return;
        }
        let Some(id) = next_queued(app) else { return };
        if resume_job(app, id, false).await.is_err() {
            return;
        }
    }
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
        let _ = app
            .events
            .send(json!({"t":"status","id":id,"status":intent}).to_string());
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
    // all slots busy again? put it back in the queue instead of oversubscribing
    if !restart && running_count(app) >= max_parallel() {
        let was = app.db_job_row(id).map(|(s, _)| s);
        if was.as_deref() != Some("queued") {
            db_status(app, id, "queued");
            let _ = app
                .events
                .send(json!({"t":"status","id":id,"status":"queued"}).to_string());
        }
        return Ok(());
    }
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
        job.limits.limiter = Some(app.limiter.clone());
        job.limits.job_limiter = Some(job_bucket(offer.speed_limit.unwrap_or(0)));
        job.fresh = restart;
        RunnerSpec::Direct(job)
    };
    if !restart {
        // keep the paused progress on screen until fresh bytes arrive
        db_status(app, id, "running");
        let _ = app
            .events
            .send(json!({"t":"status","id":id,"status":"running"}).to_string());
    }
    spawn_job(app.clone(), id, runner);
    Ok(())
}

/// May `path` be removed as a job's own folder? It must be strictly below
/// `$HOME`, and neither the base download folder itself nor any directory
/// containing it — page jobs store the BASE folder as out_path until a real
/// filename arrives, and one unguarded delete erased a whole ~/Downloads.
fn dir_delete_allowed(path: &Path, base: &Path) -> bool {
    let Ok(home) = std::env::var("HOME") else {
        return false;
    };
    let home = PathBuf::from(home);
    path != home && path.starts_with(&home) && path != base && !base.starts_with(path)
}

fn delete_job(app: &Arc<App>, id: u64, files: bool) -> Result<()> {
    {
        let ctl = app.ctl.lock().unwrap();
        if let Some(c) = ctl.get(&id) {
            let _ = c.kill.send(true);
        }
    }
    app.ctl.lock().unwrap().remove(&id);
    if files {
        if let Some((status, out)) = app.db_job_row(id) {
            if status != "completed" {
                if let Some(out) = out.filter(|p| !p.is_empty()) {
                    let path = PathBuf::from(&out);
                    if path.is_dir() {
                        // gallery jobs point at their own folder; the guard
                        // below refuses the download base folder and anything
                        // at or above it
                        let base = base_dir(&settings_json());
                        if dir_delete_allowed(&path, &base) {
                            let _ = std::fs::remove_dir_all(&path);
                        } else {
                            tracing::warn!(
                                "delete: refusing to remove folder {out:?} (base/home guard)"
                            );
                        }
                    } else {
                        let _ = std::fs::remove_file(format!("{out}.rdm-part.json"));
                        let _ = std::fs::remove_file(format!("{out}.rdm-hls.json"));
                        let _ = std::fs::remove_file(&path);
                        // page jobs split into {stem}.v.ext / {stem}.a.ext parts
                        if let (Some(dir), Some(stem)) = (
                            path.parent(),
                            path.file_stem().map(|s| s.to_string_lossy().to_string()),
                        ) {
                            for pat in [format!("{stem}.v.*"), format!("{stem}.a.*")] {
                                if let Ok(rd) = std::fs::read_dir(dir) {
                                    for e in rd.flatten() {
                                        let name = e.file_name().to_string_lossy().to_string();
                                        let base =
                                            name.trim_end_matches(".rdm-part.json").to_string();
                                        if glob_like(&pat, &base) || glob_like(&pat, &name) {
                                            let _ = std::fs::remove_file(e.path());
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    let id2 = id;
    app.db_exec(move |db| {
        db.execute("DELETE FROM jobs WHERE id=?1", [id2 as i64])
            .map(|_| ())
    });
    app.live.lock().unwrap().remove(&id);
    let _ = app.events.send(json!({"t":"deleted","id":id}).to_string());
    Ok(())
}

/// Tiny wildcard match: pattern `stem.v.*` matches `stem.v.mp4` etc.
fn glob_like(pat: &str, name: &str) -> bool {
    let Some((head, _)) = pat.split_once(".*") else {
        return pat == name;
    };
    name.starts_with(head)
}

async fn run_job(app: Arc<App>, id: u64, runner: RunnerSpec, kill_rx: watch::Receiver<bool>) {
    match runner {
        RunnerSpec::Direct(job) => run_direct(app.clone(), id, job, kill_rx).await,
        RunnerSpec::Page(offer, restart) => {
            run_page(app.clone(), id, offer, restart, kill_rx).await
        }
    }
    // a slot just opened: pull the next queued job in
    try_start_queued(&app).await;
}

/// Download one media URL with the engine, emitting aggregated progress.
#[derive(Default)]
struct PhaseStat {
    done: u64,
    size: Option<u64>,
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
            Progress::Bytes {
                total_done, size, ..
            } => {
                st.done = total_done;
                st.size = size;
            }
            Progress::Detail { segments } => {
                let _ = app
                    .events
                    .send(json!({"t":"detail","id":id,"segments":segments}).to_string());
            }
            Progress::Retrying {
                index,
                attempt,
                error,
            } => {
                let _ = app.events.send(
                    json!({"t":"retrying","id":id,"segment":index,"attempt":attempt,"error":error})
                        .to_string(),
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
            // later phases (audio, mux) start, like a classic download manager's dynamic total.
            let size = st.size.map(|s| s + other_size.unwrap_or(0)).or(other_size);
            app.note_live(id, done, size);
            let _ = app
                .events
                .send(json!({"t":"progress","id":id,"done":done,"size":size}).to_string());
        }
    }
    match handle.await {
        Ok(Ok(())) => Ok(st),
        _ => Err(anyhow::anyhow!("engine failed")),
    }
}

#[allow(clippy::too_many_arguments)]
fn job_for(
    url: &str,
    out: PathBuf,
    offer: &Offer,
    segments: u64,
    limiter: std::sync::Arc<rdm_core::Limiter>,
    jlim: std::sync::Arc<rdm_core::Limiter>,
) -> Job {
    let mut job = job_from_url(
        url,
        Some(
            out.clone()
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_default(),
        ),
        segments,
        Limits {
            limiter: Some(limiter),
            job_limiter: Some(jlim),
            dns_resolver: dns_resolver_from_settings(),
            sha256: sha256_on(),
            ..Limits::default()
        },
    );
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

async fn ffmpeg_mux(
    video: &std::path::Path,
    audio: Option<&std::path::Path>,
    out: &std::path::Path,
) -> Result<()> {
    use std::process::Stdio;
    let mut cmd = tokio::process::Command::new("ffmpeg");
    cmd.arg("-y").arg("-i").arg(video);
    if let Some(a) = audio {
        cmd.arg("-i").arg(a);
    }
    cmd.arg("-c")
        .arg("copy")
        .arg(out)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let out_err = cmd.stderr(Stdio::piped());
    let mut child = out_err.spawn().context("spawn ffmpeg")?;
    let mut err = String::new();
    if let Some(mut se) = child.stderr.take() {
        use tokio::io::AsyncReadExt;
        let _ = se.read_to_string(&mut err).await;
    }
    let st = tokio::time::timeout(Duration::from_secs(600), child.wait())
        .await
        .context("ffmpeg timeout")??;
    if !st.success() {
        let tail = err.lines().rev().take(3).collect::<Vec<_>>().join(" | ");
        bail!("ffmpeg: {tail}");
    }
    Ok(())
}

async fn run_page(
    app: Arc<App>,
    id: u64,
    offer: Offer,
    restart: bool,
    mut kill_rx: watch::Receiver<bool>,
) {
    let finish = |status: &str, err: Option<String>| {
        let _ = app
            .events
            .send(json!({"t":status,"id":id,"done":0,"error":err}).to_string());
    };
    let _ = app
        .events
        .send(json!({"t":"started","id":id,"size":null}).to_string());
    let _ = app
        .events
        .send(json!({"t":"status","id":id,"status":"resolving"}).to_string());

    let info = {
        // abortable: pause/stop must not wait out a slow extraction
        let mut kill_extract = kill_rx.clone();
        let extraction = ytdlp::extract(&offer.url, offer.cookie.as_deref(), offer.ua.as_deref());
        tokio::pin!(extraction);
        let media = tokio::select! {
            res = &mut extraction => res,
            _ = kill_extract.changed() => {
                cancel_page(&app, id).await;
                return;
            }
        };
        match media {
            Ok(i) => Some(i),
            Err(media_err) => {
                // yt-dlp cannot do picture galleries (pixiv, danbooru, imgur
                // albums, ...): hand the page to gallery-dl before failing
                if gallery::installed() {
                    let g = gallery::extract(&offer.url, offer.ua.as_deref());
                    tokio::pin!(g);
                    tokio::select! {
                        res = &mut g => if let Ok(g) = res {
                            run_gallery(app, id, &offer, g, kill_rx).await;
                            return;
                        },
                        _ = kill_extract.changed() => {
                            cancel_page(&app, id).await;
                            return;
                        }
                    }
                }
                // last resort: some hosts (lulustream / luluvido, ...) have no
                // extractor support at all, but embed the HLS playlist inside
                // a Dean Edwards packed script — unpack it and hand the URL to
                // the plain HLS engine
                let rescue =
                    packer::extract(&offer.url, offer.ua.as_deref(), offer.cookie.as_deref());
                tokio::pin!(rescue);
                let rescued = tokio::select! {
                    res = &mut rescue => match res {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(target: "ext", "packer rescue fetch failed: {e:#}");
                            None
                        }
                    },
                    _ = kill_extract.changed() => {
                        cancel_page(&app, id).await;
                        return;
                    }
                };
                if rescued.is_none() {
                    tracing::info!(target: "ext", "packer rescue: nothing packed in {}", offer.url);
                }
                if let Some((m3u8, title)) = rescued {
                    tracing::info!(target: "ext", "packer rescue: {} -> {m3u8}", offer.url);
                    let mut o = offer.clone();
                    o.url = m3u8;
                    o.referer = Some(offer.url.clone());
                    o.filename = Some(format!(
                        "{}.ts",
                        rdm_core::engine::sanitize_filename(&title)
                    ));
                    if let Ok((mut job, out_path, filename)) = build_job(&o) {
                        job.limits.limiter = Some(app.limiter.clone());
                        job.limits.job_limiter = Some(job_bucket(offer.speed_limit.unwrap_or(0)));
                        app.db_exec({
                            let out_path = out_path.clone();
                            let filename = filename.clone();
                            move |db| {
                                db.execute(
                                    "UPDATE jobs SET filename=?1, out_path=?2 WHERE id=?3",
                                    rusqlite::params![filename, out_path, id as i64],
                                )?;
                                Ok(())
                            }
                        });
                        db_status(&app, id, "running");
                        let _ = app
                            .events
                            .send(json!({"t":"status","id":id,"status":"running"}).to_string());
                        run_direct(app, id, job, kill_rx).await;
                        return;
                    }
                }
                app.db_update(id, 0, "failed", Some(&format!("{media_err:#}")), None);
                finish("failed", Some(format!("{media_err:#}")));
                app.ctl.lock().unwrap().remove(&id);
                return;
            }
        }
    };
    let info = match info {
        Some(i) => i,
        None => return,
    };
    if *kill_rx.borrow() {
        cancel_page(&app, id).await;
        return;
    }
    let (v, a) = ytdlp::pick(
        &info,
        offer.height.unwrap_or(1080),
        offer.audio_only.unwrap_or(false),
    );
    let base = rdm_core::engine::sanitize_filename(&info.title);
    let vext = rdm_core::engine::sanitize_ext(&v.ext);
    let final_name = if a.is_some() {
        format!("{base}.mp4")
    } else {
        format!("{base}.{vext}")
    };
    let dir = dir_for(offer.dir.as_deref(), &final_name);
    std::fs::create_dir_all(&dir).ok();
    let vpath = dir.join(format!("{base}.v.{vext}"));
    let apath = a.as_ref().map(|af| {
        dir.join(format!(
            "{base}.a.{}",
            rdm_core::engine::sanitize_ext(&af.ext)
        ))
    });
    let final_path = dir.join(final_name);
    {
        let fname = final_path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or(base.clone());
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
    let _ = app
        .events
        .send(json!({"t":"status","id":id,"status":"running"}).to_string());

    // total = video + audio estimate up front, so the percent is monotonic:
    // otherwise phase 1 ends at 100% and drops when the audio phase grows the
    // total (the classic "100% -> 97% -> 100%" wobble)
    let a_size = a.as_ref().and_then(|f| f.filesize);
    let est_total = match (v.filesize, a_size) {
        (Some(vv), aa) => Some(vv + aa.unwrap_or(0)),
        (_, Some(aa)) => Some(aa),
        _ => None,
    };
    if let Some(total) = est_total.filter(|t| *t > 0) {
        app.db_exec(move |db| {
            db.execute(
                "UPDATE jobs SET size=?1 WHERE id=?2 AND (size IS NULL OR size < ?1)",
                rusqlite::params![total as i64, id as i64],
            )?;
            Ok(())
        });
    }

    // phase 1: video

    let mut ga = PhaseStat::default();

    // one bucket for every phase; also publish it to JobCtl so `set-job`
    // retunes a live page download
    let jlim = {
        let b = job_bucket(offer.speed_limit.unwrap_or(0));
        if let Some(c) = app.ctl.lock().unwrap().get_mut(&id) {
            c.bucket = Some(b.clone());
        }
        b
    };
    let mut vjob = job_for(
        &v.url,
        vpath.clone(),
        &offer,
        offer.segments.unwrap_or(8),
        app.limiter.clone(),
        jlim.clone(),
    );
    vjob.fresh = restart;
    let gv: PhaseStat = match run_media(&app, id, vjob, &mut kill_rx, 0, a_size).await {
        Ok(st) => st,
        Err(e) => return page_fail(&app, id, e).await,
    };
    if *kill_rx.borrow() {
        cancel_page(&app, id).await;
        return;
    }

    // phase 2: audio
    if let (Some(af), Some(ap)) = (a.as_ref(), apath.as_ref()) {
        let mut ajob = job_for(
            &af.url,
            ap.clone(),
            &offer,
            offer.segments.unwrap_or(4),
            app.limiter.clone(),
            jlim.clone(),
        );
        ajob.fresh = restart;
        match run_media(&app, id, ajob, &mut kill_rx, gv.done, None).await {
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
        app.note_live(
            id,
            gv.done + ga.done,
            gv.size.map(|v| v + ga.size.unwrap_or(0)),
        );
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
            let size = final_path
                .metadata()
                .map(|m| m.len())
                .unwrap_or(gv.done + ga.done);
            let _ = std::fs::remove_file(&vpath);
            if let Some(ap) = apath.as_ref() {
                let _ = std::fs::remove_file(ap);
            }
            // the muxed file is the finished artifact: verify + optional hash
            let sha = if sha256_on() {
                rdm_core::engine::sha256_file(&final_path).await.ok()
            } else {
                None
            };
            let _ = app.events.send(
                json!({"t":"done","id":id,"size":size,"sha256":sha,"verified":true}).to_string(),
            );
            app.db_update(id, size, "completed", None, Some(size));
            app.db_verify(id, sha.as_deref());
        }
        Err(e) => {
            let msg = format!("{e:#}");
            let _ = app
                .events
                .send(json!({"t":"failed","id":id,"error":msg}).to_string());
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
        let _ = app
            .events
            .send(json!({"t":"failed","id":id,"error":msg}).to_string());
        app.db_update(id, 0, "failed", Some(&msg), None);
        app.ctl.lock().unwrap().remove(&id);
    }
}

async fn cancel_page(app: &Arc<App>, id: u64) {
    let intent = app
        .ctl
        .lock()
        .unwrap()
        .get(&id)
        .map(|c| c.intent)
        .unwrap_or("cancelled");
    let _ = app
        .events
        .send(json!({"t":intent,"id":id,"done":0}).to_string());
    app.db_update(id, 0, intent, None, None);
    app.ctl.lock().unwrap().remove(&id);
}

/// Gallery download (gallery-dl resolved): every file goes through the
/// segmented engine one by one into a single folder; progress is the running
/// byte total, so pause/resume simply skips files that already exist.
#[allow(clippy::too_many_arguments)]
async fn run_gallery(
    app: Arc<App>,
    id: u64,
    offer: &Offer,
    g: gallery::Gallery,
    mut kill_rx: watch::Receiver<bool>,
) {
    let _ = app
        .events
        .send(json!({"t":"started","id":id,"size":null}).to_string());
    let _ = app
        .events
        .send(json!({"t":"status","id":id,"status":"resolving"}).to_string());

    let n = g.items.len();
    let base = if g.title.trim().is_empty() {
        host_of(&offer.url)
    } else {
        rdm_core::engine::sanitize_filename(g.title.trim())
    };
    // ".jpg" sample routes the folder into the Images category override
    let dir = dir_for(offer.dir.as_deref(), &format!("{base}.jpg"));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        page_fail(&app, id, anyhow::anyhow!("create folder: {e}")).await;
        return;
    }
    let label = if n == 1 {
        format!("{base} (1 file)")
    } else {
        format!("{base} ({n} files)")
    };
    let outp = dir.to_string_lossy().to_string();
    app.db_exec(move |db| {
        db.execute(
            "UPDATE jobs SET filename=?1, out_path=?2 WHERE id=?3",
            rusqlite::params![label, outp, id as i64],
        )?;
        Ok(())
    });
    db_status(&app, id, "running");
    let _ = app
        .events
        .send(json!({"t":"status","id":id,"status":"running"}).to_string());

    // one shared bucket for every file; publish to JobCtl for live retuning
    let jlim = {
        let b = job_bucket(offer.speed_limit.unwrap_or(0));
        if let Some(c) = app.ctl.lock().unwrap().get_mut(&id) {
            c.bucket = Some(b.clone());
        }
        b
    };

    let mut done: u64 = 0;
    for (i, (url, name)) in g.items.into_iter().enumerate() {
        // resume: files already on disk (non-empty) are skipped and counted
        let fname = if n > 1 {
            format!("{:04}_{}", i + 1, name)
        } else {
            name
        };
        let path = dir.join(&fname);
        if let Ok(md) = std::fs::metadata(&path) {
            if md.len() > 0 {
                done += md.len();
                continue;
            }
        }
        if *kill_rx.borrow() {
            cancel_page(&app, id).await;
            return;
        }
        let job = job_for(&url, path, offer, 4, app.limiter.clone(), jlim.clone());
        match run_media(&app, id, job, &mut kill_rx, done, None).await {
            Ok(st) => {
                done += st.size.unwrap_or(st.done);
            }
            Err(e) => {
                page_fail(&app, id, e).await;
                return;
            }
        }
        if *kill_rx.borrow() {
            cancel_page(&app, id).await;
            return;
        }
    }
    let _ = app
        .events
        .send(json!({"t":"done","id":id,"size":done,"done":done,"verified":true}).to_string());
    app.db_update(id, done, "completed", None, Some(done));
    // every file went through the engine's on-disk size verification;
    // a folder job has no single hash
    app.db_verify(id, None);
    app.ctl.lock().unwrap().remove(&id);
}

/// Distinguish a flaky-network failure (worth another attempt) from a
/// definitive answer (403/404 and friends are never retried).
fn retryable_error(e: &str) -> bool {
    if e.starts_with("request:") || e.contains("short read") {
        return true;
    }
    e.split("HTTP ")
        .nth(1)
        .and_then(|s| s.split([' ', '@']).next())
        .and_then(|s| s.parse::<u16>().ok())
        .map(|code| matches!(code, 408 | 425 | 429 | 500..=599))
        .unwrap_or(false)
}

const AUTO_RETRY_MAX: u32 = 3;

async fn run_direct(app: Arc<App>, id: u64, job: Job, kill_rx: watch::Receiver<bool>) {
    let mut attempt = 0u32;
    let mut done = 0u64;
    let mut size: Option<u64> = None;
    loop {
        let retry_err =
            run_direct_attempt(&app, id, &job, &kill_rx, attempt, &mut done, &mut size).await;
        let Some(err) = retry_err else {
            break;
        };
        attempt += 1;
        // transient failure: pause, then restart the engine — it resumes from
        // the saved partial state instead of restarting from byte zero
        app.db_update(id, done, "retrying", Some(&err), size);
        let _ = app.events.send(
            json!({"t":"status","id":id,"status":"retrying","attempt":attempt,"max":AUTO_RETRY_MAX,"error":err}).to_string(),
        );
        let wait = Duration::from_secs(5 * 3u64.pow(attempt - 1)); // 5s, 15s, 45s
        let mut k = kill_rx.clone();
        let aborted = tokio::select! {
            _ = tokio::time::sleep(wait) => false,
            _ = k.changed() => true,
        };
        if aborted {
            let intent = app
                .ctl
                .lock()
                .unwrap()
                .get(&id)
                .map(|c| c.intent)
                .unwrap_or("cancelled");
            let _ = app
                .events
                .send(json!({"t":intent,"id":id,"done":done}).to_string());
            app.db_update(id, done, intent, None, size);
            break;
        }
    }
    app.ctl.lock().unwrap().remove(&id);
}

/// One engine run. Returns Some(transient error) when the whole download
/// failed but is worth retrying; None means the job reached any terminal
/// state (completed/failed/cancelled) and events were already sent.
async fn run_direct_attempt(
    app: &Arc<App>,
    id: u64,
    job: &Job,
    kill_rx: &watch::Receiver<bool>,
    attempt: u32,
    done: &mut u64,
    size: &mut Option<u64>,
) -> Option<String> {
    let (tx, mut rx) = mpsc::channel::<Progress>(512);
    let handle = tokio::spawn(download(job.clone(), tx, kill_rx.clone()));
    let mut last = Instant::now() - Duration::from_secs(10);
    let mut transient: Option<String> = None;
    let mut terminal_seen = false;
    while let Some(p) = rx.recv().await {
        let mut line: Option<String> = None;
        let mut terminal: Option<(&'static str, Option<String>)> = None;
        match &p {
            Progress::Started {
                size: s, filename, ..
            } => {
                *size = *s;
                let mut ev = json!({"t":"started","id":id,"size":s});
                // the engine can rescue information-less names mid-start
                // (Gmail "/attachment/u/0/" -> "0" via Content-Disposition):
                // carry that rename into the DB row and the UI list
                if let Some((old, out_path)) = app.db_get_name(id) {
                    if old != *filename {
                        let new_out = std::path::Path::new(&out_path).parent().map(|d| {
                            d.join(filename.trim_start_matches('/'))
                                .to_string_lossy()
                                .to_string()
                        });
                        if let Some(new_out) = new_out {
                            let (f, o) = (filename.clone(), new_out.clone());
                            app.db_exec(move |db| {
                                db.execute(
                                    "UPDATE jobs SET filename=?1, out_path=?2 WHERE id=?3",
                                    rusqlite::params![f, o, id as i64],
                                )?;
                                Ok(())
                            });
                            ev["filename"] = json!(filename);
                            ev["out_path"] = json!(new_out);
                            tracing::info!(target: "ext", "job {id}: renamed \"{old}\" -> \"{filename}\"");
                        }
                    }
                }
                line = Some(ev.to_string());
            }
            Progress::Detail { segments } => {
                line = Some(json!({"t":"detail","id":id,"segments":segments}).to_string());
            }
            Progress::Bytes {
                total_done,
                size: s,
                ..
            } => {
                *done = *total_done;
                *size = *s;
                if last.elapsed() >= Duration::from_millis(100) {
                    app.note_live(id, *done, *size);
                    line =
                        Some(json!({"t":"progress","id":id,"done":done,"size":size}).to_string());
                    last = Instant::now();
                }
            }
            Progress::Retrying {
                index,
                attempt,
                error,
            } => {
                line = Some(
                    json!({"t":"retrying","id":id,"segment":index,"attempt":attempt,"error":error})
                        .to_string(),
                );
            }
            Progress::Completed { size: s, sha256 } => {
                *done = *s;
                *size = Some(*s);
                line = Some(
                    json!({"t":"done","id":id,"size":s,"sha256":sha256,"verified":true})
                        .to_string(),
                );
                app.db_verify(id, sha256.as_deref());
                terminal = Some(("completed", None));
                terminal_seen = true;
            }
            Progress::Failed { error } => {
                if attempt < AUTO_RETRY_MAX && retryable_error(error) {
                    transient = Some(error.clone());
                } else {
                    line = Some(json!({"t":"failed","id":id,"error":error}).to_string());
                    terminal = Some(("failed", Some(error.clone())));
                    terminal_seen = true;
                }
            }
            Progress::Cancelled { total_done } => {
                *done = *total_done;
                let intent = app
                    .ctl
                    .lock()
                    .unwrap()
                    .get(&id)
                    .map(|c| c.intent)
                    .unwrap_or("cancelled");
                line = Some(json!({"t":intent,"id":id,"done":done}).to_string());
                terminal = Some((intent, None));
                terminal_seen = true;
            }
            Progress::SegmentDone { .. } => {}
        }
        if let Some(s) = line {
            let _ = app.events.send(s);
        }
        if let Some((status, error)) = terminal {
            app.db_update(id, *done, status, error.as_deref(), *size);
        } else if last.elapsed() >= Duration::from_secs(1) {
            app.db_update(id, *done, "running", None, *size);
        }
        if transient.is_some() {
            break;
        }
    }
    if transient.is_some() {
        return transient;
    }
    if terminal_seen {
        return None;
    }
    // The engine returned before emitting any event — typically the probe
    // bailing on an HTTP error (`?` propagation in download()). Without this
    // the channel just closes and the job would sit in "running" forever.
    let err = match handle.await {
        Ok(Ok(())) => "download ended without an event".to_string(),
        Ok(Err(e)) => format!("{e:#}"),
        Err(e) => format!("download task panicked: {e}"),
    };
    if attempt < AUTO_RETRY_MAX && retryable_error(&err) {
        return Some(err);
    }
    let _ = app
        .events
        .send(json!({"t":"failed","id":id,"error":err}).to_string());
    app.db_update(id, *done, "failed", Some(&err), *size);
    None
}

fn list_jobs(app: &App) -> Result<serde_json::Value> {
    let db = app.db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    let mut stmt = db.prepare(
        "SELECT id,url,filename,out_path,size,done,status,error,queue,priority,speed_limit,sha256,verified \
         FROM jobs ORDER BY id DESC LIMIT 200",
    )?;
    let rows = stmt.query_map([], |r| {
        let status: String = r.get(6)?;
        let out_path: String = r.get(3)?;
        // a finished job whose file/folder is gone (moved or deleted outside
        // RDM) gets a badge instead of silently claiming "Done"
        let missing = status == "completed" && !std::path::Path::new(&out_path).exists();
        let dir = std::path::Path::new(&out_path).is_dir();
        Ok(json!({
            "id": r.get::<_, i64>(0)?,
            "url": r.get::<_, String>(1)?,
            "filename": r.get::<_, String>(2)?,
            "out_path": out_path,
            "size": r.get::<_, Option<i64>>(4)?,
            "done": r.get::<_, i64>(5)?,
            "status": status,
            "error": r.get::<_, Option<String>>(7)?,
            "missing": missing,
            "dir": dir,
            "queue": r.get::<_, String>(8)?,
            "priority": r.get::<_, i64>(9)?,
            "speed_limit": r.get::<_, i64>(10)?,
            "sha256": r.get::<_, Option<String>>(11)?,
            "verified": r.get::<_, i64>(12)? != 0,
        }))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    // running jobs carry their live byte counts in `live`, not the DB row
    let live = app
        .live
        .lock()
        .map_err(|e| anyhow::anyhow!("live lock: {e}"))?;
    for j in out.iter_mut() {
        if !matches!(j["status"].as_str(), Some("running") | Some("resolving")) {
            continue;
        }
        let Some(id) = j["id"].as_i64() else { continue };
        if let Some((done, size)) = live.get(&(id as u64)) {
            j["done"] = json!(done);
            if let Some(s) = size {
                j["size"] = json!(s);
            }
        }
    }
    Ok(serde_json::Value::Array(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn delete_guard_blocks_base_and_ancestors() {
        let home = std::env::var("HOME").map(PathBuf::from).unwrap();
        let base = home.join("Downloads");
        // page job out_path == the base folder itself
        assert!(!dir_delete_allowed(&base, &base));
        // dir="~" collapses to $HOME
        assert!(!dir_delete_allowed(&home, &base));
        // filesystem root / anything outside $HOME
        assert!(!dir_delete_allowed(Path::new("/"), &base));
        assert!(!dir_delete_allowed(Path::new("/etc"), &base));
        // a gallery job's own folder below base: allowed
        assert!(dir_delete_allowed(&base.join("gallery-123"), &base));
        // a category override folder elsewhere under $HOME: allowed
        assert!(dir_delete_allowed(&home.join("Pictures").join("g"), &base));
    }
}
