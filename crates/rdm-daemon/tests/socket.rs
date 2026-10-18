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
        .with_state(data);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/f.bin")
}

async fn handler(State(data): State<Arc<Vec<u8>>>, headers: HeaderMap) -> Response {
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok()).map(str::to_string);
    if let Some(r) = range {
        let spec = r.trim_start_matches("bytes=");
        let (a, b) = spec.split_once('-').unwrap();
        let start: usize = a.parse().unwrap();
        let end = if b.is_empty() { data.len() - 1 } else { b.parse().unwrap() };
        let slice = data[start..=end].to_vec();
        return Response::builder()
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::ETAG, "\"v1\"")
            .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{}", data.len()))
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

#[tokio::test]
async fn offer_via_socket_downloads_file() {
    let url = serve().await;
    let dir = std::env::temp_dir().join(format!("rdm-daemon-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sock = dir.join("rdm.sock");
    let db = dir.join("rdm.db");
    std::env::set_var("RDM_DB", &db);

    tokio::spawn(rdm_daemon::run(sock.clone()));

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
    assert!(got.iter().enumerate().all(|(i, b)| *b == (i * 17 + 3) as u8));
    assert!(!dir.join("hasil.bin.rdm-part.json").exists());
}
