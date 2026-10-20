//! Integration tests: advanced HLS — AES-128 (explicit + sequence-derived
//! IV), multiple fMP4 init segments, and live (growing) playlists.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use rdm_core::{download, job_from_url, Limits, Progress};
use tokio::sync::{mpsc, watch};

const SEG: usize = 32 * 1024;
const NSEG: usize = 8;

fn seg_bytes(i: usize) -> Vec<u8> {
    (0..SEG).map(|k| ((i * 13 + k / 512) % 247) as u8).collect()
}

fn aes_enc(key: &[u8; 16], iv: &[u8; 16], data: &[u8]) -> Vec<u8> {
    use cbc::cipher::{BlockEncryptMut, KeyIvInit};
    type Enc = cbc::Encryptor<aes::Aes128>;
    Enc::new_from_slices(key, iv)
        .unwrap()
        .encrypt_padded_vec_mut::<cbc::cipher::block_padding::Pkcs7>(data)
}

fn seq_iv(seq: u64) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[8..].copy_from_slice(&seq.to_be_bytes());
    b
}

#[derive(Clone)]
struct St {
    segs: Arc<Vec<Vec<u8>>>,
    encrypted: bool,
    key: [u8; 16],
    explicit_iv: bool,
    media_seq: u64,
    multi_map: bool,
    // live: reveal more segments on each playlist fetch, ENDLIST at the end
    live: bool,
    polls: Arc<AtomicU64>,
}

async fn key_file(State(st): State<St>) -> Response {
    st.key.to_vec().into_response()
}

