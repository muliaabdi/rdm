//! Integration tests: segmented download + cancel/resume against a local
//! Range-capable HTTP server.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::Response;
use rdm_core::{download, job_from_url, Limits, Progress};
use tokio::sync::{mpsc, watch};

const SIZE: usize = 4 * 1024 * 1024;

fn test_data() -> Arc<Vec<u8>> {
    use std::sync::OnceLock;
    static D: OnceLock<Arc<Vec<u8>>> = OnceLock::new();
    D.get_or_init(|| {
        let mut v = vec![0u8; SIZE];
        for (i, b) in v.iter_mut().enumerate() {
            *b = (i * 31 + 7) as u8;
        }
        Arc::new(v)
    })
    .clone()
}

async fn serve() -> String {
    let data = test_data();
    let app = axum::Router::new()
        .route("/file.bin", axum::routing::get(handler))
        .with_state(data);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/file.bin")
}

async fn handler(State(data): State<Arc<Vec<u8>>>, headers: HeaderMap) -> Response {
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok()).map(str::to_string);
    let mut resp = Response::builder().header(header::ETAG, "\"v1\"");
    if let Some(r) = range {
        let spec = r.trim_start_matches("bytes=");
        let (a, b) = spec.split_once('-').unwrap();
        let start: usize = a.parse().unwrap();
        let end = if b.is_empty() { data.len() - 1 } else { b.parse().unwrap() };
        if start >= data.len() {
            return Response::builder().status(StatusCode::RANGE_NOT_SATISFIABLE).body(Body::empty()).unwrap();
        }
        let end = end.min(data.len() - 1);
        let slice = data[start..=end].to_vec();
        resp = resp
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{}", data.len()))
            .header(header::CONTENT_LENGTH, slice.len());
        return resp.body(Body::from(slice)).unwrap();
    }
    let body = data.clone();
    resp = resp
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, body.len());
    resp.body(Body::from(body.to_vec())).unwrap()
}

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rdm-test-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[tokio::test]
async fn segmented_download_matches_source() {
    let url = serve().await;
    let dir = tmpdir("full");
    let job = job_from_url(&url, Some(dir.clone()), 4, Limits { sha256: true, ..Default::default() });
    let (tx, mut rx) = mpsc::channel(4096);
    let (_kx, kx) = watch::channel(false);
    let handle = tokio::spawn(download(job, tx, kx));
    let mut started = None;
    let mut done = None;
    while let Some(p) = rx.recv().await {
        match p {
            Progress::Started { size, segments, .. } => started = Some((size, segments)),
            Progress::Completed { .. } => done = Some(p),
            _ => {}
        }
    }
    handle.await.unwrap().unwrap();
    let (size, segments) = started.unwrap();
    assert_eq!(size, Some(SIZE as u64));
    assert_eq!(segments, 4);
    assert!(done.is_some());

    let got = std::fs::read(dir.join("file.bin")).unwrap();
    assert_eq!(got.len(), SIZE);
    assert!(got.iter().enumerate().all(|(i, b)| *b == (i * 31 + 7) as u8));
}

#[tokio::test]
async fn cancel_then_resume_completes() {
    let url = serve().await;
    let dir = tmpdir("resume");
    let out = dir.join("file.bin");
    let spath = {
        let mut s = out.as_os_str().to_os_string();
        s.push(".rdm-part.json");
        PathBuf::from(s)
    };

    // First pass: cancel after ≥ 64 KiB progress.
    {
        let job = job_from_url(&url, Some(dir.clone()), 4, Limits::default());
        let (tx, mut rx) = mpsc::channel::<Progress>(4096);
        let (kx_tx, kx_rx) = watch::channel(false);
        let handle = tokio::spawn(download(job, tx, kx_rx));
        while let Some(p) = rx.recv().await {
            if let Progress::Bytes { total_done, .. } = p {
                if total_done > 128 * 1024 {
                    kx_tx.send(true).ok();
                    break;
                }
            }
        }
        handle.await.unwrap().unwrap(); // cancel path returns Ok
        assert!(spath.exists(), "state file must exist after cancel");
        let st: rdm_core::state::JobState =
            serde_json::from_slice(&std::fs::read(&spath).unwrap()).unwrap();
        assert!(st.total_done() < SIZE as u64);
        assert!(st.total_done() > 0);
    }

    // Second pass: resume from state, run to completion.
    {
        let job = job_from_url(&url, Some(dir.clone()), 4, Limits::default());
        let (tx, mut rx) = mpsc::channel::<Progress>(4096);
        let (_kx, kx) = watch::channel(false);
        let handle = tokio::spawn(download(job, tx, kx));
        let mut cancelled_or_failed = false;
        while let Some(p) = rx.recv().await {
            if matches!(p, Progress::Cancelled { .. } | Progress::Failed { .. }) {
                cancelled_or_failed = true;
            }
        }
        handle.await.unwrap().unwrap();
        assert!(!cancelled_or_failed);
        assert!(!spath.exists(), "state file removed after completion");
        let got = std::fs::read(&out).unwrap();
        assert_eq!(got.len(), SIZE);
        assert!(got.iter().enumerate().all(|(i, b)| *b == (i * 31 + 7) as u8));
    }
}
