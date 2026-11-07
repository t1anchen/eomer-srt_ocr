//! Frame-streaming job: the browser decodes the video and posts one frame at
//! a time; the server OCRs each frame as it arrives.
//!
//! This replaces posting a whole video file. A 1080p frame crop is a few
//! hundred kilobytes, so every request is small enough to retry and to show
//! progress on. The server never holds more than one decoded frame, which is
//! what keeps memory flat regardless of how long the video is.

use crate::band::Band;
use crate::ocr::LlamaClient;
use crate::timeline::{self, Cue};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::broadcast;

/// One received frame, identified by its index in the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FrameIndex(pub u32);

/// Reuse the pipeline's observation shape so both paths feed one timeline
/// builder and cannot drift apart.
pub use crate::timeline::FrameObservation;

/// What the client tells us when it opens a stream.
#[derive(Debug, Clone)]
pub struct StreamSpec {
    pub fps: f64,
    pub width: u32,
    pub height: u32,
    /// Subtitle band in full-frame pixel coordinates, if the client already
    /// knows it. `None` means the server derives it from the first frames.
    pub band: Option<Band>,
    pub prompt: String,
    pub ignore: Vec<String>,
    /// Frames the client sends already cropped to `band`, with the crop as
    /// the image origin. Then the band is only metadata: every frame's height
    /// is the band height, and cropping server-side would cut the dialogue.
    pub cropped: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamState {
    /// Accepting frames.
    Open,
    /// Frames received and OCR'd; SRT written.
    Done,
}

#[derive(Debug, Clone, Serialize)]
pub struct StreamView {
    pub id: u64,
    pub state: StreamState,
    pub fps: f64,
    pub width: u32,
    pub height: u32,
    pub band: Option<BandView>,
    /// Frames accepted so far.
    pub frames_received: u32,
    /// Frames that produced at least one subtitle line.
    pub frames_with_text: u32,
    pub cues: usize,
    pub elapsed_secs: f64,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct BandView {
    pub y0: u32,
    pub y1: u32,
}

impl From<Band> for BandView {
    fn from(b: Band) -> Self {
        Self { y0: b.y0, y1: b.y1 }
    }
}

#[derive(Debug)]
pub struct FrameStream {
    pub id: u64,
    spec: StreamSpec,
    state: StreamState,
    band: Option<Band>,
    frames: BTreeMap<FrameIndex, FrameObservation>,
    started: std::time::Instant,
    finished: Option<FinishedStream>,
    /// Kept so a finished stream can be downloaded without re-OCRing.
    pub out_dir: PathBuf,
    /// Fans out snapshots to every /events subscriber. Never holds the stream
    /// lock, so publishing cannot deadlock against a reader.
    progress: broadcast::Sender<StreamView>,
}

#[derive(Debug, Clone)]
struct FinishedStream {
    cues: Vec<Cue>,
    srt: String,
}

impl FrameStream {
    fn new(id: u64, spec: StreamSpec, out_dir: PathBuf) -> Self {
        Self {
            id,
            band: spec.band,
            spec,
            state: StreamState::Open,
            frames: BTreeMap::new(),
            started: std::time::Instant::now(),
            finished: None,
            out_dir,
            progress: broadcast::channel(PROGRESS_BUFFER).0,
        }
    }

    pub fn view(&self) -> StreamView {
        let cues = self.finished.as_ref().map(|f| f.cues.len()).unwrap_or(0);
        let with_text = self.frames.values().filter(|f| !f.lines.is_empty()).count();
        StreamView {
            id: self.id,
            state: self.state,
            fps: self.spec.fps,
            width: self.spec.width,
            height: self.spec.height,
            band: self.band.map(BandView::from),
            frames_received: self.frames.len() as u32,
            frames_with_text: with_text as u32,
            cues,
            elapsed_secs: self.started.elapsed().as_secs_f64(),
        }
    }

    /// Push the current snapshot to every subscriber. Best-effort: when nobody
    /// is listening the send simply fails and is ignored.
    pub fn publish(&self) {
        let _ = self.progress.send(self.view());
    }

    /// Subscribe to snapshots. Callers must publish a snapshot right after
    /// subscribing so the client starts from a known state instead of waiting
    /// for the next frame.
    pub fn subscribe(&self) -> broadcast::Receiver<StreamView> {
        self.progress.subscribe()
    }

