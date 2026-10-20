//! gallery-dl integration: second page extractor for image galleries and
//! boards yt-dlp cannot handle (pixiv, danbooru, imgur albums, exhentai,
//! wallhaven, ...). yt-dlp stays first; gallery-dl is the fallback, so the
//! ladder in `run_page`/`extract` tries media first, pictures second.

use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{bail, Context, Result};
use tokio::io::AsyncReadExt;

#[derive(Debug, Clone)]
pub struct Gallery {
    pub title: String,
    /// (direct media url, sanitized filename) in gallery order
    pub items: Vec<(String, String)>,
}

pub fn gallery_dl_bin() -> PathBuf {
    if let Ok(p) = std::env::var("RDM_GALLERYDL") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let local = PathBuf::from(&home).join(".local/bin/gallery-dl");
    if local.is_file() {
        return local;
    }
    PathBuf::from("gallery-dl")
}

pub fn installed() -> bool {
    let b = gallery_dl_bin();
    b.is_file() || std::env::var("RDM_GALLERYDL").is_ok()
}

fn name_from(url: &str, ext: Option<&str>) -> String {
    let base = url
        .rsplit('/')
        .next()
        .unwrap_or("")
        .split('?')
        .next()
        .unwrap_or("");
    if base.is_empty() {
        return format!("file.{}", ext.unwrap_or("bin"));
    }
    if base.contains('.') {
        base.to_string()
    } else {
        format!("{base}.{}", ext.unwrap_or("bin"))
    }
}

/// Resolve a gallery/page url into its direct media list. One JSON object is
/// printed per file (json-lines), so the process stdout is streamed instead
/// of buffered: galleries can be huge.
pub async fn extract(url: &str, ua: Option<&str>) -> Result<Gallery> {
    if !installed() {
        bail!("gallery-dl not installed");
    }
    // gallery-dl only knows threads.net, browsers only ever serve threads.com
    let url = if let Some(rest) = url
        .strip_prefix("https://www.threads.com/")
        .or_else(|| url.strip_prefix("https://threads.com/"))
    {
        format!("https://www.threads.net/{rest}")
    } else {
        url.to_string()
    };
    let bin = gallery_dl_bin();
    let mut cmd = tokio::process::Command::new(&bin);
    cmd.args(["--dump-json", "--no-download", url.as_str()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(ua) = ua.filter(|s| !s.is_empty()) {
        cmd.args(["--user-agent", ua]);
    }
    let mut child = cmd.spawn().context("spawn gallery-dl")?;
    let mut stdout = child.stdout.take().context("gallery-dl stdout")?;
    // drain stderr concurrently: both pipes must be read at once or the
    // child blocks once one of them fills up
    let err_task = child.stderr.take().map(|mut e| {
        tokio::spawn(async move {
            let mut b = Vec::new();
            let _ = e.read_to_end(&mut b).await;
            b
        })
    });

    let mut buf = Vec::new();
    stdout.read_to_end(&mut buf).await.ok();
    let status = child.wait().await.context("wait gallery-dl")?;
    let stderr = match err_task {
        Some(h) => h.await.unwrap_or_default(),
        None => Vec::new(),
    };
    if !status.success() {
        let text = String::from_utf8_lossy(&stderr);
        let msg = text
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("extraction failed");
        bail!("gallery-dl: {msg}");
    }

    let text = String::from_utf8_lossy(&buf);
    // v1.32+ prints one JSON array of [queue-index, metadata] pairs (index
    // -1 = error entry); older builds printed one metadata object per line.
    // Accept both shapes.
    let entries: Vec<serde_json::Value> =
        if let Ok(arr) = serde_json::from_str::<serde_json::Value>(&text) {
            arr.as_array().cloned().unwrap_or_default()
        } else {
            text.lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .collect()
        };

    let mut items = Vec::new();
    let mut title = String::new();
    for v in entries {
        let obj = if v.is_array() {
            match v.as_array().and_then(|a| a.get(1).cloned()) {
                Some(o) if o.is_object() => o,
                _ => continue, // [-1, {"error": ...}] etc.
            }
        } else if v.is_object() {
            v
        } else {
            continue;
        };
        let Some(url) = obj["url"].as_str().filter(|u| u.starts_with("http")) else {
            continue;
        };
        let name = obj["filename"]
            .as_str()
            .filter(|f| !f.is_empty())
            .map(|f| {
                // keep only the basename: every file lands in one folder
                f.rsplit(['/', '\\']).next().unwrap_or(f).to_string()
            })
            .filter(|f| !f.is_empty())
            .unwrap_or_else(|| name_from(url, obj["extension"].as_str()));
        items.push((url.to_string(), rdm_core::engine::sanitize_filename(&name)));
        if title.is_empty() {
            title = obj["gallery"]["title"]
                .as_str()
                .or_else(|| obj["title"].as_str())
                .unwrap_or_default()
                .to_string();
        }
    }
    if items.is_empty() {
        bail!("gallery-dl: no files found");
    }
    Ok(Gallery { title, items })
}
