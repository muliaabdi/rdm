//! rdm — CLI client for the rdm-daemon socket.
//!
//! `rdm add <url> | rdm add - < urls.txt` (batch: one URL per line,
//! `#` comments allowed) | `rdm list` | `rdm pause|resume|restart|delete <id>`.

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};

mod client;

#[derive(Parser)]
#[command(name = "rdm", version, about = "Rust Download Manager — daemon client")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Add URLs (one job each). `-` reads URLs from stdin, one per line.
    Add {
        /// URL, or `-` to read URLs from stdin (`#` lines are comments).
        url: String,
        /// Output directory.
        #[arg(short, long)]
        out: Option<String>,
        /// Max parallel segments per job.
        #[arg(short, long)]
        segments: Option<u64>,
    },
    /// List jobs (newest first).
    List,
    /// Pause a job.
    Pause { id: u64 },
    /// Resume a paused/queued job.
    Resume { id: u64 },
    /// Restart a job from scratch.
    Restart { id: u64 },
    /// Delete a job (and its files).
    Delete {
        id: u64,
        /// Keep partial files on disk.
        #[arg(long)]
        keep_files: bool,
    },
}

// println! panics on EPIPE (e.g. `rdm list | head`); ignore closed pipes
fn pline(s: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stdout(), "{s}");
}

fn fmt_size(v: Option<u64>) -> String {
    match v {
        None => "?".into(),
        Some(b) => {
            let units = ["B", "KiB", "MiB", "GiB", "TiB"];
            let mut val = b as f64;
            let mut u = 0;
            while val >= 1024.0 && u < units.len() - 1 {
                val /= 1024.0;
                u += 1;
            }
            if u == 0 {
                format!("{b} B")
            } else {
                format!("{val:.1} {u}", u = units[u])
            }
        }
    }
}

fn print_jobs(jobs: &serde_json::Value) -> Result<()> {
    let Some(arr) = jobs.as_array() else {
        bail!("bad jobs payload");
    };
    if arr.is_empty() {
        pline("no jobs");
        return Ok(());
    }
    for j in arr {
        let id = j["id"].as_u64().unwrap_or(0);
        let name = j["filename"].as_str().unwrap_or("?");
        let status = j["status"].as_str().unwrap_or("?");
        let done = j["done"].as_u64().unwrap_or(0);
        let size = j["size"].as_u64();
        let queue = j["queue"].as_str().unwrap_or("main");
        let url = j["url"].as_str().unwrap_or("");
        pline(&format!(
            "{id:>4}  [{status:<10}] {name}  {d}/{s}",
            d = fmt_size(Some(done)),
            s = fmt_size(size)
        ));
        pline(&format!("      queue={queue}  {url}"));
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Add { url, out, segments } => {
            let text = if url == "-" {
                use std::io::Read;
                let mut buf = String::new();
                std::io::stdin().read_to_string(&mut buf)?;
                buf
            } else {
                url
            };
            let urls = client::parse_urls(&text);
            if urls.is_empty() {
                bail!("no http(s) URLs found");
            }
            for u in &urls {
                match client::add_url(u, out.as_deref(), segments).await {
                    Ok(id) => pline(&format!("added {id}: {u}")),
                    Err(e) => eprintln!("failed {u}: {e:#}"),
                }
            }
        }
        Cmd::List => {
            print_jobs(&client::list().await?)?;
        }
        Cmd::Pause { id } => client::job_action("pause", id).await?,
        Cmd::Resume { id } => client::job_action("resume", id).await?,
        Cmd::Restart { id } => client::job_action("restart", id).await?,
        Cmd::Delete { id, keep_files } => {
            let req = serde_json::json!({"t":"delete","id":id,"files":!keep_files}).to_string();
            if let Some(r) = client::rpc_wait(&req, &["deleted", "error"], 5_000).await? {
                if r["t"] == "error" {
                    eprintln!("{}", r["error"].as_str().unwrap_or("error"));
                }
            }
        }
    }
    Ok(())
}
