//! Generic HLS (m3u8) downloader.
//!
//! Supported: VOD and live playlists, AES-128 encryption (explicit IV or
//! media-sequence derived), fMP4 init segments (`#EXT-X-MAP`) — an init is
//! written each time the active map changes (ad-inserted / multi-period
//! streams). `BYTERANGE` segments and `SAMPLE-AES` are rejected with a clear
//! error.
//!
//! VOD resume state: `{out}.rdm-hls.json` lists finished *item* indices
//! (items = init segments + media segments, in stream order).
//! Live resume: state lists the URLs already written; resuming continues
//! with whatever the (sliding) window still serves — the middle of a paused
//! live stream cannot be recovered.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, watch};

use crate::engine::Job;
use crate::events::{Progress, SegmentInfo};

use cbc::cipher::{BlockDecryptMut, KeyIvInit};

type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeySpec {
    uri: String,
    iv: Option<String>, // hex, 0x-prefixed or bare
}

#[derive(Debug, Clone)]
struct Key {
    spec: KeySpec,
}

impl Key {
    fn iv_for(&self, seq: u64) -> Result<[u8; 16]> {
        if let Some(h) = &self.spec.iv {
            let hex = h.trim_start_matches("0x");
            let mut b = [0u8; 16];
            if hex.len() != 32 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                bail!("bad EXT-X-KEY IV");
            }
            for i in 0..16 {
                b[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).context("bad IV hex")?;
            }
            Ok(b)
        } else {
            // RFC 8216: no IV -> big-endian media sequence number
            let mut b = [0u8; 16];
            b[8..].copy_from_slice(&seq.to_be_bytes());
            Ok(b)
        }
    }
}

#[derive(Debug, Clone)]
enum Item {
    Init {
        url: String,
    },
    Media {
        url: String,
        key: Option<Arc<Key>>,
        seq: u64,
    },
}

impl Item {
    fn url(&self) -> &str {
        match self {
            Item::Init { url } => url,
            Item::Media { url, .. } => url,
        }
    }
}

struct Plan {
    target_dur: f64,
    ended: bool,
    items: Vec<Item>,
    /// master playlist: URL of the highest-bandwidth variant to descend into
    variant: Option<String>,
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

/// Resolve a playlist line against the playlist URL (absolute,
/// root-relative, or plain relative).
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

fn attr<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let rest = line.split(&format!("{name}=")).nth(1)?;
    if let Some(r) = rest.strip_prefix('"') {
        Some(r.split('"').next().unwrap_or(r))
    } else {
        Some(rest.split(',').next().unwrap_or(rest))
    }
}

fn parse_iv(s: &str) -> Option<String> {
    s.split("IV=")
        .nth(1)
        .map(|v| v.trim().trim_matches('"').to_string())
}