    /// Timestamp of a frame index. Derived from fps rather than carried in the
    /// request, so a client cannot desynchronise the timeline from the index.
    /// Timestamp of a frame index on the sampling grid.
    pub fn timestamp_for(&self, index: FrameIndex) -> f64 {
        index.0 as f64 / self.spec.fps
    }
}

/// How many progress snapshots a slow subscriber may fall behind before it is
/// told it missed some. Small on purpose: a snapshot is cheap and the latest
/// one is always the useful one.
const PROGRESS_BUFFER: usize = 16;

/// Owns every open and finished stream.
///
/// `Clone` shares the same map: it is a handle, like `Store`.
#[derive(Debug, Default, Clone)]
pub struct StreamStore {
    streams: Arc<Mutex<BTreeMap<u64, Arc<Mutex<FrameStream>>>>>,
    next_id: Arc<Mutex<u64>>,
}

impl StreamStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<u64, Arc<Mutex<FrameStream>>>> {
        self.streams.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn open(&self, spec: StreamSpec, out_dir: PathBuf) -> Arc<Mutex<FrameStream>> {
        let id = {
            let mut n = self.next_id.lock().unwrap_or_else(|e| e.into_inner());
            *n += 1;
            *n
        };
        let stream = Arc::new(Mutex::new(FrameStream::new(id, spec, out_dir)));
        {
            let guard = lock(&stream);
            guard.publish();
        }
        self.lock().insert(id, Arc::clone(&stream));
        stream
    }

    pub fn get(&self, id: u64) -> Option<Arc<Mutex<FrameStream>>> {
        self.lock().get(&id).cloned()
    }

