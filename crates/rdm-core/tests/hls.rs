//! Integration tests: generic HLS (m3u8) download against a local server.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use rdm_core::{download, job_from_url, Limits, Progress};
use tokio::sync::{mpsc, watch};

const SEG: usize = 64 * 1024;
const NSEG: usize = 12;

fn seg_bytes(i: usize) -> Vec<u8> {
    (0..SEG).map(|k| ((i * 7 + k / 256) % 251) as u8).collect()
}

fn full() -> Vec<u8> {
    (0..NSEG).flat_map(seg_bytes).collect()
}

#[derive(Clone)]
struct St(Arc<Vec<Vec<u8>>>);

async fn playlist() -> Response {
    let mut body = String::from("#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:4\n");
    for i in 0..NSEG {
        body.push_str(&format!("#EXTINF:4.0,\nseg{i:03}.ts\n"));
    }
    body.push_str("#EXT-X-ENDLIST\n");
    (
        [(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")],
        body,
    )
        .into_response()
}

async fn segment(State(st): State<St>, Path(i): Path<String>) -> Response {
    let idx: usize = i.trim_start_matches("seg").trim_end_matches(".ts").parse().unwrap();
    match st.0.get(idx) {
        Some(b) => ([(header::CONTENT_TYPE, "video/mp2t")], b.clone()).into_response(),
        None => Response::builder().status(StatusCode::NOT_FOUND).body(Body::empty()).unwrap(),
    }
}

async fn serve() -> String {
    let st = St(Arc::new((0..NSEG).map(seg_bytes).collect()));
    let app = axum::Router::new()
        .route("/vod.m3u8", axum::routing::get(playlist))
        .route("/{i}", axum::routing::get(segment))
        .with_state(st);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/vod.m3u8")
}

async fn tmp_out(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rdm-hls-test-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// True when the job was cancelled mid-way (state file present, no full file).
fn app_state_incomplete(dir: &std::path::Path, out: &std::path::Path) -> bool {
    dir.join("vod.ts.rdm-hls.json").exists()
        && std::fs::metadata(out).map(|m| m.len() < full().len() as u64).unwrap_or(true)
}

#[tokio::test]
async fn hls_download_matches_concat() {
    let base = serve().await;
    let dir = tmp_out("full").await;
    let job = job_from_url(&base, Some(dir.clone()), 4, Limits { retries: 2, ..Default::default() });
    let (tx, mut rx) = mpsc::channel(256);
    let kill = watch::channel(false).1;
    tokio::spawn(download(job, tx, kill));

    let mut completed = false;
    while let Some(p) = rx.recv().await {
        match p {
            Progress::Completed { .. } => completed = true,
            Progress::Failed { error } => panic!("hls failed: {error}"),
            _ => {}
        }
    }
    assert!(completed, "no Completed event");
    let out = dir.join("vod.ts"); // .m3u8 renamed to .ts
    assert!(out.exists(), "output {:?} missing", out);
    assert_eq!(std::fs::read(&out).unwrap(), full(), "concatenated segments mismatch");
    // state file cleaned up on completion
    assert!(!dir.join("vod.ts.rdm-hls.json").exists());
}

#[tokio::test]
async fn hls_resume_finishes_after_cancel() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static SLOW: AtomicBool = AtomicBool::new(false);

    let st = St(Arc::new((0..NSEG).map(seg_bytes).collect()));
    let app = axum::Router::new()
        .route("/vod.m3u8", axum::routing::get(playlist))
        .route("/{i}", axum::routing::get(|State(st): State<St>, Path(i): Path<String>| async move {
            if SLOW.load(Ordering::Relaxed) {
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            }
            segment(State(st), Path(i)).await
        }))
        .with_state(st);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let base = format!("http://{addr}/vod.m3u8");

    let dir = tmp_out("resume").await;
    let out = dir.join("vod.ts");

    // run 1: slow segments, cancel once the first few landed
    {
        SLOW.store(true, Ordering::Relaxed);
        let job = job_from_url(&base, Some(dir.clone()), 4, Limits { retries: 1, ..Default::default() });
        let (tx, mut rx) = mpsc::channel(256);
        let (ktx, kill) = watch::channel(false);
        let h = tokio::spawn(download(job, tx, kill));
        let mut got_bytes = 0;
        while let Some(p) = rx.recv().await {
            if let Progress::Bytes { delta, .. } = p {
                got_bytes += delta;
                if got_bytes >= SEG as u64 * 4 {
                    ktx.send(true).ok();
                }
            }
        }
        h.await.unwrap().unwrap();
        assert!(app_state_incomplete(&dir, &out), "run 1 should not have completed");
        SLOW.store(false, Ordering::Relaxed);
    }

    // run 2: resume, must complete and match
    {
        let job = job_from_url(&base, Some(dir.clone()), 4, Limits { retries: 2, ..Default::default() });
        let (tx, mut rx) = mpsc::channel(256);
        let kill = watch::channel(false).1;
        tokio::spawn(download(job, tx, kill));
        let mut completed = false;
        while let Some(p) = rx.recv().await {
            match p {
                Progress::Completed { .. } => completed = true,
                Progress::Failed { error } => panic!("resume failed: {error}"),
                _ => {}
            }
        }
        assert!(completed);
    }
    assert_eq!(std::fs::read(&out).unwrap(), full(), "resumed output mismatch");
}