fn parse_playlist(text: &str, base: &str) -> Result<Plan> {
    let mut items: Vec<Item> = Vec::new();
    let mut ended = false;
    let mut target_dur = 4.0;
    let mut media_seq: u64 = 0;
    let mut media_n: u64 = 0;
    let mut cur_key: Option<Arc<Key>> = None;
    let mut cur_map: Option<String> = None;
    let mut map_emitted: Option<String> = None;
    // master playlist state: after #EXT-X-STREAM-INF the next bare line is
    // a variant URL, not a media segment
    let mut master = false;
    let mut pending_bw: Option<u64> = None;
    let mut best: Option<(u64, String)> = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if !line.starts_with('#') {
            if pending_bw.is_some() {
                let url = join_url(base, line);
                let bw = pending_bw.take().unwrap_or(0);
                if best.as_ref().map(|(b, _)| bw > *b).unwrap_or(true) {
                    best = Some((bw, url));
                }
                continue;
            }
            let url = join_url(base, line);
            // the active map (if any, and if not already emitted) opens a
            // new stream group: write the init before this media segment
            if let Some(map) = cur_map.clone() {
                if map_emitted.as_deref() != Some(map.as_str()) {
                    items.push(Item::Init { url: map.clone() });
                    map_emitted = Some(map);
                }
            }
            items.push(Item::Media {
                url,
                key: cur_key.clone(),
                seq: media_seq + media_n,
            });
            media_n += 1;
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("#ext-x-stream-inf") {
            master = true;
            pending_bw = attr(line, "BANDWIDTH").and_then(|b| b.trim().parse().ok());
        } else if lower.starts_with("#ext-x-key") {
            let method = attr(line, "METHOD").unwrap_or("").to_string();
            if method.eq_ignore_ascii_case("none") || method.is_empty() {
                cur_key = None;
            } else if method.eq_ignore_ascii_case("aes-128") {
                let Some(uri) = attr(line, "URI") else {
                    bail!("AES-128 key without URI");
                };
                cur_key = Some(Arc::new(Key {
                    spec: KeySpec {
                        uri: join_url(base, uri),
                        iv: parse_iv(line),
                    },
                }));
            } else {
                bail!("unsupported EXT-X-KEY method: {method}");
            }
        } else if lower.starts_with("#ext-x-map") {
            if let Some(uri) = attr(line, "URI") {
                cur_map = Some(join_url(base, uri));
            }
        } else if lower.starts_with("#ext-x-media-sequence") {
            media_seq = line
                .split(':')
                .nth(1)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
        } else if lower.starts_with("#ext-x-targetduration") {
            target_dur = line
                .split(':')
                .nth(1)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(4.0);
        } else if lower.starts_with("#ext-x-endlist") {
            ended = true;
        } else if lower.starts_with("#ext-x-byterange") {
            bail!("EXT-X-BYTERANGE segments are not supported");
        }
    }
    if master {
        // a variant list, not media: the caller re-fetches the chosen URL
        let Some((_, url)) = best else {
            bail!("master playlist without variants");
        };
        return Ok(Plan {
            target_dur,
            ended,
            items: Vec::new(),
            variant: Some(url),
        });
    }
    if items.is_empty() {
        bail!("no media segments in playlist");
    }
    Ok(Plan {
        target_dur,
        ended,
        items,
        variant: None,
    })
}

/// One fetched (and possibly decrypted) item.
enum Msg {
    Item(u64, Vec<u8>),
    Warn(String),
    Err(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HlsState {
    url: String,
    /// v2+: finished item indices (init + media, stream order). v1 (absent):
    /// media-only indices from the pre-AES build.
    v: Option<u8>,
    done: Vec<u64>,
    /// live only: URLs already written, in stream order
    written: Option<Vec<String>>,
}

async fn fetch_bytes(
    client: &reqwest::Client,
    headers: &reqwest::header::HeaderMap,
    url: &str,
) -> Result<Vec<u8>> {
    let r = client.get(url).headers(headers.clone()).send().await;
    match r {
        Ok(r) if r.status().is_success() => {
            let b = r.bytes().await.context("read body")?;
            Ok(b.to_vec())
        }
        Ok(r) => bail!("HTTP {} for {url}", r.status()),
        Err(e) => bail!("{e}"),
    }
}

/// Fetch `url`, descending through master playlists (variant lists) until a
/// media playlist is found. Returns the media playlist URL (kept for resume
/// state) and its parsed plan.
async fn resolve_media(
    client: &reqwest::Client,
    headers: &reqwest::header::HeaderMap,
    url: &str,
) -> Result<(String, Plan)> {
    let mut url = url.to_string();
    for _ in 0..4 {
        let bytes = fetch_bytes(client, headers, &url).await?;
        let text = String::from_utf8(bytes).context("playlist is not utf-8")?;
        let plan = parse_playlist(&text, &url)?;
        match plan.variant.clone() {
            Some(v) => {
                tracing::debug!("hls: master playlist -> {v}");
                url = v;
            }
            None => return Ok((url, plan)),
        }
    }
    bail!("playlist nesting too deep (master -> master -> ...)")
}

/// Normalize a URL for resume-state comparison: lowercase host, drop query
/// and fragment. Signed playlists rotate tokens in the query (`?t=...&s=`)
/// and reqwest lowercases the host of `resp.url()`, so raw strings from two
/// fetches of the same playlist never match otherwise.
fn norm_url(u: &str) -> String {
    let u = u.split('#').next().unwrap_or(u);
    let u = u.split('?').next().unwrap_or(u);
    match u.split_once("://") {
        Some((scheme, rest)) => match rest.split_once('/') {
            Some((h, p)) => format!("{scheme}://{}/{p}", h.to_ascii_lowercase()),
            None => format!("{scheme}://{}", rest.to_ascii_lowercase()),
        },
        None => u.to_string(),
    }
}

/// Fetch an item, decrypting it first when it carries an AES-128 key.
async fn fetch_item(
    client: &reqwest::Client,
    headers: &reqwest::header::HeaderMap,
    key_cache: &Mutex<HashMap<String, [u8; 16]>>,
    item: &Item,
) -> Result<Vec<u8>> {
    let body = fetch_bytes(client, headers, item.url()).await?;
    let Item::Media { key, seq, .. } = item else {
        return Ok(body); // init segments are not encrypted
    };
    let Some(key) = key else { return Ok(body) };
    let iv = key.iv_for(*seq)?;
    let material = {
        let cached = key_cache.lock().unwrap().get(&key.spec.uri).copied();
        match cached {
            Some(m) => m,
            None => {
                let k = fetch_bytes(client, headers, &key.spec.uri).await?;
                if k.len() != 16 {
                    bail!("AES key is {} bytes (want 16)", k.len());
                }
                let mut m = [0u8; 16];
                m.copy_from_slice(&k);
                key_cache.lock().unwrap().insert(key.spec.uri.clone(), m);
                m
            }
        }
    };
    let dec = Aes128CbcDec::new_from_slices(&material, &iv).context("AES init")?;
    let plain = dec
        .decrypt_padded_vec_mut::<cbc::cipher::block_padding::Pkcs7>(&body)
        .map_err(|_| anyhow::anyhow!("AES-128 decrypt failed (wrong key/IV) for {}", item.url()))?;
    Ok(plain)
}

fn snap(n: u64, done: &dyn Fn(u64) -> bool) -> Vec<SegmentInfo> {
    (0..n)
        .map(|i| SegmentInfo {
            start: i,
            end: i,
            done: u64::from(done(i)),
        })
        .collect()
}

/// Download a VOD or live HLS playlist into `job.out`.
pub async fn download(
    job: Job,
    events: mpsc::Sender<Progress>,
    kill: watch::Receiver<bool>,
    _content_type: Option<String>,
) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .build()?;

    let (final_url, plan) = match resolve_media(&client, &job.headers, &job.url).await {
        Ok(r) => r,
        Err(e) => return Err(e.context("playlist request failed")),
    };

    let filename = job
        .out
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "download".into());

