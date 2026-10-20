//! Integration: offer via daemon socket -> engine runs -> done event + file.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::Response;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

const SIZE: usize = 2 * 1024 * 1024;

fn test_data() -> Arc<Vec<u8>> {
    use std::sync::OnceLock;
    static D: OnceLock<Arc<Vec<u8>>> = OnceLock::new();
    D.get_or_init(|| {
        let mut v = vec![0u8; SIZE];
        for (i, b) in v.iter_mut().enumerate() {
            *b = (i * 17 + 3) as u8;
        }
        Arc::new(v)
    })
    .clone()
}

async fn serve() -> String {
    let data = test_data();
    let app = axum::Router::new()
        .route("/f.bin", axum::routing::get(handler))
        .route("/slow.bin", axum::routing::get(handler_slow))
        .with_state(data);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/f.bin")
}

async fn handler(State(data): State<Arc<Vec<u8>>>, headers: HeaderMap) -> Response {
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if let Some(r) = range {
        let spec = r.trim_start_matches("bytes=");
        let (a, b) = spec.split_once('-').unwrap();
        let start: usize = a.parse().unwrap();
        let end = if b.is_empty() {
            data.len() - 1
        } else {
            b.parse().unwrap()
        };
        let slice = data[start..=end].to_vec();
        return Response::builder()
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::ETAG, "\"v1\"")
            .header(
                header::CONTENT_RANGE,
                format!("bytes {start}-{end}/{}", data.len()),
            )
            .header(header::CONTENT_LENGTH, slice.len())
            .body(Body::from(slice))
            .unwrap();
    }
    Response::builder()
        .header(header::ETAG, "\"v1\"")
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, data.len())
        .body(Body::from(data.to_vec()))
        .unwrap()
}

// throttled handler: ~16 KB per 6 ms so a pause can land mid-download
async fn handler_slow(State(data): State<Arc<Vec<u8>>>, headers: HeaderMap) -> Response {
    use futures_util::stream::iter;
    use futures_util::StreamExt;
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let (mut start, mut end) = (0usize, data.len() - 1);
    if let Some(r) = range {
        let spec = r.trim_start_matches("bytes=");
        let (a, b) = spec.split_once('-').unwrap();
        start = a.parse().unwrap();
        if !b.is_empty() {
            end = b.parse().unwrap();
        }
    }
    let mut chunks: Vec<Result<Vec<u8>, std::io::Error>> = Vec::new();
    let mut i = start;
    while i <= end {
        let hi = (i + 16 * 1024).min(end + 1);
        chunks.push(Ok(data[i..hi].to_vec()));
        i = hi;
    }
    let stream = iter(chunks).then(|c| async move {
        tokio::time::sleep(std::time::Duration::from_millis(6)).await;
        c
    });
    Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header(header::ETAG, "\"v1\"")
        .header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{}", data.len()),
        )
        .body(Body::from_stream(stream))
        .unwrap()
}

#[tokio::test]
async fn pause_resume_roundtrip() {
    let url = serve().await.replace("f.bin", "slow.bin");
    let dir = std::env::temp_dir().join(format!("rdm-pause-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sock = dir.join("rdm.sock");
    let db = dir.join("rdm.db");
    tokio::spawn(rdm_daemon::run_at(sock.clone(), db));

    let mut stream = None;
    for _ in 0..100 {
        if let Ok(s) = UnixStream::connect(&sock).await {
            stream = Some(s);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let stream = stream.expect("daemon socket");
    let (r, mut w) = stream.into_split();

    async fn send(w: &mut tokio::net::unix::OwnedWriteHalf, v: serde_json::Value) {
        w.write_all(v.to_string().as_bytes()).await.unwrap();
        w.write_all(b"\n").await.unwrap();
    }
    send(
        &mut w,
        serde_json::json!({
            "t":"offer","url":url,"filename":"pause.bin","segments":2,
            "dir": dir.to_str().unwrap()
        }),
    )
    .await;

    let mut lines = BufReader::new(r).lines();
    let mut id = 0u64;
    let mut paused = false;
    let mut resumed_done = false;
    let mut expect = "pauseable";
    loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(30), lines.next_line())
            .await
            .expect("timeout")
            .expect("closed")
            .expect("io");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        match v["t"].as_str() {
            Some("accepted") => id = v["id"].as_u64().unwrap(),
            Some("progress") if expect == "pauseable" => {
                if v["done"].as_u64().unwrap_or(0) > 64 * 1024 {
                    send(&mut w, serde_json::json!({"t":"pause","id":id})).await;
                    expect = "paused";
                }
            }
            Some("paused") => {
                paused = true;
                assert!(v["done"].as_u64().unwrap() < 2 * 1024 * 1024);
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                send(&mut w, serde_json::json!({"t":"resume","id":id})).await;
                expect = "done";
            }
            Some("done") => {
                assert_eq!(v["size"], serde_json::json!(SIZE as u64));
                resumed_done = true;
                break;
            }
            Some("failed") | Some("cancelled") | Some("error") => panic!("daemon: {line}"),
            _ => {}
        }
    }
    assert!(paused, "pause event tidak diterima");
    assert!(resumed_done, "resume tidak selesai");
    let got = std::fs::read(dir.join("pause.bin")).unwrap();
    assert_eq!(got.len(), SIZE);
    assert!(got
        .iter()
        .enumerate()
        .all(|(i, b)| *b == (i * 17 + 3) as u8));
    assert!(!dir.join("pause.bin.rdm-part.json").exists());
}

#[tokio::test]
async fn offer_via_socket_downloads_file() {
    let url = serve().await;
    let dir = std::env::temp_dir().join(format!("rdm-daemon-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sock = dir.join("rdm.sock");
    let db = dir.join("rdm.db");
    tokio::spawn(rdm_daemon::run_at(sock.clone(), db));

    // connect once socket appears
    let mut stream = None;
    for _ in 0..100 {
        if let Ok(s) = UnixStream::connect(&sock).await {
            stream = Some(s);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let stream = stream.expect("daemon socket");

    let (r, mut w) = stream.into_split();
    let offer = serde_json::json!({
        "t": "offer",
        "url": url,
        "filename": "hasil.bin",
        "referer": "http://example.com/page",
        "cookie": "session=abc",
        "ua": "rdm-test/0.1",
        "segments": 4,
        "dir": dir.to_str().unwrap(),
    });
    w.write_all(offer.to_string().as_bytes()).await.unwrap();
    w.write_all(b"\n").await.unwrap();

    let mut lines = BufReader::new(r).lines();
    let mut got_done = false;
    let mut got_accepted = false;
    loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(30), lines.next_line())
            .await
            .expect("timeout waiting daemon event")
            .expect("stream closed")
            .expect("read error");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        match v["t"].as_str() {
            Some("accepted") => got_accepted = true,
            Some("started") => assert!(got_accepted),
            Some("progress") => {}
            Some("done") => {
                assert_eq!(v["size"], serde_json::json!(SIZE as u64));
                got_done = true;
                break;
            }
            Some("failed") | Some("cancelled") | Some("error") => panic!("daemon: {line}"),
            _ => {}
        }
    }
    assert!(got_done);

    let got = std::fs::read(dir.join("hasil.bin")).unwrap();
    assert_eq!(got.len(), SIZE);
    assert!(got
        .iter()
        .enumerate()
        .all(|(i, b)| *b == (i * 17 + 3) as u8));
    assert!(!dir.join("hasil.bin.rdm-part.json").exists());
}