async fn playlist(State(st): State<St>) -> Response {
    let reveal = if st.live {
        let n = st.polls.fetch_add(1, Ordering::SeqCst) + 1;
        (n as usize * 3).min(NSEG)
    } else {
        NSEG
    };
    let mut b = String::from("#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:4\n");
    if st.live {
        b.push_str("#EXT-X-MEDIA-SEQUENCE:100\n");
    } else {
        b.push_str(&format!("#EXT-X-MEDIA-SEQUENCE:{}\n", st.media_seq));
    }
    if st.encrypted {
        let iv = if st.explicit_iv {
            ",IV=0x0000000000000000000000000000002a"
        } else {
            ""
        };
        b.push_str(&format!("#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"{iv}\n"));
    }
    if st.multi_map {
        b.push_str("#EXT-X-MAP:URI=\"init/0\"\n");
    }
    for i in 0..reveal {
        if st.multi_map && i == NSEG / 2 {
            b.push_str("#EXT-X-DISCONTINUITY\n#EXT-X-MAP:URI=\"init/1\"\n");
        }
        b.push_str(&format!("#EXTINF:4.0,\nseg/{i:03}\n"));
    }
    if !st.live || reveal == NSEG {
        b.push_str("#EXT-X-ENDLIST\n");
    }
    ([(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")], b).into_response()
}

async fn init_seg(State(st): State<St>, Path(i): Path<String>) -> Response {
    let idx: usize = i.parse().unwrap();
    let mut b = st.segs[0].clone();
    b[0] = idx as u8; // distinguish the two inits
    b.truncate(1024);
    b.into_response()
}

async fn segment(State(st): State<St>, Path(i): Path<String>) -> Response {
    let idx: usize = i.trim_start_matches("seg/").parse().unwrap();
    let plain = st.segs[idx].clone();
    let body = if st.encrypted {
        let iv = if st.explicit_iv {
            let mut v = [0u8; 16];
            v[15] = 0x2a;
            v
        } else {
            seq_iv(st.media_seq + idx as u64)
        };
        aes_enc(&st.key, &iv, &plain)
    } else {
        plain
    };
    ([(header::CONTENT_TYPE, "video/mp2t")], body).into_response()
}

async fn serve(st: St) -> String {
    let app = axum::Router::new()
        .route("/vod.m3u8", axum::routing::get(playlist))
        .route("/key.bin", axum::routing::get(key_file))
        .route("/init/{i}", axum::routing::get(init_seg))
        .route("/seg/{i}", axum::routing::get(segment))
        .with_state(st);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/vod.m3u8")
}

fn base_st() -> St {
    St {
        segs: Arc::new((0..NSEG).map(seg_bytes).collect()),
        encrypted: false,
        key: [0x11; 16],
        explicit_iv: false,
        media_seq: 0,
        multi_map: false,
        live: false,
        polls: Arc::new(AtomicU64::new(0)),
    }
}

async fn run(base: &str, name: &str, segs: u64) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rdm-hlsx-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let job = job_from_url(
        base,
        Some(dir.clone()),
        segs,
        Limits {
            retries: 2,
            ..Default::default()
        },
    );
    let (tx, mut rx) = mpsc::channel(256);
    let kill = watch::channel(false).1;
    tokio::spawn(download(job, tx, kill));
    while let Some(p) = rx.recv().await {
        match p {
            Progress::Completed { .. } => break,
            Progress::Failed { error } => panic!("hls failed: {error}"),
            _ => {}
        }
    }
    dir
}

fn full() -> Vec<u8> {
    (0..NSEG).flat_map(seg_bytes).collect()
}

fn read_out(dir: &std::path::Path) -> Vec<u8> {
    std::fs::read(dir.join("vod.ts")).unwrap()
}

#[tokio::test]
async fn aes128_iv_from_media_sequence() {
    let mut st = base_st();
    st.encrypted = true;
    st.media_seq = 7;
    let base = serve(st).await;
    let dir = run(&base, "aes-seq", 4).await;
    assert_eq!(read_out(&dir), full(), "decrypted concat mismatch");
}

#[tokio::test]
async fn aes128_explicit_iv() {
    let mut st = base_st();
    st.encrypted = true;
    st.explicit_iv = true;
    st.media_seq = 3;
    let base = serve(st).await;
    let dir = run(&base, "aes-iv", 4).await;
    assert_eq!(
        read_out(&dir),
        full(),
        "decrypted concat mismatch (explicit IV)"
    );
}

#[tokio::test]
async fn multi_map_writes_each_init_once() {
    let mut st = base_st();
    st.multi_map = true;
    let segs = st.segs.clone();
    let base = serve(st).await;
    let dir = run(&base, "multimap", 4).await;
    let out = read_out(&dir);
    let mut expect = Vec::new();
    let mut i0 = segs[0].clone();
    i0[0] = 0;
    i0.truncate(1024);
    let mut i1 = segs[0].clone();
    i1[0] = 1;
    i1.truncate(1024);
    expect.extend_from_slice(&i0);
    for k in 0..NSEG / 2 {
        expect.extend_from_slice(&segs[k]);
    }
    expect.extend_from_slice(&i1);
    for k in NSEG / 2..NSEG {
        expect.extend_from_slice(&segs[k]);
    }
    assert_eq!(out, expect, "init segments must open each stream group");
}

#[tokio::test]
async fn live_playlist_completes_after_endlist() {
    let mut st = base_st();
    st.live = true;
    st.media_seq = 100;
    let polls = st.polls.clone();
    let base = serve(st).await;
    let t0 = std::time::Instant::now();
    let dir = run(&base, "live", 4).await;
    assert_eq!(read_out(&dir), full(), "live capture mismatch");
    assert!(t0.elapsed().as_secs() >= 2, "must have polled, not spammed");
    assert!(
        polls.load(Ordering::SeqCst) >= 3,
        "must have re-fetched the playlist"
    );
}

#[tokio::test]
async fn live_pause_then_resume() {
    use std::sync::atomic::AtomicUsize;
    static SLOW: AtomicBool = AtomicBool::new(false);
    static SERVED: AtomicUsize = AtomicUsize::new(0);

    let mut st = base_st();
    st.live = true;
    st.media_seq = 100;
    let app = axum::Router::new()
        .route("/vod.m3u8", axum::routing::get(playlist))
        .route(
            "/seg/{i}",
            axum::routing::get(
                move |State(s): State<St>, Path(i): Path<String>| async move {
                    SERVED.fetch_add(1, Ordering::SeqCst);
                    if SLOW.load(Ordering::Relaxed) {
                        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
                    }
                    segment(State(s), Path(i)).await
                },
            ),
        )
        .with_state(st);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let base = format!("http://{addr}/vod.m3u8");

    let dir = std::env::temp_dir().join(format!("rdm-hlsx-{}-live-resume", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // run 1: slow segments, cancel after a couple land
    {
        SLOW.store(true, Ordering::Relaxed);
        let job = job_from_url(
            &base,
            Some(dir.clone()),
            4,
            Limits {
                retries: 1,
                ..Default::default()
            },
        );
        let (tx, mut rx) = mpsc::channel(256);
        let (ktx, kill) = watch::channel(false);
        let h = tokio::spawn(download(job, tx, kill));
        let mut got = 0;
        while let Some(p) = rx.recv().await {
            if let Progress::Bytes { delta, .. } = p {
                got += delta;
                if got >= SEG as u64 {
                    ktx.send(true).ok();
                }
            }
        }
        h.await.unwrap().unwrap();
        SLOW.store(false, Ordering::Relaxed);
        assert!(
            dir.join("vod.ts.rdm-hls.json").exists(),
            "live state must be saved on cancel"
        );
    }

    // run 2: resume; playlist keeps serving everything, so full capture
    {
        let job = job_from_url(
            &base,
            Some(dir.clone()),
            4,
            Limits {
                retries: 2,
                ..Default::default()
            },
        );
        let (tx, mut rx) = mpsc::channel(256);
        let kill = watch::channel(false).1;
        tokio::spawn(download(job, tx, kill));
        while let Some(p) = rx.recv().await {
            match p {
                Progress::Completed { .. } => break,
                Progress::Failed { error } => panic!("live resume failed: {error}"),
                _ => {}
            }
        }
    }
    assert_eq!(read_out(&dir), full(), "live resume capture mismatch");
}