    let spath = state_path(&job.out);
    let mut fresh = job.fresh;
    let mut st: HlsState = if fresh {
        std::fs::remove_file(&spath).ok();
        HlsState {
            url: final_url.clone(),
            v: Some(2),
            done: vec![],
            written: None,
        }
    } else {
        serde_json::from_str::<HlsState>(&std::fs::read_to_string(&spath).unwrap_or_default())
            .unwrap_or(HlsState {
                url: final_url.clone(),
                v: Some(2),
                done: vec![],
                written: None,
            })
    };

    // The output name is reused when the same page is re-downloaded (and
    // `base.v.mp4` is identical across qualities), so a stale state file must
    // never be trusted: `done` is a list of *indices*, and applying it to a
    // different playlist marks segments complete that were never written.
    // Same reasoning for a state file whose output file has disappeared
    // (indices recorded, bytes gone -> holes at the front).
    let state_ok =
        !st.url.is_empty() && norm_url(&st.url) == norm_url(&final_url) && job.out.exists();
    if !fresh && !state_ok {
        tracing::info!(
            "hls: discarding resume state for {} (state url {}, wanted {final_url})",
            job.out.display(),
            if st.url.is_empty() { "-" } else { &st.url }
        );
        fresh = true;
        std::fs::remove_file(&spath).ok();
        st = HlsState {
            url: final_url.clone(),
            v: Some(2),
            done: vec![],
            written: None,
        };
    }

