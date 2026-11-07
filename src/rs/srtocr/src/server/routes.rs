//! HTTP routes: static UI, health, frame streams, result download.

use crate::server::frames::{self, FrameIndex, FrameStream, StreamSpec, StreamView};
use crate::server::AppState;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{sse, Html, IntoResponse, Json, Response, Sse};
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;
use serde_json::json;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

// The browser assets live at the repository's src/web/, four levels above this
// file (src/rs/srtocr/src/server/).
async fn index_html() -> Html<&'static str> {
    Html(include_str!("../../../../web/index.html"))
}

async fn script_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../../../../web/app.js"),
    )
}

async fn frames_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../../../../web/frames.js"),
    )
}

async fn style_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../../../../web/style.css"),
    )
}

pub fn router(state: AppState, tx: Sender<()>) -> Router {
    Router::new()
        .route("/", get(index_html))
        .route("/app.js", get(script_js))
        .route("/frames.js", get(frames_js))
        .route("/style.css", get(style_css))
        .route("/api/v1/health", get(health))
        .route("/api/v1/streams", post(open_stream).get(list_streams))
        .route("/api/v1/streams/{id}", get(get_stream))
        .route("/api/v1/streams/{id}/frames", post(put_frame))
        .route("/api/v1/streams/{id}/finish", post(finish_stream))
        .route("/api/v1/streams/{id}/srt", get(download_stream_srt))
        .route("/api/v1/streams/{id}/cues", get(stream_cues))
        .route("/api/v1/streams/{id}/txt", get(download_stream_txt))
        .route("/api/v1/streams/{id}/events", get(stream_events))
        // Unversioned aliases for v1, so a cached client does not break.
        .route("/api/health", get(health))
        .route("/api/streams", post(open_stream).get(list_streams))
        .route("/api/streams/{id}", get(get_stream))
        .route("/api/streams/{id}/frames", post(put_frame))
        .route("/api/streams/{id}/finish", post(finish_stream))
        .route("/api/streams/{id}/srt", get(download_stream_srt))
        .route("/api/streams/{id}/cues", get(stream_cues))
        .route("/api/streams/{id}/txt", get(download_stream_txt))
        .with_state((state, tx))
}

type Ctx = (AppState, Sender<()>);

#[derive(Debug, Serialize)]
struct HealthBody {
    ok: bool,
    backend: String,
    backend_reachable: bool,
}

async fn health(State((state, _)): State<Ctx>) -> Json<HealthBody> {
    let base = state.cfg.server.clone();
    let url = format!("{base}/health");
    // A blocking HTTP call inside an async handler would stall the reactor, so
    // the probe runs on a blocking thread.
    let reachable = tokio::task::spawn_blocking(move || {
        reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .ok()
            .and_then(|c| c.get(&url).send().ok())
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    })
    .await
    .unwrap_or(false);
    Json(HealthBody {
        ok: true,
        backend: state.cfg.server,
        backend_reachable: reachable,
    })
}

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Error type that always renders as JSON, so the UI can show the message.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, format!("{e}"))
    }
}

// ---------------------------------------------------------------------------
// Frame streaming
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct OpenStreamBody {
    fps: f64,
    width: u32,
    height: u32,
    /// Subtitle band in full-frame pixels. Omit to let the server use a
    /// bottom-band default for the first frames.
    #[serde(default)]
    band: Option<BandBody>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    ignore: Vec<String>,
    /// True when the client sends crops instead of full frames.
    #[serde(default)]
    cropped: bool,
}

#[derive(Debug, Deserialize)]
struct BandBody {
    y0: u32,
    y1: u32,
}

#[derive(Debug, Deserialize)]
struct FrameParams {
    /// Position of this frame in the sampling grid; the timestamp is
    /// `index / fps`.
    index: u32,
}

/// Per-frame ceiling. A 1080p subtitle crop at 1280 wide is a few hundred KB;
/// the headroom covers a client that sends a full frame by mistake.
pub const MAX_FRAME_BYTES: usize = 24 * 1024 * 1024;

// The client has already isolated the subtitle band. Ask the OCR model to read
// that crop literally; the conversational subtitle prompt can return an empty
// response even when the crop contains clear text.
const OCR_PROMPT: &str = crate::ocr::DEFAULT_IMAGE_PROMPT;

