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

async fn run_ytdlp(args: &[&str], timeout: Duration) -> Result<String> {
    let bin = yt_dlp_bin();
    let mut cmd = tokio::process::Command::new(&bin);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().with_context(|| format!("spawn {}", bin.display()))?;
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
    let out = tokio::time::timeout(Duration::from_secs(5), ro).await;
    let err = tokio::time::timeout(Duration::from_secs(5), re).await;
    let out = out.ok().and_then(|r| r.ok()).unwrap_or_default();
    let err = err.ok().and_then(|r| r.ok()).unwrap_or_default();
    match wait {
        Ok(Ok(st)) if st.success() && !out.is_empty() => Ok(out),
        Ok(Ok(_)) | Err(_) if !out.is_empty() => Ok(out),
        _ => {
            let tail = err.lines().rev().take(3).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join(" | ");
            let msg = if tail.is_empty() { "yt-dlp failed without output".into() } else { tail };
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

/// Extract metadata + direct format URLs for a page/video URL.
pub async fn extract(page_url: &str, cookie: Option<&str>, ua: Option<&str>) -> Result<ExtractInfo> {
    let mut args: Vec<String> = vec![
        "-J".into(),
        "--no-playlist".into(),
        "--no-warnings".into(),
        page_url.into(),
    ];
    if let Some(c) = cookie.filter(|c| !c.is_empty()) {
        args.push("--add-header".into());
        args.push(format!("Cookie: {c}"));
    }
    if let Some(u) = ua.filter(|u| !u.is_empty()) {
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
        .filter(|f| f.progressive && f.height.map_or(true, |h| h <= max_height))
        .max_by_key(|f| f.height.unwrap_or(0));
    if let Some(p) = prog {
        return (p.clone(), None);
    }
    // dash: best video-only under cap (prefer mp4/avc for compatibility)
    let vid = info
        .formats
        .iter()
        .filter(|f| f.vcodec != "none" && f.acodec == "none" && f.height.map_or(false, |h| h <= max_height))
        .max_by_key(|f| (f.height.unwrap_or(0), f.ext == "mp4" && f.vcodec.starts_with("avc")))
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