    let workers = job.segments.clamp(1, 16) as usize;
    let total = Arc::new(AtomicU64::new(0));
    let mut writer = if fresh {
        // restarting: the old bytes belong to another playlist run
        tokio::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&job.out)
            .await
            .with_context(|| format!("open {}", job.out.display()))?
    } else {
        tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&job.out)
            .await
            .with_context(|| format!("open {}", job.out.display()))?
    };
    total.fetch_add(writer.metadata().await?.len(), Ordering::Relaxed);

    events
        .send(Progress::Started {
            url: job.url.clone(),
            filename,
            size: None,
            segments: workers,
        })
        .await
        .ok();

    if plan.ended {
        run_vod(
            &client,
            &job,
            &events,
            kill,
            plan,
            st,
            &spath,
            &mut writer,
            total.clone(),
            workers,
        )
        .await
    } else {
        run_live(
            &client,
            &job,
            &events,
            kill,
            plan,
            st,
            &spath,
            &mut writer,
            total,
        )
        .await
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_vod(
    client: &reqwest::Client,
    job: &Job,
    events: &mpsc::Sender<Progress>,
    kill: watch::Receiver<bool>,
    plan: Plan,
    st: HlsState,
    spath: &std::path::Path,
    writer: &mut tokio::fs::File,
    total: Arc<AtomicU64>,
    workers: usize,
) -> Result<()> {
    let n = plan.items.len() as u64;
    // v1 state (pre multi-init) counts media segments only; offset by the
    // leading init item when present
    let v1_offset = match (st.v, plan.items.first()) {
        (None | Some(1), Some(Item::Init { .. })) => 1,
        _ => 0,
    };
    let have: HashSet<u64> = if st.written.is_none() && !st.done.is_empty() {
        st.done.iter().map(|i| i + v1_offset).collect()
    } else if let Some(w) = &st.written {
        plan.items
            .iter()
            .enumerate()
            .filter(|(_, it)| w.contains(&it.url().to_string()))
            .map(|(i, _)| i as u64)
            .collect()
    } else {
        HashSet::new()
    };

    events
        .send(Progress::Detail {
            segments: snap(n, &|i| have.contains(&i)),
        })
        .await
        .ok();

    let (tx, mut rx) = mpsc::channel::<Msg>(workers * 2);
    let next = Arc::new(AtomicU64::new(0));
    let key_cache = Arc::new(Mutex::new(HashMap::<String, [u8; 16]>::new()));
    let limiter = job
        .limits
        .limiter
        .clone()
        .unwrap_or_else(|| Arc::new(crate::engine::Limiter::new(job.limits.max_speed)));

    for _ in 0..workers {
        let (tx, client, next, have, key_cache, total) = (
            tx.clone(),
            client.clone(),
            next.clone(),
            have.clone(),
            key_cache.clone(),
            total.clone(),
        );
        let limiter = limiter.clone();
        let (items, headers, retries, kill) = (
            Arc::new(plan.items.clone()),
            job.headers.clone(),
            job.limits.retries,
            kill.clone(),
        );
        tokio::spawn(async move {
            loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= items.len() as u64 {
                    return;
                }
                if have.contains(&i) {
                    continue;
                }
                let item = items[i as usize].clone();
                let mut err = String::new();
                let mut body = None;
                for att in 0..=retries {
                    if *kill.borrow() {
                        return;
                    }
                    match fetch_item(&client, &headers, &key_cache, &item).await {
                        Ok(b) => {
                            body = Some(b);
                            break;
                        }
                        Err(e) => err = format!("{e:#}"),
                    }
                    if tx
                        .send(Msg::Warn(format!("item {i}: {err}")))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(300 * (att + 1) as u64)).await;
                }
                let Some(b) = body else {
                    let _ = tx
                        .send(Msg::Err(format!("item {i} failed after retries: {err}")))
                        .await;
                    return;
                };
                total.fetch_add(b.len() as u64, Ordering::Relaxed);
                limiter.acquire(b.len()).await;
                if tx.send(Msg::Item(i, b)).await.is_err() {
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
                None => break,
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
            Msg::Item(i, data) => {
                pending.insert(i, data);
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
                        .send(Progress::Bytes {
                            delta: bytes,
                            total_done: t,
                            size: None,
                        })
                        .await
                        .ok();
                }
                if fail.is_some() {
                    break;
                }
                if write_idx < n {
                    events
                        .send(Progress::Detail {
                            segments: snap(n, &|i| dl_done.contains(&i)),
                        })
                        .await
                        .ok();
                }
            }
        }
    }

    if fail.is_none() && write_idx >= n && !cancelled {
        std::fs::remove_file(spath).ok();
        let size = total.load(Ordering::Relaxed);
        events
            .send(Progress::Completed { size, sha256: None })
            .await
            .ok();
        return Ok(());
    }
    if cancelled || (fail.is_none() && write_idx < n && *kill.borrow()) {
        let t = total.load(Ordering::Relaxed);
        let st = HlsState {
            url: job.url.clone(),
            v: Some(2),
            done: sorted(&dl_done),
            written: None,
        };
        std::fs::write(spath, serde_json::to_string(&st).unwrap_or_default()).ok();
        events
            .send(Progress::Cancelled { total_done: t })
            .await
            .ok();
        return Ok(());
    }
    let e = fail.unwrap_or_else(|| "HLS download incomplete".into());
    events
        .send(Progress::Failed { error: e.clone() })
        .await
        .ok();
    bail!("{e}")
}

#[allow(clippy::too_many_arguments)]
async fn run_live(
    client: &reqwest::Client,
    job: &Job,
    events: &mpsc::Sender<Progress>,
    mut kill: watch::Receiver<bool>,
    first: Plan,
    st: HlsState,
    spath: &std::path::Path,
    writer: &mut tokio::fs::File,
    total: Arc<AtomicU64>,
) -> Result<()> {
    let mut written: HashSet<String> = st.written.unwrap_or_default().into_iter().collect();
    let key_cache = Arc::new(Mutex::new(HashMap::<String, [u8; 16]>::new()));
    let limiter = job
        .limits
        .limiter
        .clone()
        .unwrap_or_else(|| Arc::new(crate::engine::Limiter::new(job.limits.max_speed)));
    let mut poll_n: usize = 0;

    loop {
        let plan = if poll_n == 0 {
            first.clone_items()
        } else {
            match resolve_media(client, &job.headers, &job.url).await {
                Ok((_, p)) => p,
                Err(e) => {
                    tracing::warn!("live poll failed: {e:#}");
                    if !sleep_killable(&mut kill, 2.0).await {
                        break;
                    }
                    continue;
                }
            }
        };
        poll_n += 1;

        // candidates in playlist order, skipping already-written URLs
        let candidates: Vec<Item> = plan
            .items
            .iter()
            .filter(|it| !written.contains(it.url()))
            .cloned()
            .collect();
        let n_all = plan.items.len() as u64;
        let written_n = (n_all - candidates.len() as u64) as u64;
        events
            .send(Progress::Detail {
                segments: snap(n_all, &|i| i < written_n),
            })
            .await
            .ok();

        // fetch candidates with a small pool, write strictly in order
        let (tx, mut rx) = mpsc::channel::<Msg>(8);
        let ncand = candidates.len() as u64;
        let next = Arc::new(AtomicU64::new(0));
        let pool = 4.min(workers_of(job));
        for _ in 0..pool {
            let (tx, client, next, key_cache) =
                (tx.clone(), client.clone(), next.clone(), key_cache.clone());
            let limiter = limiter.clone();
            let (cands, headers) = (Arc::new(candidates.clone()), job.headers.clone());
            tokio::spawn(async move {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= cands.len() as u64 {
                        return;
                    }
                    match fetch_item(&client, &headers, &key_cache, &cands[i as usize]).await {
                        Ok(b) => {
                            limiter.acquire(b.len()).await;
                            if tx.send(Msg::Item(i, b)).await.is_err() {
                                return;
                            }
                        }
                        Err(e) => {
                            // live: drop the segment, do not fail the job
                            let _ = tx
                                .send(Msg::Warn(cands[i as usize].url().to_string()))
                                .await;
                            tracing::debug!("live segment dropped: {e:#}");
                        }
                    }
                }
            });
        }
        drop(tx);

        let mut pending: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
        let mut wpos: u64 = 0;
        let mut dropped: HashSet<u64> = HashSet::new();
        enum Step {
            Msg(Msg),
            Drained,
            Killed(bool),
        }
        loop {
            let step = tokio::select! {
                m = rx.recv() => match m {
                    Some(m) => Step::Msg(m),
                    None => Step::Drained,
                },
                k = changed_or_stall(&mut kill) => Step::Killed(k),
            };
            match step {
                Step::Drained => break,
                Step::Killed(true) => {
                    return finish_live(job, events, kill, writer, &total, spath, written, false)
                        .await;
                }
                Step::Killed(false) => continue,
                Step::Msg(Msg::Warn(u)) => {
                    // a dropped candidate still advances the write cursor so
                    // ordering of later segments is preserved
                    if let Some(pos) = candidates.iter().position(|c| c.url() == u) {
                        dropped.insert(pos as u64);
                    }
                }
                Step::Msg(Msg::Err(e)) => {
                    tracing::debug!("live item error: {e}");
                }
                Step::Msg(Msg::Item(i, data)) => {
                    pending.insert(i, data);
                }
            }
            // flush everything contiguous (skipping dropped)
            while pending.contains_key(&wpos) || dropped.contains(&wpos) {
                if let Some(buf) = pending.remove(&wpos) {
                    if writer.write_all(&buf).await.is_err() {
                        bail!("write failed");
                    }
                    let _ = writer.flush().await;
                    total.fetch_add(buf.len() as u64, Ordering::Relaxed);
                    let t = total.load(Ordering::Relaxed);
                    events
                        .send(Progress::Bytes {
                            delta: buf.len() as u64,
                            total_done: t,
                            size: None,
                        })
                        .await
                        .ok();
                    if let Some(it) = candidates.get(wpos as usize) {
                        written.insert(it.url().to_string());
                    }
                }
                wpos += 1;
            }
        }
        let _ = ncand;

        if plan.ended && candidates.iter().all(|c| written.contains(c.url())) {
            return finish_live(job, events, kill, writer, &total, spath, written, true).await;
        }

        let wait = plan.target_dur.clamp(1.0, 8.0) as f64 / 2.0;
        if !sleep_killable(&mut kill, wait).await {
            break;
        }
    }

    // kill observed
    finish_live(job, events, kill, writer, &total, spath, written, false).await
}