/// Open a frame stream. Returns the id plus the band the server will use, so
/// the client can crop identically and cut payload size.
async fn open_stream(
    State((state, _)): State<Ctx>,
    Json(body): Json<OpenStreamBody>,
) -> Result<Json<StreamView>, ApiError> {
    crate::validate::check_fps(body.fps)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    crate::validate::check_width(body.width)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    if body.height == 0 || body.height > 8192 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("height {} is outside 1..=8192", body.height),
        ));
    }
    let band = body.band.map(|b| crate::band::Band {
        y0: b.y0,
        y1: b.y1,
    });
    if let Some(b) = band {
        if b.y0 >= b.y1 || b.y1 > body.height {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                format!(
                    "band {}..{} does not fit a {}px frame",
                    b.y0, b.y1, body.height
                ),
            ));
        }
    }

    let spec = StreamSpec {
        fps: body.fps,
        width: body.width,
        height: body.height,
        band,
        prompt: body.prompt.unwrap_or_else(|| OCR_PROMPT.to_string()),
        ignore: body.ignore,
        cropped: body.cropped,
    };
    let out_dir = state.cfg.work_dir.join(format!(
        "stream-{}-{}",
        std::process::id(),
        now_millis()
    ));
    let stream = state.streams.open(spec, out_dir);
    let view = {
        let s = stream.lock().unwrap_or_else(|e| e.into_inner());
        s.view()
    };
    Ok(Json(view))
}

async fn list_streams(State((state, _)): State<Ctx>) -> Json<Vec<StreamView>> {
    Json(state.streams.views())
}

async fn get_stream(
    State((state, _)): State<Ctx>,
    Path(id): Path<u64>,
) -> Result<Json<StreamView>, ApiError> {
    let stream = state
        .streams
        .get(id)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, format!("no stream with id {id}")))?;
    let view = {
        let s = stream.lock().unwrap_or_else(|e| e.into_inner());
        s.view()
    };
    Ok(Json(view))
}

/// Accept one frame, decode it and OCR it.
///
/// The OCR client wraps a blocking HTTP client. It is constructed *and*
/// dropped inside `spawn_blocking`, never on an async runtime thread: doing
/// either on the reactor panics with "Cannot drop a runtime in a context where
/// blocking is not allowed". See docs/troubleshooting/2026-W45-4-*.md.
async fn put_frame(
    State((state, _)): State<Ctx>,
    Path(id): Path<u64>,
    Query(params): Query<FrameParams>,
    body: Bytes,
) -> Result<Json<StreamView>, ApiError> {
    if body.len() > MAX_FRAME_BYTES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("frame of {} bytes exceeds {MAX_FRAME_BYTES}", body.len()),
        ));
    }
    let stream = state
        .streams
        .get(id)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, format!("no stream with id {id}")))?;

    let server = state.cfg.server.clone();
    let prompt = frames::prompt_of(&stream);
    let idx = FrameIndex(params.index);
    let stream_for_task: Arc<Mutex<FrameStream>> = Arc::clone(&stream);
    // The same gate the job worker holds, so a frame upload queues behind a
    // running pipeline instead of competing with it for the single model.
    let gate = state.gate.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<(), String> {
        let _permit = gate.acquire();
        // Built and dropped here, on a plain blocking thread.
        let ocr = crate::ocr::LlamaClient::new(server, None, Some(prompt));
        let obs = frames::ocr_frame(&stream_for_task, &ocr, idx, &body).map_err(|e| format!("{e:#}"))?;
        frames::record(&stream_for_task, obs).map(|_| ()).map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, format!("frame task failed: {e}")))?;

    result.map_err(|e| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, e))?;

    let view = {
        let s = stream.lock().unwrap_or_else(|e| e.into_inner());
        s.view()
    };
    Ok(Json(view))
}

async fn finish_stream(
    State((state, _)): State<Ctx>,
    Path(id): Path<u64>,
) -> Result<Json<StreamView>, ApiError> {
    let stream = state
        .streams
        .get(id)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, format!("no stream with id {id}")))?;
    frames::finish(&stream).map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    let view = {
        let s = stream.lock().unwrap_or_else(|e| e.into_inner());
        s.view()
    };
    Ok(Json(view))
}

/// One subtitle line span, as the browser needs it for review: what it says and
/// where it sits on the timeline.
#[derive(Debug, Serialize, Clone)]
struct CueView {
    start: f64,
    end: f64,
    text: String,
}

async fn stream_cues(
    State((state, _)): State<Ctx>,
    Path(id): Path<u64>,
) -> Result<Json<Vec<CueView>>, ApiError> {
    let stream = state
        .streams
        .get(id)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, format!("no stream with id {id}")))?;
    let cues = frames::cues_of(&stream)
        .ok_or_else(|| ApiError::new(StatusCode::CONFLICT, "stream is not finished yet"))?;
    Ok(Json(
        cues.into_iter()
            .map(|c| CueView {
                start: c.start,
                end: c.end,
                text: c.text,
            })
            .collect(),
    ))
}

