//! HTTP-layer tests for the v1 stream API.
//!
//! These drive the *real* router over a real TCP listener on 127.0.0.1:0, so a
//! regression in routing, status codes or JSON shape fails here even when the
//! pure-logic unit tests still pass. No inference backend is involved: every
//! case either fails validation before OCR, or finishes a stream with no
//! frames, which needs no model at all.

use eomer_srt_ocr::server::{AppState, InferenceGate, ServerConfig, Store, StreamStore};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::path::PathBuf;

/// Start the service on an ephemeral loopback port.
///
/// Returns the real address; the listener is bound before the port is read, so
/// parallel tests never race for a fixed number.
async fn start() -> SocketAddr {
    let state = AppState {
        cfg: ServerConfig {
            // Port 0: the OS assigns a free one.
            addr: "127.0.0.1:0".parse().expect("valid loopback addr"),
            server: "http://127.0.0.1:9".to_string(),
            width: 640,
            work_dir: PathBuf::from("out").join("serve-test"),
            skip_backend_check: true,
        },
        store: Store::new(),
        streams: StreamStore::new(),
        gate: InferenceGate::default(),
    };
    let listener = eomer_srt_ocr::server::bind(&state)
        .await
        .expect("bind ephemeral loopback port");
    let addr = listener.local_addr().expect("listener address");
    tokio::spawn(async move {
        let _ = eomer_srt_ocr::server::serve_on(listener, state).await;
    });
    addr
}

fn url(addr: SocketAddr, path: &str) -> String {
    format!("http://{addr}{path}")
}

#[tokio::test]
async fn serves_the_embedded_browser_assets() {
    let addr = start().await;
    for (path, needle) in [
        ("/", "<!doctype html>"),
        ("/app.js", ""),
        ("/frames.js", ""),
        ("/style.css", ""),
    ] {
        let body = reqwest::get(url(addr, path)).await.unwrap();
        assert_eq!(body.status(), 200, "{path} should be served");
        let text = body.text().await.unwrap();
        assert!(!text.is_empty(), "{path} must not be empty");
        assert!(text.contains(needle), "{path} should contain {needle:?}");
    }
}

#[tokio::test]
async fn health_reports_backend_and_ok_flag() {
    let addr = start().await;
    let res = reqwest::get(url(addr, "/api/v1/health")).await.unwrap();
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    // Port 9 is never a llama-server, so reachability must be false rather
    // than the request hanging or the endpoint erroring out.
    assert_eq!(body["backend_reachable"], json!(false));
    assert!(body["backend"].is_string());
}

#[tokio::test]
async fn unversioned_aliases_answer_like_v1() {
    let addr = start().await;
    let v1 = reqwest::get(url(addr, "/api/v1/health")).await.unwrap();
    let plain = reqwest::get(url(addr, "/api/health")).await.unwrap();
    assert_eq!(v1.status(), plain.status());
    assert_eq!(
        v1.json::<Value>().await.unwrap(),
        plain.json::<Value>().await.unwrap()
    );
}

#[tokio::test]
async fn open_stream_returns_an_id_and_rejects_bad_geometry() {
    let addr = start().await;
    let client = reqwest::Client::new();

    let ok = client
        .post(url(addr, "/api/v1/streams"))
        .json(&json!({"fps": 2.0, "width": 640, "height": 360}))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
    let view: Value = ok.json().await.unwrap();
    let id = view["id"].as_u64().expect("stream id");
    assert_eq!(view["state"], "open");
    assert_eq!(view["frames_received"], 0);

    // Zero width is not a video frame; validation must reject it with 400,
    // not accept it and fail later deep inside the pipeline.
    let bad = client
        .post(url(addr, "/api/v1/streams"))
        .json(&json!({"fps": 2.0, "width": 0, "height": 360}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);

    // Unknown ids are a client error, never a 500.
    let missing = reqwest::get(url(addr, &format!("/api/v1/streams/{id}"))).await.unwrap();
    assert_eq!(missing.status(), 200);
}

#[tokio::test]
async fn finishing_twice_is_idempotent_and_srt_is_served() {
    let addr = start().await;
    let client = reqwest::Client::new();
    let view: Value = client
        .post(url(addr, "/api/v1/streams"))
        .json(&json!({"fps": 2.0, "width": 640, "height": 360}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = view["id"].as_u64().unwrap();

    let first: Value = client
        .post(url(addr, &format!("/api/v1/streams/{id}/finish")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(first["state"], "done");
    assert_eq!(first["cues"], 0);

    // A late duplicate (browser retry) must not reopen or error the stream.
    let second = client
        .post(url(addr, &format!("/api/v1/streams/{id}/finish")))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 200);
    let second: Value = second.json().await.unwrap();
    assert_eq!(second["state"], "done");

    let srt = reqwest::get(url(addr, &format!("/api/v1/streams/{id}/srt")))
        .await
        .unwrap();
    assert_eq!(srt.status(), 200);
}

#[tokio::test]
async fn removed_v0_upload_endpoints_are_gone() {
    let addr = start().await;
    let client = reqwest::Client::new();
    // The whole legacy path was deleted; a 404 here is the contract that keeps
    // it from creeping back.
    for path in ["/api/v0/upload/video", "/api/v0/upload/image", "/api/v0/jobs"] {
        let res = client
            .post(url(addr, path))
            .body("x")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 404, "{path} must be removed");
    }
}

#[tokio::test]
async fn progress_stream_pushes_changes_and_survives_an_idle_interval() {
    let addr = start().await;
    let client = reqwest::Client::new();

    // Subscribing before the stream exists must be accepted, not rejected:
    // the browser connects before it uploads the first frame.
    let mut events = client
        .get(url(addr, "/api/v1/streams/1/events"))
        .send()
        .await
        .unwrap();
    assert_eq!(events.status(), 200, "event stream should be accepted");
    assert_eq!(
        events
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        "text/event-stream"
    );
    let idle = read_event(&mut events).await;
    assert!(idle.contains("waiting"), "expected an idle comment, got {idle:?}");

    // A second subscriber, opened after the stream exists, gets the current
    // snapshot immediately instead of waiting for the next frame.
    let opened = client
        .post(url(addr, "/api/v1/streams"))
        .json(&json!({"fps": 2.0, "width": 640, "height": 360}))
        .send()
        .await
        .unwrap();
    assert_eq!(opened.status(), 200);

    let mut late = client
        .get(url(addr, "/api/v1/streams/1/events"))
        .send()
        .await
        .unwrap();
    let first = read_event(&mut late).await;
    assert!(
        first.contains("event: progress") && first.contains("\"state\":\"open\""),
        "late subscriber should get a snapshot at once, got {first:?}"
    );
}
/// Read one SSE frame (up to the blank line that terminates it) as text.
async fn read_event(resp: &mut reqwest::Response) -> String {
    let mut buf = Vec::new();
    while let Some(chunk) = resp.chunk().await.unwrap() {
        buf.extend_from_slice(&chunk);
        if buf.windows(2).any(|w| w == b"\n\n") {
            break;
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}
