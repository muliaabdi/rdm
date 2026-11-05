//! Thin daemon-socket client: JSON-lines RPC over the unix socket.
//!
//! The daemon greets every client with a `settings` push and broadcasts
//! job events, so replies are matched by type, never by order.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// Split raw multi-line input (stdin batch or argv) into clean URLs:
/// trims, drops empty lines and `#` comments, keeps http(s) only,
/// dedupes preserving order.
pub fn parse_urls(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if !line.starts_with("http://") && !line.starts_with("https://") {
            continue;
        }
        if !out.iter().any(|u| u == line) {
            out.push(line.to_string());
        }
    }
    out
}

fn path() -> PathBuf {
    rdm_daemon::socket_path()
}

/// Send one request line; read lines until a reply of a wanted type
/// arrives (skipping broadcast events), or until `timeout_ms` of silence.
/// Returns the matched reply, or None when the daemon stayed silent
/// (fire-and-forget actions such as `pause` reply nothing themselves).
pub async fn rpc_wait(line: &str, want: &[&str], timeout_ms: u64) -> Result<Option<Value>> {
    let mut stream = UnixStream::connect(path())
        .await
        .with_context(|| format!("connect {}", path().display()))?;
    stream.write_all(line.as_bytes()).await?;
    stream.write_all(b"\n").await?;
    let (r, _w) = stream.into_split();
    let mut lines = BufReader::new(r).lines();
    loop {
        let line = tokio::time::timeout(
            std::time::Duration::from_millis(timeout_ms),
            lines.next_line(),
        )
        .await
        .context("rpc timeout")?
        .context("socket closed")?;
        let Some(v) = line.and_then(|l| serde_json::from_str::<Value>(&l).ok()) else {
            continue;
        };
        if let Some(t) = v["t"].as_str() {
            if want.contains(&t) {
                return Ok(Some(v));
            }
        }
    }
}

/// Offer one URL; returns the accepted job id.
pub async fn add_url(url: &str, dir: Option<&str>, segments: Option<u64>) -> Result<u64> {
    let mut offer = json!({"t":"offer","url":url});
    if let Some(d) = dir {
        offer["dir"] = json!(d);
    }
    if let Some(s) = segments {
        offer["segments"] = json!(s);
    }
    let reply = rpc_wait(&offer.to_string(), &["accepted", "error"], 10_000)
        .await?
        .context("no accepted reply")?;
    if reply["t"] == "error" {
        anyhow::bail!("{}", reply["error"].as_str().unwrap_or("unknown error"));
    }
    reply["id"].as_u64().context("bad id")
}

/// Current job snapshot (`{"t":"jobs"}`).
pub async fn list() -> Result<Value> {
    let reply = rpc_wait(r#"{"t":"list"}"#, &["jobs"], 10_000)
        .await?
        .context("no jobs reply")?;
    Ok(reply["jobs"].clone())
}

/// Fire-and-forget job action (pause/resume/restart/cancel/delete).
pub async fn job_action(action: &str, id: u64) -> Result<()> {
    let req = json!({"t":action,"id":id}).to_string();
    // actions acknowledge via broadcast status events; silence = fine
    let want: &[&str] = match action {
        "delete" => &["deleted"],
        _ => &["status"],
    };
    match rpc_wait(&req, want, 2_000).await {
        Ok(_) => Ok(()),
        Err(e) => {
            // timeout after a clean send is success for fire-and-forget;
            // only connection-level failures propagate
            let msg = format!("{e:#}");
            if msg.contains("rpc timeout") {
                Ok(())
            } else {
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_urls_trims_filters_and_dedupes() {
        let inp = "  https://a.example/f1  \n\nhttp://b.example/f2\n# comment\nftp://skip.me\nhttps://a.example/f1\nnot a url\n";
        assert_eq!(
            parse_urls(inp),
            vec![
                "https://a.example/f1".to_string(),
                "http://b.example/f2".to_string()
            ]
        );
    }

    #[test]
    fn parse_urls_empty_and_comments_only() {
        assert!(parse_urls("").is_empty());
        assert!(parse_urls("# just a comment\n\n   \n").is_empty());
    }

    #[test]
    fn parse_urls_dedupes_across_whitespace_variants() {
        assert_eq!(parse_urls("https://x\nhttps://x\n"), vec!["https://x"]);
    }

    #[tokio::test]
    async fn offer_and_list_roundtrip_via_daemon() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("rdm.sock");
        let db = tmp.path().join("rdm.db");
        tokio::spawn(rdm_daemon::run_at(sock.clone(), db));
        let mut up = false;
        for _ in 0..100 {
            if sock.exists() {
                up = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(up, "daemon socket never appeared");

        // point the client at the test socket
        std::env::set_var("XDG_RUNTIME_DIR", tmp.path());
        let id = add_url("https://example.invalid/x.bin", None, None)
            .await
            .expect("offer accepted");
        assert!(id > 0);
        let jobs = list().await.expect("list");
        let arr = jobs.as_array().expect("jobs array");
        assert!(arr.iter().any(|j| j["id"].as_u64() == Some(id)));
    }
}
