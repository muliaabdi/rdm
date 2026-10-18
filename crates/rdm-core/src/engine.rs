//! Segmented downloader: probe, parallel range fetch, resume, speed limit.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_RANGE, ETAG, IF_MATCH, LAST_MODIFIED, RANGE};
use tokio::sync::{mpsc, watch, Mutex};

use crate::events::Progress;
use crate::state::{split, JobState};

pub const DEFAULT_MIN_SEGMENT: u64 = 1024 * 1024;
const RETRY_DELAY: Duration = Duration::from_millis(800);

#[derive(Debug, Clone)]
pub struct Limits {
    /// Global cap in bytes/second across all segments.
    pub max_speed: Option<u64>,
    /// Per-segment retry count before failing the job.
    pub retries: u32,
    /// Do not split files smaller than this.
    pub min_segment_size: u64,
    /// Compute sha256 on completion.
    pub sha256: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_speed: None, retries: 5, min_segment_size: DEFAULT_MIN_SEGMENT, sha256: false }
    }
}

#[derive(Debug, Clone)]
pub struct Job {
    pub url: String,
    pub out: PathBuf,
    pub headers: HeaderMap,
    pub segments: u64,
    pub limits: Limits,
}

/// Build a job for `url`, storing in `out_dir`, name from URL path.
pub fn job_from_url(url: &str, out_dir: Option<PathBuf>, segments: u64, limits: Limits) -> Job {
    let dir = out_dir.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));
    let name = filename_from_url(url);
    Job {
        url: url.to_string(),
        out: dir.join(name),
        headers: HeaderMap::new(),
        segments,
        limits,
    }
}

pub fn sanitize_filename(name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '(' | ')' | ' ') { c } else { '_' })
        .collect();
    let trimmed = clean.trim();
    if trimmed.is_empty() {
        "download".to_string()
    } else {
        trimmed.chars().take(180).collect()
    }
}

pub fn filename_from_url(url: &str) -> String {
    let path = url.split('#').next().unwrap_or(url);
    let path = path.split('?').next().unwrap_or(path);
    let name = path.rsplit('/').find(|s| !s.is_empty()).unwrap_or("download");
    sanitize_filename(name)
}

fn state_path(out: &std::path::Path) -> PathBuf {
    let mut s = out.as_os_str().to_os_string();
    s.push(".rdm-part.json");
    PathBuf::from(s)
}

/// Simple global token bucket, refilled lazily on access.
#[derive(Debug)]
struct Limiter {
    max: Option<u64>,
    inner: Mutex<(u64, Instant)>, // (budget, last refill)
}

impl Limiter {
    fn new(max: Option<u64>) -> Self {
        Self { max, inner: Mutex::new((0, Instant::now())) }
    }
    /// Wait until `n` bytes may be transferred.
    async fn acquire(&self, n: usize) {
        let Some(max) = self.max else { return };
        if max == 0 {
            return;
        }
        let mut guard = self.inner.lock().await;
        loop {
            let now = Instant::now();
            let elapsed = now.duration_since(guard.1).as_millis() as u64;
            if elapsed > 0 {
                guard.0 = guard.0.saturating_add(elapsed * max / 1000).min(max);
                guard.1 = now;
            }
            if guard.0 as usize >= n {
                guard.0 -= n as u64;
                return;
            }
            let need = n as u64 - guard.0;
            let wait_ms = (need * 1000 / max).clamp(1, 1000);
            drop(guard);
            tokio::time::sleep(Duration::from_millis(wait_ms)).await;
            guard = self.inner.lock().await;
        }
    }
}

struct Probe {
    size: Option<u64>,
    accept_ranges: bool,
    etag: Option<String>,
    last_modified: Option<String>,
    final_url: reqwest::Url,
}

