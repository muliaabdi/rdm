//! Settings file (`~/.local/share/rdm/settings.json`), shared with rdm-ui,
//! plus path helpers that depend on it (base/category folders).

use std::path::PathBuf;

use serde_json::json;

use crate::socket_path;

/// Shared settings file, also written by rdm-ui (dir/segments/lang/intercept_ext).
/// Lives in the persistent data dir; falls back to the legacy location next to
/// the socket (tmpfs) when only that exists, so nothing is lost on upgrade.
pub fn settings_path() -> PathBuf {
    crate::data_dir().join("settings.json")
}

pub(crate) fn settings_json() -> serde_json::Value {
    let p = settings_path();
    let b = std::fs::read(&p).ok().or_else(|| {
        let legacy = socket_path().parent().map(|d| d.join("settings.json"))?;
        std::fs::read(legacy).ok()
    });
    b.and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| json!({}))
}

/// `speed_limit` setting in KiB/s (0/absent = unlimited), as bytes/s.
pub(crate) fn speed_limit_bytes() -> Option<u64> {
    match settings_json()["speed_limit"].as_u64().unwrap_or(0) {
        0 => None,
        k => Some(k.saturating_mul(1024)),
    }
}

/// Whether finished downloads should get a SHA-256 checksum recorded.
pub(crate) fn sha256_on() -> bool {
    settings_json()["sha256"].as_bool().unwrap_or(false)
}

/// Active DoH resolver from settings. Defaults to Cloudflare (1.1.1.1) to avoid
/// ISP poisoning (NXDOMAIN) on common stream/embed hosts.
pub fn dns_resolver_from_settings() -> Option<std::sync::Arc<rdm_core::DohResolver>> {
    let s = settings_json();
    let mode = s
        .get("dns_mode")
        .and_then(|v| v.as_str())
        .unwrap_or("cloudflare");
    let custom = s.get("dns_custom").and_then(|v| v.as_str());
    let cfg = rdm_core::DnsConfig::from_settings(mode, custom);
    cfg.endpoint()
        .map(|ep| std::sync::Arc::new(rdm_core::DohResolver::new(ep)))
}

pub(crate) fn default_out_dir() -> PathBuf {
    let mut p = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    p.push("Downloads");
    if p.is_dir() {
        return p;
    }
    // recreate the standard folder instead of dumping files into the cwd
    if std::fs::create_dir_all(&p).is_ok() {
        p
    } else {
        PathBuf::from(".")
    }
}

pub(crate) fn expand_path(p: &str) -> PathBuf {
    let p = p.trim();
    let home = || {
        std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."))
    };
    if let Some(rest) = p.strip_prefix("~/") {
        return home().join(rest);
    }
    if let Some(rest) = p.strip_prefix("$HOME/") {
        return home().join(rest);
    }
    if p == "~" || p == "$HOME" {
        return home();
    }
    PathBuf::from(p)
}

/// Base folder: the `dir` setting, else ~/Downloads.
pub(crate) fn base_dir(settings: &serde_json::Value) -> PathBuf {
    settings["dir"]
        .as_str()
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(expand_path)
        .unwrap_or_else(default_out_dir)
}

/// Destination folder for a job: explicit `offer.dir` wins, then the
/// per-category folder from settings, then the base folder.
pub(crate) fn dir_for(offer_dir: Option<&str>, filename: &str) -> PathBuf {
    if let Some(d) = offer_dir.map(str::trim).filter(|d| !d.is_empty()) {
        return expand_path(d);
    }
    let s = settings_json();
    let base = base_dir(&s);
    let cat = rdm_core::category_of(filename);
    let Some(p) = s["categories"][cat]
        .as_str()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    else {
        return base;
    };
    let p = expand_path(p);
    if p.is_absolute() {
        p
    } else {
        base.join(p)
    }
}

/// Keep yt-dlp fresh: extractors go stale within weeks. Runs `yt-dlp -U` in
/// the background (once a day, first check shortly after start). System
/// packages are not writable, so `-U` just logs a hint there.
pub(crate) async fn ytdlp_updater() {
    tokio::time::sleep(std::time::Duration::from_secs(45)).await;
    loop {
        if settings_json()["ytdlp_update"].as_bool().unwrap_or(true) {
            update_ytdlp().await;
        }
        tokio::time::sleep(std::time::Duration::from_secs(24 * 3600)).await;
    }
}

async fn update_ytdlp() {
    let bin = crate::ytdlp::yt_dlp_bin();
    let version = |bin: std::path::PathBuf| async move {
        tokio::process::Command::new(bin)
            .arg("--version")
            .output()
            .await
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    };
    let before = version(bin.clone()).await;
    match tokio::process::Command::new(&bin).arg("-U").output().await {
        Ok(o) if o.status.success() => {
            let after = version(bin).await;
            if after.is_empty() || after == before {
                tracing::info!("yt-dlp {before}: already up to date");
            } else {
                tracing::info!("yt-dlp updated: {before} -> {after}");
            }
        }
        Ok(o) => {
            let msg = String::from_utf8_lossy(&o.stderr);
            let first = msg.trim().lines().next().unwrap_or("no output");
            tracing::info!("yt-dlp {before}: self-update unavailable ({first})");
        }
        Err(e) => tracing::warn!("yt-dlp self-update failed: {e}"),
    }
}
