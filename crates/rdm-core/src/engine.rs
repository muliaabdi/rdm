//! Segmented downloader: probe, parallel range fetch, resume, speed limit.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use reqwest::header::{
    HeaderMap, HeaderValue, CONTENT_RANGE, ETAG, IF_MATCH, LAST_MODIFIED, RANGE,
};
use tokio::sync::{mpsc, watch, Mutex};

use crate::events::Progress;
use crate::state::{split, JobState};

pub const DEFAULT_MIN_SEGMENT: u64 = 1024 * 1024;
const RETRY_DELAY: Duration = Duration::from_millis(800);
/// A task only steals when at least this much is left, so split halves stay
/// meaningful (mirrors the dynamic segmentation floor of classic download managers).
pub const MIN_STEAL: u64 = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Limits {
    /// Global cap in bytes/second across all segments.
    pub max_speed: Option<u64>,
    /// Shared process-wide bucket (daemon passes one so the cap covers every
    /// concurrent job); when `None` a private bucket is created per job.
    pub limiter: Option<Arc<Limiter>>,
    /// Per-segment retry count before failing the job.
    pub retries: u32,
    /// Do not split files smaller than this.
    pub min_segment_size: u64,
    /// Compute sha256 on completion.
    pub sha256: bool,
    /// Optional DNS-over-HTTPS resolver to bypass ISP poisoning.
    pub dns_resolver: Option<Arc<crate::dns::DohResolver>>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_speed: None,
            limiter: None,
            retries: 5,
            min_segment_size: DEFAULT_MIN_SEGMENT,
            sha256: false,
            dns_resolver: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Job {
    pub url: String,
    pub out: PathBuf,
    pub headers: HeaderMap,
    pub segments: u64,
    pub limits: Limits,
    /// Restart mode: discard any on-disk part state and download from zero.
    pub fresh: bool,
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
        fresh: false,
    }
}

/// Category bucket for a filename, used to auto-sort downloads into the
/// per-category folders configured in the UI.
pub fn category_of(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    const GROUPS: [(&str, &[&str]); 7] = [
        (
            "images",
            &[
                "jpg", "jpeg", "png", "gif", "webp", "bmp", "svg", "ico", "tiff", "avif", "heic",
            ],
        ),
        (
            "videos",
            &[
                "mp4", "mkv", "avi", "mov", "webm", "flv", "wmv", "m4v", "mpg", "mpeg", "ts", "3gp",
            ],
        ),
        (
            "audio",
            &[
                "mp3", "wav", "flac", "aac", "ogg", "opus", "m4a", "wma", "mid",
            ],
        ),
        (
            "documents",
            &[
                "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "rtf", "odt", "ods",
                "odp", "csv", "epub", "mobi",
            ],
        ),
        (
            "apps",
            &[
                "exe", "msi", "apk", "deb", "rpm", "appimage", "flatpak", "snap", "dmg", "pkg",
                "jar",
            ],
        ),
        (
            "archives",
            &[
                "zip", "rar", "7z", "tar", "gz", "bz2", "xz", "zst", "lz", "lzma", "cab", "iso",
                "img",
            ],
        ),
        (
            "others",
            &["torrent", "bin", "dat", "m3u8", "mpd", "nfo", "srt"],
        ),
    ];
    for (cat, exts) in GROUPS {
        if exts.contains(&ext.as_str()) {
            return cat;
        }
    }
    "others"
}

/// Extension from remote metadata (yt-dlp etc): strict allow-list so a
/// hostile value can never smuggle path separators or traversal into an
/// output path. Empty/blank falls back to "bin".
pub fn sanitize_ext(e: &str) -> String {
    let s: String = e
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect();
    if s.is_empty() {
        "bin".to_string()
    } else {
        s.to_ascii_lowercase()
    }
}

pub fn sanitize_filename(name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '(' | ')' | ' ') {
                c
            } else {
                '_'
            }
        })
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
    let name = path
        .rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or("download");
    sanitize_filename(name)
}

fn state_path(out: &std::path::Path) -> PathBuf {
    let mut s = out.as_os_str().to_os_string();
    s.push(".rdm-part.json");
    PathBuf::from(s)
}

/// Token bucket shared by every transfer (all jobs, all segments), so a
/// configured cap is a true global cap. `0` means unlimited.
#[derive(Debug)]
pub struct Limiter {
    max: std::sync::atomic::AtomicU64, // bytes/second, 0 = unlimited
    inner: Mutex<(u64, Instant)>,      // (budget, last refill)
}

