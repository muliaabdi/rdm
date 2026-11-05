//! Job lifecycle: accept offers, build engine jobs, the download queue
//! (parallel cap, priority + FIFO), and stop/resume/delete.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, COOKIE, REFERER, USER_AGENT};
use serde_json::json;
use tokio::sync::watch;
use tokio::time::Duration;

use rdm_core::{job_from_url, Job, Limits};

use crate::runners::{run_direct, run_page};
use crate::settings::{
    base_dir, dir_for, dns_resolver_from_settings, expand_path, settings_json, sha256_on,
};
use crate::uilaunch::{bring_ui, ui_running};
use crate::{App, Offer};

pub(crate) fn sanitize_queue(q: &str) -> String {
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
pub(crate) fn job_bucket(kbps: u64) -> std::sync::Arc<rdm_core::Limiter> {
    let max = match kbps {
        0 => None,
        k => Some(k.saturating_mul(1024)),
    };
    std::sync::Arc::new(rdm_core::Limiter::new(max))
}

pub(crate) async fn accept_job(app: Arc<App>, offer: Offer) -> Result<u64> {
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

pub(crate) fn host_of(url: &str) -> String {
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

pub(crate) fn build_job(offer: &Offer) -> Result<(Job, String, String)> {
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

pub(crate) enum RunnerSpec {
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
        crate::JobCtl {
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

/* ---------- job queue: cap concurrent downloads, priority + FIFO ---------- */

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
pub(crate) async fn try_start_queued(app: &Arc<App>) {
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
pub(crate) fn db_status(app: &Arc<App>, id: u64, status: &str) {
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

pub(crate) async fn stop_job(app: &Arc<App>, id: u64, intent: &'static str) -> Result<()> {
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

pub(crate) async fn resume_job(app: &Arc<App>, id: u64, restart: bool) -> Result<()> {
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

pub(crate) fn delete_job(app: &Arc<App>, id: u64, files: bool) -> Result<()> {
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
