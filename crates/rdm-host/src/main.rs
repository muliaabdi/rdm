//! Native messaging host: Chrome framed stdio <-> rdm-daemon JSON-lines socket.
//!
//! Protocol in (stdin):  [u32 LE len][JSON bytes]
//! Protocol out (stdout): same framing, daemon lines forwarded verbatim.

use std::io::{Read, Write};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

fn write_frame(bytes: &[u8]) -> std::io::Result<()> {
    let mut out = std::io::stdout().lock();
    out.write_all(&(bytes.len() as u32).to_le_bytes())?;
    out.write_all(bytes)?;
    out.flush()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let path = rdm_daemon::socket_path();
    let sock = UnixStream::connect(&path).await?;
    let (r, w) = sock.into_split();

    // socket -> stdout (framed)
    let pump_out = tokio::spawn(async move {
        let mut reader = BufReader::new(r);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if write_frame(line.trim_end().as_bytes()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    // stdin (framed, blocking) -> socket
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(64);
    let pump_in = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let mut stdin = std::io::stdin().lock();
        loop {
            let mut len = [0u8; 4];
            if stdin.read_exact(&mut len).is_err() {
                break;
            }
            let n = u32::from_le_bytes(len) as usize;
            if n == 0 || n > 1024 * 1024 {
                break;
            }
            let mut buf = vec![0u8; n];
            stdin.read_exact(&mut buf)?;
            if tx.blocking_send(buf).is_err() {
                break;
            }
        }
        Ok(())
    });

    let mut w = w;
    let mut writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if w.write_all(&msg).await.is_err() || w.write_all(b"\n").await.is_err() {
                let _ = w.shutdown().await;
                return;
            }
        }
        let _ = w.shutdown().await;
    });

    tokio::select! {
        _ = pump_out => {},
        _ = pump_in => {},
        _ = &mut writer => {},
        _ = tokio::time::sleep(Duration::from_secs(24 * 3600)) => {},
    }
    Ok(())
}