impl Limiter {
    pub fn new(max: Option<u64>) -> Self {
        Self {
            max: std::sync::atomic::AtomicU64::new(max.unwrap_or(0)),
            inner: Mutex::new((0, Instant::now())),
        }
    }
    /// Change the cap at runtime (settings dialog). `None`/`Some(0)` = unlimited.
    pub fn set_max(&self, max: Option<u64>) {
        self.max
            .store(max.unwrap_or(0), std::sync::atomic::Ordering::Relaxed);
    }
    /// Wait until `n` bytes may be transferred.
    pub async fn acquire(&self, n: usize) {
        // a request larger than the per-second cap is served in cap-sized
        // slices, otherwise the bucket could never fill up to `n`
        let mut left = n as u64;
        while left > 0 {
            let max = self.max.load(std::sync::atomic::Ordering::Relaxed);
            if max == 0 {
                return;
            }
            let take = left.min(max);
            self.take(take, max).await;
            left -= take;
        }
    }

    async fn take(&self, n: u64, max: u64) {
        let mut guard = self.inner.lock().await;
        loop {
            let now = Instant::now();
            let elapsed = now.duration_since(guard.1).as_millis() as u64;
            if elapsed > 0 {
                guard.0 = guard.0.saturating_add(elapsed * max / 1000).min(max);
                guard.1 = now;
            }
            if guard.0 >= n {
                guard.0 -= n;
                return;
            }
            let need = n - guard.0;
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
    content_type: Option<String>,
    cd_filename: Option<String>,
    playlist: bool,
}

async fn probe(client: &reqwest::Client, job: &Job) -> Result<Probe> {
    let req = client
        .get(&job.url)
        .headers(job.headers.clone())
        .header(RANGE, "bytes=0-0");
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
            if let Some(total) = s
                .rsplit('/')
                .next()
                .and_then(|t| t.trim().parse::<u64>().ok())
            {
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
    let headers_ct = resp.headers().get(reqwest::header::CONTENT_TYPE).cloned();
    let headers_cd = resp
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let _ = resp.bytes().await;
    let content_type = headers_ct.and_then(|v| v.to_str().ok().map(str::to_string));
    let cd_filename = headers_cd.as_deref().and_then(cd_name);
    let playlist = crate::hls::looks_like_playlist(content_type.as_deref(), final_url.as_str());
    Ok(Probe {
        size,
        accept_ranges,
        etag,
        last_modified,
        final_url,
        content_type,
        cd_filename,
        playlist,
    })
}

/// Minimal percent-decoder for `filename*=UTF-8''...` values.
fn pct_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Extract a filename from a Content-Disposition header value.
/// RFC 5987 `filename*` wins (true UTF-8), plain `filename` is the fallback
/// (its escapes are latin-1 at best; browsers still send raw UTF-8 there).
fn cd_name(cd: &str) -> Option<String> {
    for part in cd.split(';').map(str::trim) {
        if part.get(..10).is_some_and(|p| p.eq_ignore_ascii_case("filename*=")) {
            let v = part[10..].trim();
            // UTF-8''name.pdf — skip the charset prefix when present
            let name = v.split("''").nth(1).unwrap_or(v);
            let name = name.trim_matches('"').trim();
            if !name.is_empty() {
                return Some(pct_decode(name));
            }
        }
    }
    for part in cd.split(';').map(str::trim) {
        if part.get(..9).is_some_and(|p| p.eq_ignore_ascii_case("filename=")) {
            let v = part[9..].trim();
            let v = v.trim_matches('"').trim();
            if !v.is_empty() && v != "\\" && v != "." && v != ".." {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Common extension for a MIME type; used only to rescue extension-less
/// names (".../u/0/", ".../download") so the file opens with the right app.
fn ext_for_content_type(ct: Option<&str>) -> Option<&'static str> {
    let base = ct?.split(';').next()?.trim().to_ascii_lowercase();
    Some(match base.as_str() {
        "application/pdf" => "pdf",
        "application/zip" => "zip",
        "application/x-rar-compressed" | "application/vnd.rar" => "rar",
        "application/x-7z-compressed" => "7z",
        "application/x-iso9660-image" => "iso",
        "application/json" => "json",
        "application/epub+zip" => "epub",
        "application/msword" => "doc",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
        "application/vnd.ms-excel" => "xls",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
        "application/vnd.ms-powerpoint" => "ppt",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => "pptx",
        "application/octet-stream" => return None, // could be anything
        "text/plain" => "txt",
        "text/csv" => "csv",
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "image/avif" => "avif",
        "audio/mpeg" => "mp3",
        "audio/mp4" => "m4a",
        "audio/ogg" => "ogg",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/flac" => "flac",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "video/x-matroska" => "mkv",
        "video/quicktime" => "mov",
        "video/mpeg" => "mpeg",
        _ => return None,
    })
}

/// Decide a better output name when the URL-derived one carries no
/// information (no extension, e.g. Gmail's `/attachment/u/0/` -> "0").
/// `true` only when the current name has no extension and a real one is
/// derivable — names that already have an extension are never touched.
fn better_name(out: &std::path::Path, probe: &Probe) -> Option<String> {
    let cur = out.file_name()?.to_string_lossy().to_string();
    if cur.contains('.') && cur.rsplit('.').next().is_some_and(|e| !e.is_empty()) {
        return None; // already has an extension
    }
    if let Some(cd) = probe.cd_filename.as_deref() {
        let clean = sanitize_filename(cd);
        if clean != "download" && clean.contains('.') {
            return Some(clean);
        }
    }
    let ext = ext_for_content_type(probe.content_type.as_deref())?;
    // keep the URL stem when it is not pure noise ("0", "download", "bin")
    let stem = cur.trim();
    let stem = if stem.is_empty()
        || stem == "download"
        || stem == "bin"
        || stem.chars().all(|c| c.is_ascii_digit())
    {
        "download"
    } else {
        stem
    };
    Some(format!("{stem}.{ext}"))
}

enum Outcome {
    Completed,
    Cancelled,
}

struct Shared {
    state: Mutex<JobState>,
    limiter: Arc<Limiter>,
    failed: Mutex<Option<String>>,
    attempts: Mutex<Vec<u32>>,
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
    // Fast path: if the URL clearly points to an HLS/DASH playlist (.m3u8 / .mpd),
    // bypass the reqwest probe entirely. Strict streaming CDNs (e.g. surrit.com,
    // dramiyos/acek) reject rustls TLS fingerprints with HTTP 403 Forbidden,
    // while hls::download uses Chrome BoringSSL emulation (wreq).
    let path = job.url.split(['?', '#']).next().unwrap_or(&job.url);
    if path.ends_with(".m3u8") || path.ends_with(".mpd") {
        let mut job = job;
        if job
            .out
            .extension()
            .map(|e| e.to_string_lossy().eq_ignore_ascii_case("m3u8"))
            .unwrap_or(false)
        {
            job.out.set_extension("ts");
        }
        return crate::hls::download(job, events, kill, Some("application/vnd.apple.mpegurl".to_string())).await;
    }

    let limits = job.limits.clone();
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10));
    if let Some(ref dns) = limits.dns_resolver {
        builder = builder.dns_resolver(dns.clone());
    }
    let client = builder.build()?;
    let probe = probe(&client, &job).await?;

    // generic HLS: hand the job to the m3u8 path (output name x.m3u8 -> x.ts)
    if probe.playlist {
        let mut job = job;
        if job
            .out
            .extension()
            .map(|e| e.to_string_lossy().eq_ignore_ascii_case("m3u8"))
            .unwrap_or(false)
        {
            job.out.set_extension("ts");
        }
        return crate::hls::download(job, events, kill, probe.content_type).await;
    }

    // Rescue meaningless URL-derived names (no extension, e.g. Gmail's
    // "/attachment/u/0/" -> "0") using Content-Disposition or Content-Type
    // from the probe response. Must run before the part-state path is
    // derived from job.out, so state and output stay a matched pair.
    let mut job = job;
    if let Some(better) = better_name(&job.out, &probe) {
        tracing::info!("rename {} -> {better} (content-disposition/type)", job.out.display());
        job.out = job.out.with_file_name(better);
    }

    let filename = job
        .out
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "download".into());

    // Load resumable state. Identity is kept when validators match; when the
    // server sends neither ETag nor Last-Modified (e.g. signed CDN URLs such
    // as googlevideo), a matching size is enough. The URL itself may differ:
    // redirects and signed URLs expire, so the fresh probe URL replaces it.
    let spath = state_path(&job.out);
    let resumed = if job.fresh {
        std::fs::remove_file(&spath).ok();
        None
    } else {
        JobState::load(&spath)
            .ok()
            .flatten()
            .filter(|st| !st.complete())
            .filter(|st| {
                if st.etag.is_some() || st.last_modified.is_some() {
                    st.etag == probe.etag && st.last_modified == probe.last_modified
                } else {
                    st.size.is_some() && st.size == probe.size
                }
            })
    };

    let (mut st, resume) = match resumed {
        Some(mut st) => {
            st.url = probe.final_url.to_string();
            (st, true)
        }
        _ => {
            let segs = match probe.size {
                Some(sz)
                    if probe.accept_ranges
                        && sz >= limits.min_segment_size.max(1)
                        && job.segments > 1 =>
                {
                    split(sz, job.segments)
                }
                Some(sz) => vec![crate::state::Segment {
                    start: 0,
                    end: sz.saturating_sub(1),
                    done: 0,
                }],
                None => vec![crate::state::Segment {
                    start: 0,
                    end: u64::MAX,
                    done: 0,
                }],
            };
            (
                JobState {
                    url: probe.final_url.to_string(),
                    size: probe.size,
                    etag: probe.etag.clone(),
                    last_modified: probe.last_modified.clone(),
                    segments: segs,
                },
                false,
            )
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
        limiter: limits
            .limiter
            .clone()
            .unwrap_or_else(|| Arc::new(Limiter::new(limits.max_speed))),
        attempts: Mutex::new(vec![0; st.segments.len()]),
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
    let mut kill_watch = kill.clone();
    loop {
        if shared.failed.lock().await.is_some() {
            outcome = Outcome::Cancelled;
            set.abort_all();
            break;
        }
        if *kill_watch.borrow() {
            outcome = Outcome::Cancelled;
            set.abort_all();
            break;
        }
        if set.is_empty() {
            break;
        }
        // react to kill immediately instead of waiting for the next segment
        // task to finish - pause must feel instant even with long segments
        tokio::select! {
            changed = kill_watch.changed() => {
                if changed.is_ok() && *kill_watch.borrow() {
                    outcome = Outcome::Cancelled;
                    set.abort_all();
                    break;
                }
            }
            _ = set.join_next() => {}
        }
    }

    let stg = shared.state.lock().await;
    let st = stg.clone();
    if matches!(outcome, Outcome::Cancelled) {
        st.save(&spath).ok();
        events
            .send(Progress::Cancelled {
                total_done: st.total_done(),
            })
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
        bail!(
            "engine ended before completion: {} / {}",
            st.total_done(),
            st.size.unwrap_or(0)
        );
    }
    finish(job, st, &limits, &events).await
}

async fn finish(
    job: Job,
    st: JobState,
    limits: &Limits,
    events: &mpsc::Sender<Progress>,
) -> Result<()> {
    let size = st.total_done();
    // flush directory entry best effort
    if let Ok(f) = std::fs::File::open(&job.out) {
        f.sync_all().ok();
    }
    std::fs::remove_file(state_path(&job.out)).ok();
    let sha = if limits.sha256 {
        sha256_file(&job.out).await.ok()
    } else {
        None
    };
    events
        .send(Progress::Completed { size, sha256: sha })
        .await
        .ok();
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
    start_idx: usize,
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
    let mut idx = start_idx;
    loop {
        // fetch the assigned segment; on completion try to steal more work
        // from the largest remaining segment (classic-style dynamic splitting)
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
                break; // segment finished -> steal phase
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
                // the segment may have been shrunk by a stealing task while
                // this request was in flight; never write past `end`
                let (seg_start, seg_end) = {
                    let st = sh.state.lock().await;
                    (st.segments[idx].start, st.segments[idx].end)
                };
                let room = if seg_end == u64::MAX {
                    usize::MAX
                } else {
                    ((seg_end + 1).saturating_sub(pos)) as usize
                };
                if room == 0 {
                    break; // stolen mid-flight; the thief owns the rest now
                }
                let take = room.min(chunk.len());
                if let Err(e) = write_at(&mut file, pos, &chunk[..take]) {
                    *sh.failed.lock().await = Some(format!("write: {e}"));
                    return;
                }
                pos += take as u64;
                let total_done = {
                    let mut st = sh.state.lock().await;
                    st.segments[idx].done = pos - seg_start;

                    st.total_done()
                };
                let due = {
                    sh.last_detail
                        .lock()
                        .await
                        .map(|i| i.elapsed() >= Duration::from_millis(400))
                        .unwrap_or(true)
                };
                if due {
                    let snap = {
                        let st = sh.state.lock().await;
                        st.segments
                            .iter()
                            .map(|s| crate::events::SegmentInfo {
                                start: s.start,
                                end: s.end,
                                done: s.done,
                            })
                            .collect::<Vec<_>>()
                    };
                    *sh.last_detail.lock().await = Some(Instant::now());
                    ev.send(Progress::Detail { segments: snap }).await.ok();
                }
                let size_now = { sh.state.lock().await.size };
                ev.send(Progress::Bytes {
                    delta: take as u64,
                    total_done,
                    size: size_now,
                })
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
                break; // -> steal phase
            }
            {
                let msg = format!("seg {idx} short read, {remaining}B missing");
                if !retry(sh.clone(), idx, retries, msg, &ev).await {
                    return;
                }
            }
        }
        match steal(&sh, idx).await {
            Some(next) => idx = next,
            None => return, // nothing left worth stealing; this connection is done
        }
    }
}

/// Classic download-manager work stealing: take the upper half of the *remaining* bytes of
/// the largest incomplete segment and append it as a fresh segment.
async fn steal(sh: &Shared, from: usize) -> Option<usize> {
    let mut st = sh.state.lock().await;
    let best = st
        .segments
        .iter()
        .enumerate()
        .filter(|(i, s)| *i != from && s.remaining() > 0)
        .map(|(i, s)| (i, s.remaining()))
        .max_by_key(|&(_, r)| r)?;
    let (j, _rem) = best;
    let (new_end, seg) = split_remaining(&st.segments[j], MIN_STEAL)?;
    st.segments[j].end = new_end;
    st.segments.push(seg);
    let ni = st.segments.len() - 1;
    let mut attempts = sh.attempts.lock().await;
    if attempts.len() < ni + 1 {
        attempts.resize(ni + 1, 0);
    }
    Some(ni)
}

/// Split what is left of `s` in half; returns (new end for `s`, stolen segment).
fn split_remaining(
    s: &crate::state::Segment,
    min_stolen: u64,
) -> Option<(u64, crate::state::Segment)> {
    let rem = s.remaining();
    if rem < min_stolen.saturating_mul(2) {
        return None; // halves would be too small to matter
    }
    let off = s.offset();
    let mid = off + rem / 2;
    Some((
        mid - 1,
        crate::state::Segment {
            start: mid,
            end: s.end,
            done: 0,
        },
    ))
}

async fn retry(
    sh: Arc<Shared>,
    idx: usize,
    retries: u32,
    msg: String,
    ev: &mpsc::Sender<Progress>,
) -> bool {
    let n = {
        let mut v = sh.attempts.lock().await;
        if v.len() <= idx {
            v.resize(idx + 1, 0);
        }
        v[idx] += 1;
        v[idx]
    };
    if n > retries {
        *sh.failed.lock().await = Some(msg);
        return false;
    }
    ev.send(Progress::Retrying {
        index: idx,
        attempt: n,
        error: msg,
    })
    .await
    .ok();
    tokio::time::sleep(RETRY_DELAY * n as u32).await;
    true
}

fn write_at(f: &mut std::fs::File, pos: u64, buf: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    f.write_all_at(buf, pos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Segment;

    #[test]
    fn category_of_maps_common_extensions() {
        assert_eq!(category_of("a.pdf"), "documents");
        assert_eq!(category_of("A.PDF"), "documents");
        assert_eq!(category_of("x.mp4"), "videos");
        assert_eq!(category_of("x.mp3"), "audio");
        assert_eq!(category_of("x.png"), "images");
        assert_eq!(category_of("x.zip"), "archives");
        assert_eq!(category_of("x.apk"), "apps");
        assert_eq!(category_of("x.torrent"), "others");
        assert_eq!(category_of("noext"), "others");
    }

    #[test]
    fn cd_name_prefers_rfc5987_then_plain() {
        assert_eq!(
            cd_name("attachment; filename=\"invoice.pdf\""),
            Some("invoice.pdf".into())
        );
        assert_eq!(
            cd_name("attachment; filename*=UTF-8''laporan%20q3.pdf"),
            Some("laporan q3.pdf".into())
        );
        // RFC 5987 wins over plain
        assert_eq!(
            cd_name("attachment; filename=\"a.pdf\"; filename*=UTF-8''b%2Epdf"),
            Some("b.pdf".into())
        );
        assert_eq!(cd_name("inline"), None);
        assert_eq!(cd_name("attachment; filename=\"\""), None);
    }

    #[test]
    fn better_name_rescues_extension_less_names_only() {
        let mk = |cd: Option<&str>, ct: Option<&str>| Probe {
            size: None,
            accept_ranges: false,
            etag: None,
            last_modified: None,
            final_url: reqwest::Url::parse("https://x.example/a").unwrap(),
            content_type: ct.map(str::to_string),
            cd_filename: cd.map(str::to_string),
            playlist: false,
        };
        // Gmail shape: "0" + application/pdf -> Content-Disposition wins
        let p = mk(Some("Kontrak 2026.pdf"), Some("application/pdf"));
        assert_eq!(better_name(std::path::Path::new("/tmp/0"), &p).as_deref(), Some("Kontrak 2026.pdf"));
        // no CD: extension from Content-Type, noisy digit stem replaced
        let p = mk(None, Some("application/pdf"));
        assert_eq!(better_name(std::path::Path::new("/tmp/0"), &p).as_deref(), Some("download.pdf"));
        // meaningful stem keeps the stem
        let p = mk(None, Some("application/pdf"));
        assert_eq!(better_name(std::path::Path::new("/tmp/laporan"), &p).as_deref(), Some("laporan.pdf"));
        // names that already carry an extension are never touched
        let p = mk(Some("evil.exe"), Some("application/pdf"));
        assert_eq!(better_name(std::path::Path::new("/tmp/0.pdf"), &p), None);
        // unknown content-type and no CD -> leave alone
        let p = mk(None, Some("application/octet-stream"));
        assert_eq!(better_name(std::path::Path::new("/tmp/0"), &p), None);
    }

    #[test]
    fn sanitize_ext_never_carries_path_chars() {
        assert_eq!(sanitize_ext("mp4"), "mp4");
        assert_eq!(sanitize_ext("MP4"), "mp4");
        assert_eq!(sanitize_ext("m2ts"), "m2ts");
        assert_eq!(sanitize_ext("../../etc/passwd"), "etcpassw");
        assert_eq!(sanitize_ext("a/b\\c"), "abc");
        assert_eq!(sanitize_ext("webm; malicious"), "webmmali");
        assert_eq!(sanitize_ext(""), "bin");
        assert_eq!(sanitize_ext(".."), "bin");
    }

    fn seg(start: u64, end: u64, done: u64) -> Segment {
        Segment { start, end, done }
    }

    #[tokio::test]
    async fn limiter_paces_and_can_be_lifted_at_runtime() {
        let l = Limiter::new(Some(200_000));
        let t = Instant::now();
        l.acquire(400_000).await; // budget starts at one second worth
        assert!(t.elapsed() >= Duration::from_millis(900));
        l.set_max(None);
        let t = Instant::now();
        l.acquire(10_000_000).await;
        assert!(t.elapsed() < Duration::from_millis(100));
    }

    #[test]
    fn split_remaining_covers_exactly() {
        // 10 MiB remaining, 3 MiB already done
        let s = seg(0, 10 * 1024 * 1024 - 1, 3 * 1024 * 1024);
        let (new_end, stolen) = split_remaining(&s, MIN_STEAL).expect("should split");
        let owner = seg(s.start, new_end, s.done);
        assert_eq!(owner.remaining() + stolen.len(), s.remaining());
        assert_eq!(stolen.start - 1, new_end);
        assert_eq!(stolen.end, s.end);
        assert_eq!(stolen.done, 0);
        // halves are roughly equal and never below the minimum
        assert!(owner.remaining() >= MIN_STEAL && stolen.remaining() >= MIN_STEAL);
    }

    #[test]
    fn split_remaining_refuses_tiny() {
        let s = seg(0, 2 * MIN_STEAL, 0); // exactly 2 MiB + 1 B: halves >= 1 MiB, ok
        assert!(split_remaining(&s, MIN_STEAL).is_some());
        let tiny = seg(0, 2 * MIN_STEAL - 2, 0); // 2 MiB - 1 B remaining
        assert!(split_remaining(&tiny, MIN_STEAL).is_none());
        let done = seg(0, 2 * MIN_STEAL + 10, 2 * MIN_STEAL); // 11 bytes left
        assert!(split_remaining(&done, MIN_STEAL).is_none());
    }
}