/// The finished cues as plain text, one line per cue. Encoded as a data URL by
/// the client so a single-file deployment needs no extra endpoint round trip.
async fn download_stream_txt(
    State((state, _)): State<Ctx>,
    Path(id): Path<u64>,
) -> Result<([(axum::http::HeaderName, String); 2], String), ApiError> {
    let stream = state
        .streams
        .get(id)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, format!("no stream with id {id}")))?;
    let cues = frames::cues_of(&stream)
        .ok_or_else(|| ApiError::new(StatusCode::CONFLICT, "stream is not finished yet"))?;
    let body = cues
        .iter()
        .map(|c| c.text.replace('\n', " "))
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    Ok((
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"stream-{id}.txt\""),
            ),
        ],
        body,
    ))
}

#[derive(Debug, Deserialize)]
struct DownloadQuery {
    /// `inline` renders in the browser, `attachment` downloads it.
    #[serde(default)]
    disposition: Option<String>,
}

async fn download_stream_srt(
    State((state, _)): State<Ctx>,
    Path(id): Path<u64>,
    Query(q): Query<DownloadQuery>,
) -> Result<Response, ApiError> {
    let stream = state
        .streams
        .get(id)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, format!("no stream with id {id}")))?;
    let srt = frames::srt_of(&stream)
        .ok_or_else(|| ApiError::new(StatusCode::CONFLICT, "stream is not finished yet"))?;
    let disposition = if q.disposition.as_deref() == Some("attachment") {
        "attachment"
    } else {
        "inline"
    };
    let cd = format!("{disposition}; filename=\"stream-{id}.srt\"");
    Ok((
        [
            (header::CONTENT_TYPE, "application/x-subrip; charset=utf-8"),
            (header::CONTENT_DISPOSITION, cd.as_str()),
        ],
        srt,
    )
        .into_response())
}


/// How often a quiet stream repeats its snapshot. A browser or a proxy drops
/// an idle SSE connection, so a quiet stream re-sends its current state on this
/// tick instead of staying silent.
const PROGRESS_KEEPALIVE: std::time::Duration = std::time::Duration::from_secs(10);

/// State carried between ticks of one /events connection.
struct EventState {
    /// Set until the first snapshot has been sent.
    initial: Option<sse::Event>,
    /// `None` when the stream does not exist (yet); the connection then idles.
    rx: Option<tokio::sync::broadcast::Receiver<StreamView>>,
    stream: Option<Arc<Mutex<FrameStream>>>,
}

/// Push stream progress to the browser over Server-Sent Events.
///
/// Replaces polling: the client gets the current snapshot as soon as it
/// connects, then one per frame, then one when the stream finishes. An id that
/// does not exist yet is accepted and idles, because the browser subscribes
/// before the first upload.
async fn stream_events(
    State((state, _)): State<Ctx>,
    Path(id): Path<u64>,
) -> Sse<impl futures_core::Stream<Item = Result<sse::Event, std::convert::Infallible>>> {
    let stream = state.streams.get(id);
    // Subscribe before taking the snapshot so nothing published in between is
    // lost, and send the snapshot first so the client never starts blank.
    let (initial, rx) = match &stream {
        Some(s) => {
            let guard = s.lock().unwrap_or_else(|e| e.into_inner());
            (Some(snapshot(&guard.view())), Some(guard.subscribe()))
        }
        None => (None, None),
    };
    let seed = EventState {
        initial,
        rx,
        stream,
    };

    let body = futures_util::stream::unfold(seed, |mut st| async move {
        if let Some(event) = st.initial.take() {
            return Some((Ok(event), st));
        }
        if let Some(receiver) = st.rx.as_mut() {
            match tokio::time::timeout(PROGRESS_KEEPALIVE, receiver.recv()).await {
                Ok(Ok(view)) => return Some((Ok(snapshot(&view)), st)),
                // Missed snapshots: the next one carries complete state.
                Ok(Err(RecvError::Lagged(_))) => {
                    let event = sse::Event::default().comment("lagged");
                    return Some((Ok(event), st));
                }
                Ok(Err(RecvError::Closed)) => return None,
                Err(_) => {}
            }
        }
        // Quiet stream, or not created yet: repeat the current state.
        match st.stream.as_ref() {
            Some(s) => {
                let view = s.lock().unwrap_or_else(|e| e.into_inner()).view();
                Some((Ok(snapshot(&view)), st))
            }
            None => {
                let event = sse::Event::default().comment("waiting");
                Some((Ok(event), st))
            }
        }
    });

    Sse::new(body).keep_alive(sse::KeepAlive::default())
}

fn snapshot(view: &StreamView) -> sse::Event {
    sse::Event::default()
        .event("progress")
        .json_data(view)
        .unwrap_or_else(|_| sse::Event::default().event("error"))
}
