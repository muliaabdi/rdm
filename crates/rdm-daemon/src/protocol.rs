//! Socket protocol: JSON-lines request handling and the job-list snapshot.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::broadcast;

use crate::jobs::{accept_job, delete_job, resume_job, stop_job, try_start_queued};
use crate::settings::{settings_json, settings_path};
use crate::{gallery, ytdlp, App, Offer};

/// Messages (one JSON object per line, both directions):
/// - in:  {"t":"offer","url":..,"filename":..,"cookie":..,"referer":..,"ua":..,"dir":..,"segments":N}
///   {"t":"list"} | {"t":"pause","id":N} | {"t":"resume","id":N} | {"t":"cancel","id":N}
///   {"t":"restart","id":N} | {"t":"delete","id":N}
/// - out: {"t":"accepted","id":N} | {"t":"started",..} | {"t":"progress",..} | {"t":"detail",..}
///   | {"t":"done",..} | {"t":"failed",..} | {"t":"cancelled",..} | {"t":"jobs",..}
pub(crate) async fn handle_client(app: Arc<App>, stream: UnixStream) {
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
                let q = crate::jobs::sanitize_queue(q);
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

pub(crate) fn list_jobs(app: &App) -> Result<serde_json::Value> {
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
