//! Generic HLS (m3u8) downloader: VOD playlists, unencrypted.
//!
//! Segment URLs are fetched in parallel (`job.segments` workers) and written
//! in playlist order. Resume state lives next to the output as
//! `{out}.rdm-hls.json` with the list of finished segment indices.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, watch};

use crate::engine::Job;
use crate::events::{Progress, SegmentInfo};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HlsState {
    url: String,
    done: Vec<u64>, // finished segment indices
}

fn state_path(out: &std::path::Path) -> PathBuf {
    let mut s = out.as_os_str().to_os_string();
    s.push(".rdm-hls.json");
    PathBuf::from(s)
}

/// Detect an HLS playlist from probe metadata.
pub fn looks_like_playlist(content_type: Option<&str>, url: &str) -> bool {
    if let Some(ct) = content_type {
        if ct.to_ascii_lowercase().contains("mpegurl") {
            return true;
        }
    }
    let path = url.split('#').next().unwrap_or(url);
    let path = path.split('?').next().unwrap_or(path);
    path.to_ascii_lowercase().ends_with(".m3u8")
}

/// Resolve a playlist line against the playlist URL (absolute, root-relative,
/// or plain relative).
fn join_url(base: &str, seg: &str) -> String {
    if seg.starts_with("http://") || seg.starts_with("https://") || seg.starts_with("data:") {
        return seg.to_string();
    }
    let Some((scheme, rest)) = base.split_once("://") else {
        return seg.to_string();
    };
    let (host, path) = match rest.split_once('/') {
        Some((h, p)) => (h.to_string(), p.to_string()),
        None => (rest.to_string(), String::new()),
    };
    let dir = match path.rfind('/') {
        Some(i) => path[..=i].to_string(),
        None => "/".to_string(),
    };
    if seg.starts_with('/') {
        format!("{scheme}://{host}{seg}")
    } else {
        format!("{scheme}://{host}{dir}{seg}")
    }
}

fn parse_playlist(text: &str, base: &str) -> Result<Vec<String>> {
    let mut urls = Vec::new();
    let mut endlist = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("#ext-x-key") {
            if !lower.contains("method=none") {
                bail!("encrypted HLS is not supported");
            }
        } else if lower.starts_with("#ext-x-map") {
            if let Some(uri) = line.split("URI=\"").nth(1).and_then(|r| r.split('"').next()) {
                urls.push(join_url(base, uri));
            }
        } else if lower.starts_with("#ext-x-endlist") {
            endlist = true;
        } else if !line.starts_with('#') {
            urls.push(join_url(base, line));
        }
    }
    if urls.is_empty() {
        bail!("no media segments in playlist");
    }
    if !endlist {
        bail!("live HLS stream is not supported");
    }
    Ok(urls)
}

/// One in-flight or landed segment result.
enum Msg {
    Seg(u64, Vec<u8>),
    Warn(String),
    Err(String),
}