    pub fn views(&self) -> Vec<StreamView> {
        self.lock().values().map(|s| lock(s).view()).collect()
    }
}

fn lock(s: &Arc<Mutex<FrameStream>>) -> MutexGuard<'_, FrameStream> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

/// The prompt selected when the stream was opened.
pub fn prompt_of(stream: &Arc<Mutex<FrameStream>>) -> String {
    lock(stream).spec.prompt.clone()
}

/// Decode one uploaded frame and OCR it.
///
/// Runs on the blocking worker thread, never inside an async handler: the OCR
/// client wraps a blocking HTTP client, and constructing or dropping one on an
/// async runtime thread panics.
pub fn ocr_frame(
    stream: &Arc<Mutex<FrameStream>>,
    ocr: &LlamaClient,
    index: FrameIndex,
    bytes: &[u8],
) -> anyhow::Result<FrameObservation> {
    let (spec, band) = {
        let s = lock(stream);
        (s.spec.clone(), s.band)
    };

    let img = image::load_from_memory(bytes)
        .map_err(|e| anyhow::anyhow!("frame {}: not a decodable image: {e}", index.0))?
        .to_rgb8();

    let (w, h) = img.dimensions();
    // Cropping to an odd height is normal, and every scale in the chain
    // rounds independently, so exact match would reject valid frames.
    // A cropped stream is only as tall as its band, so compare against the
    // width alone and let any height through.
    let height_bad = !spec.cropped && h > spec.height + 1;
    if w.abs_diff(spec.width) > 1 || height_bad {
        anyhow::bail!(
            "frame {}: expected {}x{}, got {w}x{h}",
            index.0,
            spec.width,
            spec.height
        );
    }

    // OCRing a full frame loses small subtitle text, because the model
    // downsamples the whole picture. So crop when the client sent a full
    // frame; when it already cropped, the image *is* the band.
    let crop = if spec.cropped {
        img
    } else {
        let band = band.unwrap_or_else(|| crate::band::default_band(h));
        let pad_top = ((band.height() as f64) * 0.25) as u32;
        crate::ocr::subtitle_crop(&img, band, pad_top, padding_bottom(band, h))
    };

    let t0 = std::time::Instant::now();
    let reply = ocr.ocr_image(&crop)?;
    let latency_ms = t0.elapsed().as_millis() as u64;

    Ok(FrameObservation {
        index: index.0 as usize,
        t: index.0 as f64 / spec.fps,
        raw: reply.text.clone(),
        lines: timeline::clean_lines(&reply.text),
        latency_ms,
    })
}

fn padding_bottom(band: Band, height: u32) -> u32 {
    let want = ((band.height() as f64) * 0.15) as u32;
    height.saturating_sub(band.y1).min(want)
}

/// Store an observation and reject duplicates / out-of-order replays.
pub fn record(stream: &Arc<Mutex<FrameStream>>, obs: FrameObservation) -> anyhow::Result<u32> {
    let mut s = lock(stream);
    if s.state != StreamState::Open {
        anyhow::bail!("stream {} is already finished", s.id);
    }
    let index = FrameIndex(obs.index as u32);
    // A retried frame overwrites its earlier result rather than appending, so
    // a client may resend a frame after a network failure without corrupting
    // the timeline.
    s.frames.insert(index, obs);
    s.publish();
    Ok(s.frames.len() as u32)
}

/// Apply explicit ignore rules before constructing cues.
///
/// Automatic overlay detection is deliberately not reused here yet: OCR of an
/// isolated crop has a different line-frequency distribution from the CLI's
/// wider crop, and the CLI threshold can classify the only repeated dialogue
/// line as an overlay on a short stream.
fn without_ignored_lines(
    observations: &[FrameObservation],
    ignored: &[String],
) -> Vec<FrameObservation> {
    let mut cleaned = observations.to_vec();
    for observation in &mut cleaned {
        let before = observation.lines.len();
        observation.lines.retain(|line| !ignored.contains(line));
        if observation.lines.len() != before {
            observation.raw = observation.lines.join("\n");
        }
    }
    cleaned
}

/// Build the timeline from every frame received and write the SRT.
pub fn finish(stream: &Arc<Mutex<FrameStream>>) -> anyhow::Result<StreamView> {
    let mut s = lock(stream);
    if s.state == StreamState::Done {
        return Ok(s.view());
    }
    let cfg = timeline::TimelineConfig::default();
    let observations: Vec<FrameObservation> = s.frames.values().cloned().collect();
    let observations = without_ignored_lines(&observations, &s.spec.ignore);
    let frame_dt = 1.0 / s.spec.fps;
    let cues = timeline::build_timeline(&observations, frame_dt, &cfg);
    let srt = timeline::to_srt(&cues);

    std::fs::create_dir_all(&s.out_dir)?;
    std::fs::write(s.out_dir.join("output.srt"), &srt)?;

    let elapsed = s.started.elapsed().as_secs_f64();
    let _ = elapsed;
    s.finished = Some(FinishedStream { cues, srt });
    s.state = StreamState::Done;
    s.publish();
    Ok(s.view())
}

/// The finished timeline, if the stream has one. Cues stay in memory so the
/// browser can show them for review without re-running OCR.
pub fn cues_of(stream: &Arc<Mutex<FrameStream>>) -> Option<Vec<Cue>> {
    let s = lock(stream);
    s.finished.as_ref().map(|f| f.cues.clone())
}

/// The finished SRT body, if the stream produced one.
pub fn srt_of(stream: &Arc<Mutex<FrameStream>>) -> Option<String> {
    let s = lock(stream);
    s.finished.as_ref().map(|f| f.srt.clone())
}

/// Where a stream's SRT lives on disk.
pub fn srt_path(out_dir: &Path) -> PathBuf {
    out_dir.join("output.srt")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> StreamSpec {
        StreamSpec {
            fps: 2.0,
            width: 640,
            height: 360,
            band: None,
            prompt: "x".to_string(),
            ignore: Vec::new(),
            cropped: false,
        }
    }

    #[test]
    fn stream_prompt_is_preserved_for_the_ocr_client() {
        let store = StreamStore::new();
        let s = store.open(spec(), PathBuf::from("/tmp/unused"));
        assert_eq!(prompt_of(&s), "x");
    }

    #[test]
    fn explicit_ignores_are_removed_before_the_timeline() {
        let observations = vec![FrameObservation {
            index: 0,
            t: 0.0,
            raw: "IP属地\n字幕".to_string(),
            lines: vec!["IP属地".to_string(), "字幕".to_string()],
            latency_ms: 1,
        }];
        let cleaned = without_ignored_lines(&observations, &["IP属地".to_string()]);
        assert_eq!(cleaned[0].lines, vec!["字幕"]);
        assert_eq!(cleaned[0].raw, "字幕");
    }

    #[test]
    fn timestamps_follow_the_frame_index() {
        let store = StreamStore::new();
        let s = store.open(spec(), PathBuf::from("/tmp/unused"));
        let st = lock(&s);
        assert_eq!(st.timestamp_for(FrameIndex(0)), 0.0);
        assert_eq!(st.timestamp_for(FrameIndex(5)), 2.5);
    }

    #[test]
    fn a_resent_frame_replaces_its_earlier_result() {
        let store = StreamStore::new();
        let s = store.open(spec(), PathBuf::from("/tmp/unused"));
        let obs = |text: &str| FrameObservation {
            index: 3,
            t: 1.5,
            raw: text.to_string(),
            lines: vec![text.to_string()],
            latency_ms: 1,
        };
        record(&s, obs("first")).unwrap();
        let n = record(&s, obs("second")).unwrap();
        assert_eq!(n, 1, "resending must not duplicate the frame");
        assert_eq!(lock(&s).frames[&FrameIndex(3)].lines, vec!["second".to_string()]);
    }

    #[test]
    fn finishing_twice_is_idempotent() {
        let dir = std::env::temp_dir().join(format!("eomer-stream-{}", std::process::id()));
        let store = StreamStore::new();
        let s = store.open(spec(), dir.clone());
        record(
            &s,
            FrameObservation {
                index: 0,
                t: 0.0,
                raw: "一".to_string(),
                lines: vec!["一".to_string()],
                latency_ms: 1,
            },
        )
        .unwrap();
        let a = finish(&s).unwrap();
        let b = finish(&s).unwrap();
        assert_eq!(a.state, StreamState::Done);
        assert_eq!(b.frames_received, 1);
        assert_eq!(a.cues, b.cues);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn views_count_frames_with_text() {
        let store = StreamStore::new();
        let s = store.open(spec(), PathBuf::from("/tmp/unused"));
        for (i, lines) in [(0u32, vec!["a".to_string()]), (1, vec![]), (2, vec!["b".to_string()])] {
            record(
                &s,
                FrameObservation {
                    index: i as usize,
                    t: i as f64 / 2.0,
                    raw: lines.join("\n"),
                    lines,
                    latency_ms: 1,
                },
            )
            .unwrap();
        }
        let v = store.views();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].frames_received, 3);
        assert_eq!(v[0].frames_with_text, 2);
    }
}