async fn probe(client: &reqwest::Client, job: &Job) -> Result<Probe> {
    let req = client.get(&job.url).headers(job.headers.clone()).header(RANGE, "bytes=0-0");
    let resp = req.send().await.context("probe request failed")?;
    let status = resp.status();
    if !status.is_success() {
        bail!("HTTP {status} for {}", job.url);
    }
    let final_url = resp.url().clone();
    let etag = resp
        .headers()
        .get(ETAG)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let last_modified = resp
        .headers()
        .get(LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let mut size = None;
    let mut accept_ranges = false;
    if let Some(cr) = resp.headers().get(CONTENT_RANGE) {
        // bytes 0-0/12345
        if let Ok(s) = cr.to_str() {
            if let Some(total) = s.rsplit('/').next().and_then(|t| t.trim().parse::<u64>().ok()) {
                size = Some(total);
                accept_ranges = true;
            }
        }
    }
    if size.is_none() {
        if let Some(len) = resp.content_length() {
            size = Some(len);
        }
    }
    let _ = req; // drop body via resp below
    let _ = resp.bytes().await;
    Ok(Probe { size, accept_ranges, etag, last_modified, final_url })
}

enum Outcome {
    Completed,
    Cancelled,
}

struct Shared {
    state: Mutex<JobState>,
    limiter: Limiter,
    failed: Mutex<Option<String>>,
    attempts: Vec<AtomicU32>,
    last_detail: Mutex<Option<Instant>>,
}

/// Run one download job to completion, cancellation, or failure.
///
/// `kill` set to `true` cancels as soon as possible; state is saved and
/// `Progress::Cancelled` emitted, then `Ok(())` is returned.
pub async fn download(
    job: Job,
    events: mpsc::Sender<Progress>,
    kill: watch::Receiver<bool>,
) -> Result<()> {
    let limits = job.limits.clone();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .build()?;
    let probe = probe(&client, &job).await?;
    let filename = job
        .out
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "download".into());

    // Load resumable state when identity still matches.
    let spath = state_path(&job.out);
    let resumed = JobState::load(&spath)
        .ok()
        .flatten()
        .filter(|st| {
            st.url == probe.final_url.as_str()
                && st.etag == probe.etag
                && st.last_modified == probe.last_modified
        });

    let (mut st, resume) = match resumed {
        Some(st) if !st.complete() => (st, true),
        _ => {
            let segs = match probe.size {
                Some(sz)
                    if probe.accept_ranges
                        && sz >= limits.min_segment_size.max(1)
                        && job.segments > 1 =>
                {
                    split(sz, job.segments)
                }
                Some(sz) => vec![crate::state::Segment { start: 0, end: sz.saturating_sub(1), done: 0 }],
                None => vec![crate::state::Segment { start: 0, end: u64::MAX, done: 0 }],
            };
            (JobState {
                url: probe.final_url.to_string(),
                size: probe.size,
                etag: probe.etag.clone(),
                last_modified: probe.last_modified.clone(),
                segments: segs,
            }, false)
        }
    };

    events
        .send(Progress::Started {
            url: st.url.clone(),
            filename,
            size: st.size,
            segments: st.segments.len(),
        })
        .await
        .ok();

    if st.complete() {
        finish(job, st, &limits, &events).await?;
        return Ok(());
    }

    // Prepare output file: preallocate when size known; preserve on resume.
    if !resume {
        let f = std::fs::File::create(&job.out).context("create output")?;
        if let Some(sz) = st.size {
            f.set_len(sz).context("preallocate")?;
        }
    } else if !job.out.exists() {
        // state without file: restart content
        std::fs::File::create(&job.out).context("create output")?;
        st.segments.iter_mut().for_each(|s| s.done = 0);
    }

    let shared = Arc::new(Shared {
        limiter: Limiter::new(limits.max_speed),
        attempts: st.segments.iter().map(|_| AtomicU32::new(0)).collect(),
        state: Mutex::new(st.clone()),
        failed: Mutex::new(None),
        last_detail: Mutex::new(None),
    });

    let mut set = tokio::task::JoinSet::new();
    for idx in 0..st.segments.len() {
        if st.segments[idx].complete() || st.segments[idx].remaining() == 0 {
            continue;
        }
        let sh = shared.clone();
        let client = client.clone();
        let url = st.url.clone();
        let hdrs = job.headers.clone();
        let out = job.out.clone();
        let ev = events.clone();
        let mut kill = kill.clone();
        let retries = limits.retries;
        set.spawn(async move {
            segment_task(sh, client, url, hdrs, out, idx, ev, &mut kill, retries).await;
        });
    }

    let mut outcome = Outcome::Completed;
    loop {
        if shared.failed.lock().await.is_some() {
            outcome = Outcome::Cancelled;
            set.abort_all();
            break;
        }
        if *kill.borrow() {
            outcome = Outcome::Cancelled;
            set.abort_all();
            break;
        }
        if set.is_empty() {
            break;
        }
        let _ = set.join_next().await;
    }

    let stg = shared.state.lock().await;
    let st = stg.clone();
    if matches!(outcome, Outcome::Cancelled) {
        st.save(&spath).ok();
        events
            .send(Progress::Cancelled { total_done: st.total_done() })
            .await
            .ok();
        if let Some(err) = shared.failed.lock().await.take() {
            bail!(err);
        }
        return Ok(());
    }
    if let Some(err) = shared.failed.lock().await.take() {
        bail!(err);
    }
    if !st.complete() {
        bail!("engine ended before completion: {} / {}", st.total_done(), st.size.unwrap_or(0));
    }
    finish(job, st, &limits, &events).await
}

async fn finish(job: Job, st: JobState, limits: &Limits, events: &mpsc::Sender<Progress>) -> Result<()> {
    let size = st.total_done();
    // flush directory entry best effort
    if let Some(f) = std::fs::File::open(&job.out).ok() {
        f.sync_all().ok();
    }
    std::fs::remove_file(state_path(&job.out)).ok();
    let sha = if limits.sha256 {
        sha256_file(&job.out).await.ok()
    } else {
        None
    };
    events.send(Progress::Completed { size, sha256: sha }).await.ok();
    Ok(())
}