fn workers_of(job: &Job) -> usize {
    job.segments.clamp(1, 16) as usize
}

/// Wait for a kill signal; pends forever when the sender is gone (the
/// job can never be killed then), so `select!` does not spin.
async fn changed_or_stall(kill: &mut watch::Receiver<bool>) -> bool {
    if kill.changed().await.is_err() {
        std::future::pending::<()>().await;
    }
    *kill.borrow()
}

/// Sleep that returns false when killed.
async fn sleep_killable(kill: &mut watch::Receiver<bool>, secs: f64) -> bool {
    let dur = Duration::from_millis((secs * 1000.0) as u64);
    if kill.has_changed().is_err() {
        // sender already dropped: nothing can ever kill this job
        tokio::time::sleep(dur).await;
        return true;
    }
    tokio::select! {
        _ = tokio::time::sleep(dur) => true,
        r = kill.changed() => match r {
            Ok(()) => !*kill.borrow(),
            Err(_) => true, // dropped mid-sleep: not killed
        },
    }
}

#[allow(clippy::too_many_arguments)]
async fn finish_live(
    job: &Job,
    events: &mpsc::Sender<Progress>,
    kill: watch::Receiver<bool>,
    writer: &mut tokio::fs::File,
    total: &Arc<AtomicU64>,
    spath: &std::path::Path,
    written: HashSet<String>,
    complete: bool,
) -> Result<()> {
    let _ = writer;
    let t = total.load(Ordering::Relaxed);
    if complete {
        std::fs::remove_file(spath).ok();
        events
            .send(Progress::Completed {
                size: t,
                sha256: None,
            })
            .await
            .ok();
        return Ok(());
    }
    let st = HlsState {
        url: job.url.clone(),
        v: Some(2),
        done: vec![],
        written: Some(written.into_iter().collect()),
    };
    std::fs::write(spath, serde_json::to_string(&st).unwrap_or_default()).ok();
    if *kill.borrow() {
        events
            .send(Progress::Cancelled { total_done: t })
            .await
            .ok();
        return Ok(());
    }
    let e = "live stream ended without ENDLIST".to_string();
    events
        .send(Progress::Failed { error: e.clone() })
        .await
        .ok();
    bail!("{e}")
}

fn sorted(set: &HashSet<u64>) -> Vec<u64> {
    let mut v: Vec<u64> = set.iter().copied().collect();
    v.sort_unstable();
    v
}

impl Plan {
    fn clone_items(&self) -> Plan {
        Plan {
            target_dur: self.target_dur,
            ended: self.ended,
            items: self.items.clone(),
            variant: self.variant.clone(),
        }
    }
}
