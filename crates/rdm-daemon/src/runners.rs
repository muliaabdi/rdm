//! Engine drivers: plain segmented downloads (with auto-retry), yt-dlp page
//! jobs (video+audio+mux), gallery jobs, and the packed-HLS rescue path.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, COOKIE, REFERER, USER_AGENT};
use serde_json::json;
use tokio::sync::{mpsc, watch};
use tokio::time::{Duration, Instant};

use rdm_core::{download, job_from_url, Job, Limits, Progress};

use crate::jobs::{build_job, db_status, host_of, job_bucket};
use crate::settings::{dir_for, dns_resolver_from_settings, sha256_on};
use crate::{gallery, packer, ytdlp, App, Offer};

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

pub(crate) async fn run_page(
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

pub(crate) async fn run_direct(app: Arc<App>, id: u64, job: Job, kill_rx: watch::Receiver<bool>) {
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
