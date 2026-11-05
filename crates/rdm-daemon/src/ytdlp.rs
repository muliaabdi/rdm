//! yt-dlp integration: page extraction + format selection.
//! The segmented engine still performs the actual downloads; yt-dlp only
//! resolves direct media URLs and metadata.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;

#[derive(Debug, Clone, Serialize)]
pub struct Fmt {
    pub id: String,
    pub ext: String,
    pub height: Option<u64>,
    pub vcodec: String,
    pub acodec: String,
    pub filesize: Option<u64>,
    pub progressive: bool,
    #[serde(skip_serializing)]
    pub url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExtractInfo {
    pub title: String,
    pub webpage_url: String,
    pub duration: Option<f64>,
    pub formats: Vec<Fmt>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FmtSeg {
    pub start: u64,
    pub end: u64,
    pub done: u64,
}

pub fn yt_dlp_bin() -> PathBuf {
    if let Ok(p) = std::env::var("RDM_YTDLP") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let local = PathBuf::from(&home).join(".local/bin/yt-dlp");
    if local.is_file() {
        return local;
    }
    PathBuf::from("yt-dlp")
}

/// yt-dlp needs a JS runtime (node) for YouTube signatures and ffmpeg for
/// muxing; the daemon may run with a minimal PATH, so add the usual suspects.
fn tool_path() -> String {
    let mut dirs: Vec<String> = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        let nvm = PathBuf::from(&home).join(".nvm/versions/node");
        if let Ok(rd) = std::fs::read_dir(&nvm) {
            let mut vs: Vec<String> = rd
                .flatten()
                .filter(|e| e.path().join("bin").is_dir())
                .map(|e| e.path().join("bin").to_string_lossy().to_string())
                .collect();
            vs.sort();
            dirs.extend(vs);
        }
    }
    dirs.push("/usr/local/bin".into());
    dirs.push("/usr/bin".into());
    dirs.push("/bin".into());
    dirs.push(std::env::var("PATH").unwrap_or_default());
    dirs.join(":")
}

async fn run_ytdlp(args: &[&str], timeout: Duration) -> Result<String> {
    let bin = yt_dlp_bin();
    let mut cmd = tokio::process::Command::new(&bin);
    cmd.args(args)
        .env("PATH", tool_path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // never leak a hung yt-dlp if the future is dropped mid-wait
        .kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawn {}", bin.display()))?;
    let mut so = child.stdout.take().unwrap();
    let mut se = child.stderr.take().unwrap();
    let (txo, ro) = tokio::sync::oneshot::channel::<String>();
    let (txe, re) = tokio::sync::oneshot::channel::<String>();
    tokio::spawn(async move {
        let mut s = String::new();
        let _ = so.read_to_string(&mut s).await;
        let _ = txo.send(s);
    });
    tokio::spawn(async move {
        let mut s = String::new();
        let _ = se.read_to_string(&mut s).await;
        let _ = txe.send(s);
    });
    let wait = tokio::time::timeout(timeout, child.wait()).await;
    if wait.is_err() {
        let _ = child.start_kill();
    }
    // give the readers a moment to drain whatever yt-dlp wrote before it died
    let out = tokio::time::timeout(Duration::from_secs(5), ro)
        .await
        .ok()
        .and_then(|r| r.ok())
        .unwrap_or_default();
    let err = tokio::time::timeout(Duration::from_secs(5), re)
        .await
        .ok()
        .and_then(|r| r.ok())
        .unwrap_or_default();
    // yt-dlp prints `null` on stdout when extraction fails outright
    let usable = !out.trim().is_empty() && out.trim() != "null";
    match wait {
        Err(_) => {
            let _ = child.wait().await;
            bail!("yt-dlp timed out after {}s", timeout.as_secs())
        }
        Ok(Ok(st)) if st.success() && usable => Ok(out),
        Ok(Ok(_)) if usable => Ok(out),
        _ => {
            // keep the last few stderr lines: yt-dlp puts the real cause
            // (PO token, nsig, sign-in) after its generic preamble
            let tail = err
                .lines()
                .filter(|l| !l.trim().is_empty())
                .rev()
                .take(6)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join(" | ");
            let msg = if tail.is_empty() {
                "yt-dlp failed without output".into()
            } else {
                tail
            };
            bail!("{msg}")
        }
    }
}

#[derive(Deserialize)]
struct RawFmt {
    format_id: String,
    ext: Option<String>,
    height: Option<u64>,
    vcodec: Option<String>,
    acodec: Option<String>,
    filesize: Option<u64>,
    filesize_approx: Option<u64>,
    url: Option<String>,
}

#[derive(Deserialize)]
struct RawInfo {
    title: Option<String>,
    webpage_url: Option<String>,
    duration: Option<f64>,
    #[serde(default)]
    formats: Vec<RawFmt>,
    url: Option<String>,
}

/// yt-dlp deprecated `--add-header "Cookie: ..."` (and scopes header cookies
/// wrong); feed it a proper Netscape cookie file scoped to the page host.
struct CookieFile(std::path::PathBuf);
impl Drop for CookieFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn write_cookie_file(page_url: &str, header: &str) -> Option<CookieFile> {
    let rest = page_url.split("://").nth(1)?;
    let host = rest
        .split(['/', '?', '#'])
        .next()?
        .trim_start_matches("www.")
        .to_string();
    if host.is_empty() {
        return None;
    }
    let name = format!(
        "rdm-cookies-{}-{}.txt",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let path = std::env::temp_dir().join(name);
    let mut body = String::from("# Netscape HTTP Cookie File\n");
    let mut n = 0usize;
    for pair in header.split(';') {
        let Some((k, v)) = pair.trim().split_once('=') else {
            continue;
        };
        body.push_str(&format!(
            ".{host}\tTRUE\t/\tTRUE\t4102444800\t{}\t{}\n",
            k.trim(),
            v.trim()
        ));
        n += 1;
    }
    if n == 0 {
        return None;
    }
    // 0600 from creation: a write-then-chmod leaves a window where other
    // local users can read the session cookies off shared /tmp
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .ok()?;
        f.write_all(body.as_bytes()).ok()?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&path, &body).ok()?;
    }
    Some(CookieFile(path))
}

/// Extract metadata + direct format URLs for a page/video URL.
///
/// YouTube rejects some client/cookie/UA combinations with
/// "The page needs to be reloaded" (the `web` client now wants a PO token).
/// So we walk a small ladder of attempts instead of failing the job outright.
pub async fn extract(
    page_url: &str,
    cookie: Option<&str>,
    ua: Option<&str>,
) -> Result<ExtractInfo> {
    let is_yt = is_youtube(page_url);
    // (send cookie, send ua, avoid the web client)
    let mut ladder: Vec<(bool, bool, bool)> = vec![(true, true, false)];
    if is_yt {
        // a browser UA/cookie set pushes YouTube onto the client that now
        // demands a PO token; dropping them (or the client) gets through
        ladder.push((false, false, false));
        ladder.push((true, true, true));
        ladder.push((false, false, true));
    }
    let cookie = cookie.filter(|c| !c.is_empty());
    let ua = ua.filter(|u| !u.is_empty());

    let mut last: Option<anyhow::Error> = None;
    for (use_cookie, use_ua, avoid_web) in ladder {
        let c = if use_cookie { cookie } else { None };
        let u = if use_ua { ua } else { None };
        match extract_once(page_url, c, u, avoid_web).await {
            Ok(info) => return Ok(info),
            Err(e) => {
                tracing::warn!(
                    "yt-dlp extract failed (cookie={use_cookie} ua={use_ua} no_web={avoid_web}): {e:#}"
                );
                last = Some(e);
            }
        }
    }
    let msg = last
        .map(|e| format!("{e:#}"))
        .unwrap_or_else(|| "yt-dlp extraction failed".to_string());
    bail!("{msg}")
}

fn is_youtube(url: &str) -> bool {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    host.ends_with("youtube.com")
        || host.ends_with("youtu.be")
        || host.ends_with("youtube-nocookie.com")
}

async fn extract_once(
    page_url: &str,
    cookie: Option<&str>,
    ua: Option<&str>,
    avoid_web: bool,
) -> Result<ExtractInfo> {
    let mut args: Vec<String> = vec![
        "-J".into(),
        // IPv6 to YouTube black-holes on some networks and stalls extraction
        // for minutes; IPv4 resolves the page in ~2s
        "--force-ipv4".into(),
        "--no-playlist".into(),
        page_url.into(),
    ];
    if avoid_web {
        // `web` needs a PO token now; its siblings still serve formats
        args.push("--extractor-args".into());
        args.push("youtube:player_client=default,-web".into());
    }
    // keep the temp file alive until the child is done (Drop removes it)
    let cfile = cookie.and_then(|c| write_cookie_file(page_url, c));
    if let Some(f) = &cfile {
        args.push("--cookies".into());
        args.push(f.0.to_string_lossy().into_owned());
    }
    if let Some(u) = ua {
        args.push("--user-agent".into());
        args.push(u.into());
    }
    let argrefs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let out = run_ytdlp(&argrefs, Duration::from_secs(90)).await?;
    let raw: RawInfo = serde_json::from_str(out.trim()).context("parse yt-dlp json")?;
    let mut formats: Vec<Fmt> = raw
        .formats
        .into_iter()
        .filter_map(|f| {
            let url = f.url?;
            let v = f.vcodec.clone().unwrap_or_default();
            let a = f.acodec.clone().unwrap_or_default();
            let has_v = !v.is_empty() && v != "none";
            let has_a = !a.is_empty() && a != "none";
            Some(Fmt {
                id: f.format_id,
                ext: f.ext.unwrap_or_else(|| "bin".into()),
                height: f.height,
                vcodec: if has_v { v } else { "none".into() },
                acodec: if has_a { a } else { "none".into() },
                filesize: f.filesize.or(f.filesize_approx),
                progressive: has_v && has_a,
                url,
            })
        })
        .collect();
    // single direct URL (generic extractor): synthesize one progressive format
    if formats.is_empty() {
        if let Some(u) = raw.url {
            formats.push(Fmt {
                id: "direct".into(),
                ext: std::path::Path::new(page_url)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("mp4")
                    .to_string(),
                height: None,
                vcodec: "?".into(),
                acodec: "?".into(),
                filesize: None,
                progressive: true,
                url: u,
            });
        }
    }
    if formats.is_empty() {
        bail!("no downloadable format found");
    }
    Ok(ExtractInfo {
        title: raw.title.unwrap_or_else(|| "video".into()),
        webpage_url: raw.webpage_url.unwrap_or_else(|| page_url.to_string()),
        duration: raw.duration,
        formats,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistEntry {
    pub url: String,
    pub title: Option<String>,
    pub id: Option<String>,
}

#[derive(Deserialize)]
struct RawPlaylistEntry {
    url: Option<String>,
    id: Option<String>,
    title: Option<String>,
    ie_key: Option<String>,
}

#[derive(Deserialize)]
struct RawPlaylist {
    _type: Option<String>,
    title: Option<String>,
    #[serde(default)]
    entries: Vec<Option<RawPlaylistEntry>>,
}

/// Parse yt-dlp `--flat-playlist -J` output. Returns (title, entries).
/// Rejects non-playlist JSON (`_type != "playlist"`). Normalizes bare
/// video IDs (YouTube "dQw4w9WgXcQ" -> full URL) and dedupes by URL.
pub fn parse_playlist_json(json_str: &str) -> Result<(String, Vec<PlaylistEntry>)> {
    let raw: RawPlaylist = serde_json::from_str(json_str).context("parse playlist json")?;
    if raw._type.as_deref() != Some("playlist") {
        bail!("not a playlist");
    }
    let title = raw.title.unwrap_or_else(|| "playlist".into());
    let mut out: Vec<PlaylistEntry> = Vec::new();
    for entry in raw.entries.into_iter().flatten() {
        let (raw_url, id, t) = (entry.url, entry.id, entry.title);
        let Some(u) = raw_url.or_else(|| id.clone()) else {
            continue;
        };
        // normalize: YouTube bare IDs (or ie_key=Youtube) become watch URLs
        let norm = if u.starts_with("http://") || u.starts_with("https://") {
            u
        } else if entry.ie_key.as_deref() == Some("Youtube")
            || (u.len() == 11
                && u.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        {
            format!("https://www.youtube.com/watch?v={u}")
        } else {
            // skip bare non-URL strings that don't look like YouTube IDs
            continue;
        };
        if !out.iter().any(|e| e.url == norm) {
            out.push(PlaylistEntry {
                url: norm,
                title: t,
                id,
            });
        }
    }
    Ok((title, out))
}

/// Quick check whether a URL targets a playlist. Fast: looks for common
/// query params (`list=`, `/playlist/`, `/album/`) before invoking yt-dlp.
pub fn looks_like_playlist(url: &str) -> bool {
    let u = url.to_ascii_lowercase();
    u.contains("list=") || u.contains("/playlist") || u.contains("/sets/") || u.contains("/album/")
}

/// Extract playlist entries using `--flat-playlist` (very fast: does not
/// probe formats for every video, just reads the list index).
pub async fn extract_playlist(
    page_url: &str,
    cookie: Option<&str>,
    ua: Option<&str>,
) -> Result<(String, Vec<PlaylistEntry>)> {
    let mut args: Vec<String> = vec![
        "-J".into(),
        "--flat-playlist".into(),
        "--force-ipv4".into(),
        page_url.into(),
    ];
    let cfile = cookie.and_then(|c| write_cookie_file(page_url, c));
    if let Some(f) = &cfile {
        args.push("--cookies".into());
        args.push(f.0.to_string_lossy().into_owned());
    }
    if let Some(u) = ua {
        args.push("--user-agent".into());
        args.push(u.into());
    }
    let argrefs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let out = run_ytdlp(&argrefs, Duration::from_secs(60)).await?;
    parse_playlist_json(out.trim())
}

/// Pick best (video, Option<audio>) pair under `max_height`.
pub fn pick(info: &ExtractInfo, max_height: u64, audio_only: bool) -> (Fmt, Option<Fmt>) {
    let audio_best = || {
        info.formats
            .iter()
            .filter(|f| f.acodec != "none" && f.vcodec == "none")
            .max_by_key(|f| f.filesize.unwrap_or(0))
            .cloned()
    };
    if audio_only {
        if let Some(a) = audio_best() {
            return (a, None);
        }
    }
    // progressive preferred: audio+video in one file
    let prog = info
        .formats
        .iter()
        .filter(|f| f.progressive && f.height.is_none_or(|h| h <= max_height))
        .max_by_key(|f| f.height.unwrap_or(0));
    if let Some(p) = prog {
        return (p.clone(), None);
    }
    // dash: best video-only under cap (prefer mp4/avc for compatibility)
    let vid = info
        .formats
        .iter()
        .filter(|f| {
            f.vcodec != "none" && f.acodec == "none" && f.height.is_some_and(|h| h <= max_height)
        })
        .max_by_key(|f| {
            (
                f.height.unwrap_or(0),
                f.ext == "mp4" && f.vcodec.starts_with("avc"),
            )
        })
        .cloned();
    match vid {
        Some(v) => (v, audio_best()),
        None => {
            // nothing under cap: take best video whatever the height
            let v = info
                .formats
                .iter()
                .filter(|f| f.vcodec != "none")
                .max_by_key(|f| f.height.unwrap_or(0))
                .cloned()
                .unwrap_or_else(|| info.formats[0].clone());
            (v, if audio_only { None } else { audio_best() })
        }
    }
}

#[cfg(test)]
mod playlist_tests {
    use super::*;

    const FIX: &str = r#"{
        "_type": "playlist",
        "title": "My List",
        "webpage_url": "https://www.youtube.com/playlist?list=PLx",
        "entries": [
            {"_type": "url", "ie_key": "Youtube", "id": "dQw4w9WgXcQ", "url": "dQw4w9WgXcQ", "title": "Video A"},
            {"_type": "url", "url": "https://vimeo.com/12345", "title": "Video B"},
            {"_type": "url", "id": "junk", "url": "not-a-url", "title": "skip"},
            {"_type": "url", "url": "https://vimeo.com/12345", "title": "dupe"},
            {"_type": "url", "url": "dQw4w9WgXcQ", "title": "dupe id"}
        ]
    }"#;

    #[test]
    fn parses_flat_playlist_normalizes_ids_and_dedupes() {
        let (title, entries) = parse_playlist_json(FIX).unwrap();
        assert_eq!(title, "My List");
        let urls: Vec<&str> = entries.iter().map(|e| e.url.as_str()).collect();
        assert_eq!(
            urls,
            vec![
                "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
                "https://vimeo.com/12345",
            ]
        );
        assert_eq!(entries[0].title.as_deref(), Some("Video A"));
    }

    #[test]
    fn rejects_non_playlist_json() {
        let single = r#"{"_type":"video","title":"solo","formats":[]}"#;
        assert!(parse_playlist_json(single).is_err());
        assert!(parse_playlist_json("null").is_err());
    }

    #[test]
    fn empty_playlist_is_ok_but_empty() {
        let empty = r#"{"_type":"playlist","title":"E","entries":[]}"#;
        let (t, e) = parse_playlist_json(empty).unwrap();
        assert_eq!(t, "E");
        assert!(e.is_empty());
    }
}
