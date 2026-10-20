use anyhow::Result;
use clap::{Parser, Subcommand};
use std::time::Duration;
use tokio::sync::{mpsc, watch};

use rdm_core::{download, job_from_url, Job, Limits, Progress};

#[derive(Parser)]
#[command(name = "rdm", version, about = "Rust Download Manager (M0 engine)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Add and run a download.
    Add {
        url: String,
        /// Output directory.
        #[arg(short, long)]
        out: Option<std::path::PathBuf>,
        /// Max parallel segments.
        #[arg(short, long, default_value_t = 8)]
        segments: u64,
        /// Speed limit in KiB/s.
        #[arg(long)]
        speed_kib: Option<u64>,
        /// Verify sha256 after completion.
        #[arg(long)]
        sha256: bool,
        /// Retries per segment.
        #[arg(long, default_value_t = 5)]
        retries: u32,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RDM_LOG").unwrap_or_else(|_| "warn".into()))
        .init();
    let cli = Cli::parse();
    let Cmd::Add {
        url,
        out,
        segments,
        speed_kib,
        sha256,
        retries,
    } = cli.cmd;

    let limits = Limits {
        max_speed: speed_kib.map(|k| k * 1024),
        retries,
        min_segment_size: rdm_core::Limits::default().min_segment_size,
        sha256,
    };
    let job: Job = job_from_url(&url, out, segments, limits);

    let (ev_tx, mut ev_rx) = mpsc::channel::<Progress>(1024);
    let (kill_tx, kill_rx) = watch::channel(false);

    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            kill_tx.send(true).ok();
        }
    });

    let printer = tokio::spawn(async move {
        let mut last = std::time::Instant::now();
        while let Some(p) = ev_rx.recv().await {
            match &p {
                Progress::Bytes { .. } => {
                    if last.elapsed() >= Duration::from_millis(200) {
                        last = std::time::Instant::now();
                        eprint!("{}", p.brief());
                    }
                }
                _ => eprintln!("{}", p.brief()),
            }
        }
    });

    let res = download(job, ev_tx, kill_rx).await;
    printer.await.ok();
    res
}
