//! Daemon: JSON-lines API over a 0600 Unix socket.
//!
//! Modules: [`settings`] (settings.json + path helpers), [`protocol`]
//! (socket messages), [`jobs`] (job lifecycle + queue), [`runners`]
//! (engine drivers), [`uilaunch`] (focus/launch rdm-ui).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use tokio::net::{UnixListener, UnixStream};

pub mod gallery;
mod jobs;
pub mod packer;
mod protocol;
mod runners;
mod settings;
mod uilaunch;
pub mod ytdlp;

pub use settings::{dns_resolver_from_settings, settings_path};

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

pub(crate) struct JobCtl {
    pub(crate) kill: tokio::sync::watch::Sender<bool>,
    pub(crate) intent: &'static str, // status recorded when the engine reports cancellation
    /// Per-job speed bucket; shared with the running engine so `set-job`
    /// retunes a live download without a restart.
    pub(crate) bucket: Option<std::sync::Arc<rdm_core::Limiter>>,
}

pub(crate) struct App {
    pub(crate) db: Mutex<rusqlite::Connection>,
    pub(crate) events: tokio::sync::broadcast::Sender<String>,
    pub(crate) next_id: AtomicU64,
    pub(crate) ctl: Mutex<HashMap<u64, JobCtl>>,
    // latest in-flight progress; the DB row only gets `done` on terminal or
    // pause, so snapshots (`list`) read live values from here instead
    pub(crate) live: Mutex<HashMap<u64, (i64, Option<i64>)>>,
    // process-wide token bucket so the speed cap covers every active job
    pub(crate) limiter: std::sync::Arc<rdm_core::Limiter>,
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
        let (events, _) = tokio::sync::broadcast::channel(256);
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
            limiter: std::sync::Arc::new(rdm_core::Limiter::new(settings::speed_limit_bytes())),
        })
    }

    pub(crate) fn db_update(
        &self,
        id: u64,
        done: u64,
        status: &str,
        error: Option<&str>,
        size: Option<u64>,
    ) {
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
    pub(crate) fn db_verify(&self, id: u64, sha256: Option<&str>) {
        self.db_exec(move |db| {
            db.execute(
                "UPDATE jobs SET verified=1, sha256=?1 WHERE id=?2",
                rusqlite::params![sha256, id as i64],
            )?;
            Ok(())
        });
    }

    pub(crate) fn note_live(&self, id: u64, done: u64, size: Option<u64>) {
        self.live
            .lock()
            .unwrap()
            .insert(id, (done as i64, size.map(|s| s as i64)));
    }

    pub(crate) fn db_exec<F>(&self, f: F)
    where
        F: FnOnce(&rusqlite::Connection) -> rusqlite::Result<()>,
    {
        if let Ok(db) = self.db.lock() {
            if let Err(e) = f(&db) {
                tracing::warn!("db: {e}");
            }
        }
    }

    pub(crate) fn db_get_spec(&self, id: u64) -> Option<String> {
        let db = self.db.lock().ok()?;
        db.query_row("SELECT spec FROM jobs WHERE id=?1", [id as i64], |r| {
            r.get::<_, Option<String>>(0)
        })
        .ok()
        .flatten()
    }

    /// Current (filename, out_path) row, for detecting engine-side renames.
    pub(crate) fn db_get_name(&self, id: u64) -> Option<(String, String)> {
        let db = self.db.lock().ok()?;
        db.query_row(
            "SELECT filename, out_path FROM jobs WHERE id=?1",
            [id as i64],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .ok()
    }

    /// Newest non-terminal job id already downloading `url`, if any.
    pub(crate) fn db_active_by_url(&self, url: &str) -> Option<u64> {
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
    pub(crate) fn db_job_row(&self, id: u64) -> Option<(String, Option<String>)> {
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
pub(crate) struct Offer {
    pub(crate) url: String,
    pub(crate) filename: Option<String>,
    pub(crate) cookie: Option<String>,
    pub(crate) referer: Option<String>,
    pub(crate) ua: Option<String>,
    pub(crate) dir: Option<String>,
    pub(crate) segments: Option<u64>,
    /// true: url is a video page; resolve via yt-dlp (composite job)
    pub(crate) page: Option<bool>,
    /// preferred max height for page downloads
    pub(crate) height: Option<u64>,
    pub(crate) audio_only: Option<bool>,
    /// true: create the job in `confirming` state and wait for the UI
    /// (used for intercepted browser downloads)
    pub(crate) confirm: Option<bool>,
    /// named queue label ("main" by default)
    pub(crate) queue: Option<String>,
    /// dispatch order for queued jobs; higher starts first
    pub(crate) priority: Option<i64>,
    /// per-job speed cap in KiB/s (0 = follow the global setting)
    pub(crate) speed_limit: Option<u64>,
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
    tokio::spawn(settings::ytdlp_updater());
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
            protocol::handle_client(app, stream).await;
        });
    }
}