/// Download a VOD HLS playlist into `job.out`.
pub async fn download(
    job: Job,
    events: mpsc::Sender<Progress>,
    kill: watch::Receiver<bool>,
    content_type: Option<String>,
) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .build()?;

    // playlist text (follow redirects; the final URL anchors relative segs)
    let resp = client
        .get(&job.url)
        .headers(job.headers.clone())
        .send()
        .await
        .context("playlist request failed")?;
    if !resp.status().is_success() {
        bail!("HTTP {} for playlist {}", resp.status(), job.url);
    }
    let final_url = resp.url().to_string();
    let _ = content_type;
    let text = resp.text().await.context("playlist read failed")?;
    let urls = Arc::new(parse_playlist(&text, &final_url)?);
    let n = urls.len() as u64;

    let filename = job
        .out
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "download".into());

    // resume state (same playlist URL = same segment list)
    let spath = state_path(&job.out);
    let have: HashSet<u64> = if job.fresh {
        std::fs::remove_file(&spath).ok();
        HashSet::new()
    } else {
        serde_json::from_str::<HlsState>(&std::fs::read_to_string(&spath).unwrap_or_default())
            .map(|s| s.done.iter().copied().collect())
            .unwrap_or_default()
    };

    let workers = job.segments.clamp(1, 16) as usize;
    let next = Arc::new(AtomicU64::new(0));
    let total = Arc::new(AtomicU64::new(0));
    let (tx, mut rx) = mpsc::channel::<Msg>(workers * 2);
    let mut writer = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&job.out)
        .await
        .with_context(|| format!("open {}", job.out.display()))?;
    let size_now = writer.metadata().await?.len();
    total.fetch_add(size_now, Ordering::Relaxed);

    events
        .send(Progress::Started { url: job.url.clone(), filename, size: None, segments: workers })
        .await
        .ok();
    // detail snapshot: one pseudo-segment per media segment (0/1 bytes) so the
    // UI graph keeps coloring progress
    let snap = |done: &HashSet<u64>| -> Vec<SegmentInfo> {
        (0..n)
            .map(|i| SegmentInfo { start: i, end: i, done: u64::from(done.contains(&i)) })
            .collect()
    };
    events.send(Progress::Detail { segments: snap(&have) }).await.ok();

    for _ in 0..workers {
        let (tx, client, urls, next, total, have) =
            (tx.clone(), client.clone(), urls.clone(), next.clone(), total.clone(), have.clone());
        let (headers, retries, kill) =
            (job.headers.clone(), job.limits.retries, kill.clone());
        tokio::spawn(async move {
            loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= urls.len() as u64 {
                    return;
                }
                if have.contains(&i) {
                    continue; // already on disk from an earlier run
                }
                let mut err = String::new();
                let mut body = None;
                for att in 0..=retries {
                    if *kill.borrow() {
                        return;
                    }
                    match client.get(&urls[i as usize]).headers(headers.clone()).send().await {
                        Ok(r) if r.status().is_success() => match r.bytes().await {
                            Ok(b) => {
                                body = Some(b);
                                break;
                            }
                            Err(e) => err = e.to_string(),
                        },
                        Ok(r) => err = format!("HTTP {}", r.status()),
                        Err(e) => err = e.to_string(),
                    }
                    if tx.send(Msg::Warn(format!("segment {i}: {err}"))).await.is_err() {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(300 * (att + 1) as u64)).await;
                }
                let Some(b) = body else {
                    let _ = tx.send(Msg::Err(format!("segment {i} failed after retries: {err}"))).await;
                    return;
                };
                total.fetch_add(b.len() as u64, Ordering::Relaxed);
                if tx.send(Msg::Seg(i, b.to_vec())).await.is_err() {
                    return;
                }
            }
        });
    }
    drop(tx);

    let mut dl_done = have.clone();
    let mut pending: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
    // resume: the file already holds the contiguous prefix of `have`
    let mut write_idx: u64 = {
        let mut i = 0;
        while have.contains(&i) {
            i += 1;
        }
        i
    };
    let mut fail: Option<String> = None;
    let mut cancelled = false;

    loop {
        let msg = tokio::select! {
            m = rx.recv() => match m {
                Some(m) => m,
                None => break, // all workers done
            },
            _ = tokio::time::sleep(Duration::from_millis(200)) => {
                if *kill.borrow() {
                    cancelled = true;
                    break;
                }
                continue;
            }
        };
        match msg {
            Msg::Warn(m) => tracing::debug!("hls {m}"),
            Msg::Err(e) => {
                fail = Some(e);
                break;
            }
            Msg::Seg(i, data) => {
                pending.insert(i, data);
                // flush everything that is now contiguous
                while let Some(buf) = pending.remove(&write_idx) {
                    if let Err(e) = writer.write_all(&buf).await {
                        fail = Some(format!("write failed: {e}"));
                        break;
                    }
                    let _ = writer.flush().await;
                    dl_done.insert(write_idx);
                    write_idx += 1;
                    let bytes = buf.len() as u64;
                    let t = total.fetch_add(0, Ordering::Relaxed);
                    events
                        .send(Progress::Bytes { delta: bytes, total_done: t, size: None })
                        .await
                        .ok();
                    events.send(Progress::SegmentDone { index: (write_idx - 1) as usize }).await.ok();
                }
                if fail.is_some() {
                    break;
                }
                if write_idx < n {
                    events.send(Progress::Detail { segments: snap(&dl_done) }).await.ok();
                }
            }
        }
    }

    // workers may exit on kill before the writer notices
    if fail.is_none() && write_idx < n && *kill.borrow() {
        cancelled = true;
    }

    if fail.is_none() && write_idx >= n && !cancelled {
        std::fs::remove_file(&spath).ok();
        let size = total.load(Ordering::Relaxed);
        events.send(Progress::Completed { size, sha256: None }).await.ok();
        return Ok(());
    }

    // save resume state, then report
    let st = HlsState { url: final_url, done: sorted(&dl_done) };
    std::fs::write(&spath, serde_json::to_string(&st).unwrap_or_default()).ok();
    if cancelled {
        let t = total.load(Ordering::Relaxed);
        events.send(Progress::Cancelled { total_done: t }).await.ok();
        return Ok(());
    }
    let e = fail.unwrap_or_else(|| "HLS download incomplete".into());
    events.send(Progress::Failed { error: e.clone() }).await.ok();
    bail!("{e}")
}

fn sorted(set: &HashSet<u64>) -> Vec<u64> {
    let mut v: Vec<u64> = set.iter().copied().collect();
    v.sort_unstable();
    v
}