async fn sha256_file(path: &std::path::Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut f = std::fs::File::open(&path)?;
        let mut hasher = Sha256::new();
        std::io::copy(&mut f, &mut hasher)?;
        Ok(format!("{:x}", hasher.finalize()))
    })
    .await?
}

#[allow(clippy::too_many_arguments)]
async fn segment_task(
    sh: Arc<Shared>,
    client: reqwest::Client,
    url: String,
    hdrs: HeaderMap,
    out: PathBuf,
    idx: usize,
    ev: mpsc::Sender<Progress>,
    kill: &mut watch::Receiver<bool>,
    retries: u32,
) {
    let mut file = match std::fs::OpenOptions::new().write(true).open(&out) {
        Ok(f) => f,
        Err(e) => {
            *sh.failed.lock().await = Some(format!("open output: {e}"));
            return;
        }
    };
    loop {
        if *kill.borrow() {
            return;
        }
        if sh.failed.lock().await.is_some() {
            return;
        }
        let (offset, end) = {
            let st = sh.state.lock().await;
            let s = &st.segments[idx];
            (s.offset(), s.end)
        };
        if offset > end {
            return; // segment finished
        }
        let mut req = client.get(&url).headers(hdrs.clone());
        if end != u64::MAX {
            req = req.header(RANGE, format!("bytes={offset}-{end}"));
        } else if offset > 0 {
            req = req.header(RANGE, format!("bytes={offset}-"));
        }
        if let Some(etag) = sh.state.lock().await.etag.clone() {
            if let Ok(v) = HeaderValue::from_str(&etag) {
                req = req.header(IF_MATCH, v);
            }
        }
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                if !retry(sh.clone(), idx, retries, format!("request: {e}"), &ev).await {
                    return;
                }
                continue;
            }
        };
        if !resp.status().is_success() {
            let msg = format!("HTTP {} @ seg {idx}", resp.status());
            if !retry(sh.clone(), idx, retries, msg, &ev).await {
                return;
            }
            continue;
        }
        let mut stream = resp.bytes_stream();
        let mut pos = offset;
        loop {
            if *kill.borrow() || sh.failed.lock().await.is_some() {
                return;
            }
            let chunk = match stream.next().await {
                Some(Ok(c)) => c,
                Some(Err(_)) => break, // retry outer loop from current pos
                None => break,
            };
            sh.limiter.acquire(chunk.len()).await;
            if let Err(e) = write_at(&mut file, pos, &chunk) {
                *sh.failed.lock().await = Some(format!("write: {e}"));
                return;
            }
            pos += chunk.len() as u64;
            let total_done = {
                let mut st = sh.state.lock().await;
                st.segments[idx].done = pos - st.segments[idx].start;
                let t = st.total_done();
                t
            };
            if idx == 0 {
                let due = { sh.last_detail.lock().await.map(|i| i.elapsed() >= Duration::from_millis(400)).unwrap_or(true) };
                if due {
                    let snap = {
                        let st = sh.state.lock().await;
                        st.segments
                            .iter()
                            .map(|s| crate::events::SegmentInfo { start: s.start, end: s.end, done: s.done })
                            .collect::<Vec<_>>()
                    };
                    *sh.last_detail.lock().await = Some(Instant::now());
                    ev.send(Progress::Detail { segments: snap }).await.ok();
                }
            }
            ev.send(Progress::Bytes { delta: chunk.len() as u64, total_done, size: sh.state.lock().await.size })
                .await
                .ok();
        }
        // Stream ended: verify segment coverage.
        let (complete, remaining) = {
            let st = sh.state.lock().await;
            let s = &st.segments[idx];
            (s.remaining() == 0 || s.end == u64::MAX, s.remaining())
        };
        if complete {
            let mut st = sh.state.lock().await;
            if st.segments[idx].end == u64::MAX && st.segments[idx].done > 0 {
                // unknown total size: finalize length from what we got
                st.size = Some(st.total_done());
                st.segments[idx].end = st.segments[idx].start + st.segments[idx].done - 1;
            }
            ev.send(Progress::SegmentDone { index: idx }).await.ok();
            return;
        }
        if remaining > 0 && stream.next().await.is_none() {
            let msg = format!("seg {idx} short read, {remaining}B missing");
            if !retry(sh.clone(), idx, retries, msg, &ev).await {
                return;
            }
        }
    }
}

async fn retry(sh: Arc<Shared>, idx: usize, retries: u32, msg: String, ev: &mpsc::Sender<Progress>) -> bool {
    let n = sh.attempts[idx].fetch_add(1, Ordering::SeqCst) + 1;
    if n > retries {
        *sh.failed.lock().await = Some(msg);
        return false;
    }
    ev.send(Progress::Retrying { index: idx, attempt: n, error: msg }).await.ok();
    tokio::time::sleep(RETRY_DELAY * n as u32).await;
    true
}

fn write_at(f: &mut std::fs::File, pos: u64, buf: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    f.write_all_at(buf, pos)
}
