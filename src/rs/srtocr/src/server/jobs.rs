//! Job store and the single background worker that runs OCR pipelines.
//!
//! Only one job runs at a time on purpose: a local llama.cpp backend holds one
//! model in VRAM, and concurrent inference requests make every job slower
//! without finishing any of them sooner.

use super::gate::InferenceGate;
use crate::image_ocr::ImageConfig;
use crate::ocr::LlamaClient;
use crate::pipeline::PipelineConfig;
use crate::timeline::Cue;
use serde::Serialize;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};

pub type JobId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Image,
    Video,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Done,
    Failed,
}

/// Where an uploaded file lands. The upload handler writes the bytes; the job
/// reads them back and removes the directory once it is finished.
#[derive(Debug, Clone)]
pub struct Upload {
    pub job_dir: PathBuf,
    pub input: PathBuf,
    pub original_name: String,
}

/// What a finished job produced, in a shape the browser can render directly.
#[derive(Debug, Clone, Serialize)]
pub struct JobOutcome {
    /// "image" or "video"; mirrors `kind` but survives on its own in JSON.
    pub kind: &'static str,
    pub cues: Vec<Cue>,
    /// Flat OCR text (image jobs only).
    pub text: Option<String>,
    pub srt: Option<String>,
    pub output_dir: PathBuf,
    pub frames_processed: Option<usize>,
    pub frames_with_text: Option<usize>,
    pub elapsed_secs: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobView {
    pub id: JobId,
    pub kind: JobKind,
    pub state: JobState,
    /// 0.0..=1.0; `None` while queued and before sizing is known.
    pub fraction: Option<f64>,
    /// Latest human-readable stage line from the pipeline.
    pub stage: String,
    pub original_name: String,
    /// Set once the job reached a terminal state.
    pub error: Option<String>,
    pub outcome: Option<JobOutcome>,
}

#[derive(Debug)]
pub enum Payload {
    Image { cfg: ImageConfig, ocr: LlamaClient },
    Video { cfg: PipelineConfig, ocr: LlamaClient },
}

impl Payload {
    fn kind(&self) -> JobKind {
        match self {
            Payload::Image { .. } => JobKind::Image,
            Payload::Video { .. } => JobKind::Video,
        }
    }
}

#[derive(Debug)]
pub struct Job {
    pub view: JobView,
    upload: Upload,
    payload: Option<Payload>,
    /// Timestamps for the most recent stage lines, oldest first.
    pub log: Vec<String>,
}

#[derive(Debug, Default)]
struct Inner {
    jobs: Vec<Arc<Mutex<Job>>>,
    queue: VecDeque<JobId>,
    next_id: JobId,
}

#[derive(Debug, Clone)]
pub struct Store {
    inner: Arc<Mutex<Inner>>,
}

impl Default for Store {
    fn default() -> Self {
        Self::new()
    }
}

impl Store {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A poisoned lock means a handler panicked mid-update. Recovering the
        // data is better than taking the whole server down with it.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Register a job and queue it. The caller must have written the upload.
    pub fn add(&self, upload: Upload, payload: Payload) -> JobId {
        let kind = payload.kind();
        let mut inner = self.lock();
        inner.next_id += 1;
        let id = inner.next_id;
        let view = JobView {
            id,
            kind,
            state: JobState::Queued,
            fraction: None,
            stage: "queued".to_string(),
            original_name: upload.original_name.clone(),
            error: None,
            outcome: None,
        };
        inner.jobs.push(Arc::new(Mutex::new(Job {
            view,
            upload,
            payload: Some(payload),
            log: Vec::new(),
        })));
        inner.queue.push_back(id);
        id
    }

    pub fn get(&self, id: JobId) -> Option<Arc<Mutex<Job>>> {
        self.lock().jobs.iter().find(|j| id_of(j) == id).cloned()
    }

    pub fn views(&self) -> Vec<JobView> {
        self.lock()
            .jobs
            .iter()
            .map(|j| lock_job(j).view.clone())
            .collect()
    }

    /// Oldest non-terminal job, removed from the queue.
    fn take_next(&self) -> Option<Arc<Mutex<Job>>> {
        let id = self.lock().queue.pop_front()?;
        self.get(id)
    }
}

fn id_of(job: &Arc<Mutex<Job>>) -> JobId {
    lock_job(job).view.id
}

fn lock_job(job: &Arc<Mutex<Job>>) -> MutexGuard<'_, Job> {
    job.lock().unwrap_or_else(|e| e.into_inner())
}

/// Move a job from queued to running. Returns false if it was already taken.
fn begin(job: &Arc<Mutex<Job>>) -> bool {
    let mut j = lock_job(job);
    if j.view.state != JobState::Queued {
        return false;
    }
    j.view.state = JobState::Running;
    j.view.stage = "starting".to_string();
    true
}

fn record_log(job: &Arc<Mutex<Job>>, msg: &str) {
    let mut j = lock_job(job);
    // The pipeline narrates progress often; keep a bounded tail.
    const MAX_LOG: usize = 200;
    if j.log.len() >= MAX_LOG {
        j.log.remove(0);
    }
    j.log.push(msg.to_string());
    j.view.stage = msg.to_string();
}

fn record_fraction(job: &Arc<Mutex<Job>>, done: usize, total: usize) {
    let mut j = lock_job(job);
    j.view.fraction = if total == 0 {
        None
    } else {
        Some((done as f64 / total as f64).clamp(0.0, 1.0))
    };
}

fn finish(job: &Arc<Mutex<Job>>, outcome: Result<JobOutcome, String>) {
    let mut j = lock_job(job);
    match outcome {
        Ok(o) => {
            j.view.state = JobState::Done;
            j.view.fraction = Some(1.0);
            j.view.stage = "done".to_string();
            j.view.outcome = Some(o);
        }
        Err(e) => {
            j.view.state = JobState::Failed;
            j.view.stage = "failed".to_string();
            j.view.error = Some(e);
        }
    }
    // The uploaded bytes are only an input to the job; drop them once the
    // results are written. Best effort: a leftover temp dir is not fatal.
    let _ = std::fs::remove_dir_all(&j.upload.job_dir);
}

/// Run jobs one after another until the sender side goes away.
pub fn worker(store: Store, gate: InferenceGate, rx: Receiver<()>) {
    while rx.recv().is_ok() {
        loop {
            let Some(job) = store.take_next() else { break };
            if !begin(&job) {
                continue;
            }
            let payload = {
                let mut j = lock_job(&job);
                j.payload.take()
            };
            let Some(payload) = payload else {
                finish(&job, Err("job payload missing".to_string()));
                continue;
            };
            // One pipeline run is one long inference session: it holds the
            // gate for its whole duration so frame uploads wait their turn
            // instead of splitting the backend's compute budget.
            let outcome = {
                let _permit = gate.acquire();
                run_one(&job, payload)
            };
            finish(&job, outcome);
        }
    }
}

fn run_one(job: &Arc<Mutex<Job>>, payload: Payload) -> Result<JobOutcome, String> {
    match payload {
        Payload::Image { cfg, ocr } => run_image(job, cfg, ocr),
        Payload::Video { cfg, ocr } => run_video(job, cfg, ocr),
    }
}

fn run_image(
    job: &Arc<Mutex<Job>>,
    cfg: ImageConfig,
    ocr: LlamaClient,
) -> Result<JobOutcome, String> {
    let started = std::time::Instant::now();
    record_log(job, "running OCR on the uploaded image");
    record_fraction(job, 0, 1);
    let result = crate::image_ocr::run_image(&cfg, &ocr).map_err(|e| format!("{e:#}"))?;
    record_fraction(job, 1, 1);
    let text = if result.text.is_empty() {
        None
    } else {
        Some(result.text.clone())
    };
    Ok(JobOutcome {
        kind: "image",
        cues: Vec::new(),
        text,
        srt: None,
        output_dir: cfg.output_dir,
        frames_processed: None,
        frames_with_text: None,
        elapsed_secs: started.elapsed().as_secs_f64(),
    })
}

fn run_video(
    job: &Arc<Mutex<Job>>,
    cfg: PipelineConfig,
    ocr: LlamaClient,
) -> Result<JobOutcome, String> {
    // Probe first so the UI can show a fraction before OCR starts; the
    // pipeline probes again, which costs one extra ffprobe invocation.
    let ff = crate::ffmpeg::Ffmpeg::discover(None, None).map_err(|e| format!("{e:#}"))?;
    let media = ff.probe(&cfg.input).map_err(|e| format!("{e:#}"))?;
    let fps = cfg
        .fps
        .unwrap_or_else(|| crate::pipeline::choose_fps(&media, media.fps));
    let total = ((media.duration_secs * fps).floor() as usize).max(1);

    // `run_pipeline` narrates through a string callback. The frame counters are
    // the only numeric part of that stream, so the fraction is driven from them
    // until the pipeline can report structured progress on its own.
    let job_ref = Arc::clone(job);
    let r = crate::pipeline::run_pipeline(&cfg, &ff, &ocr, &mut |msg: &str| {
        record_log(&job_ref, msg);
        if let Some((done, _)) = parse_frame_counter(msg) {
            record_fraction(&job_ref, done, total);
        }
    })
    .map_err(|e| format!("{e:#}"))?;

    let srt = std::fs::read_to_string(&r.srt_path).ok();
    Ok(JobOutcome {
        kind: "video",
        cues: r.cues.clone(),
        text: None,
        srt,
        output_dir: cfg.output_dir,
        frames_processed: Some(r.frames_processed),
        frames_with_text: Some(r.frames_with_text),
        elapsed_secs: r.elapsed_secs,
    })
}

/// Pull `n` out of `<prefix> n/total` lines. Only the frame counters are
/// numeric in a stable way; everything else stays a free-form log line.
fn parse_frame_counter(msg: &str) -> Option<(usize, usize)> {
    let rest = msg.strip_prefix("ocr ")?;
    let (n, tail) = rest.split_once('/')?;
    let done: usize = n.trim().parse().ok()?;
    let total: usize = tail.split_whitespace().next()?.parse().ok()?;
    Some((done, total))
}

/// Channel the worker waits on. Sending is how a job gets picked up.
pub fn notify(tx: &Sender<()>) {
    let _ = tx.send(());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frame_counter_lines() {
        assert_eq!(parse_frame_counter("ocr 12/300 frames (4%)"), Some((12, 300)));
        assert_eq!(parse_frame_counter("ocr 1/1 frames (100%)"), Some((1, 1)));
    }

    #[test]
    fn ignores_lines_without_counters() {
        assert_eq!(parse_frame_counter("band: y=608..742"), None);
        assert_eq!(parse_frame_counter("resume: 5 of 300 frames already processed"), None);
        assert_eq!(parse_frame_counter("ocr frames done"), None);
    }

    #[test]
    fn store_hands_out_ids_and_views() {
        let store = Store::new();
        let up = |name: &str| Upload {
            job_dir: PathBuf::from("/tmp/x"),
            input: PathBuf::from("/tmp/x/in.png"),
            original_name: name.to_string(),
        };
        let placeholder = || Payload::Image {
            cfg: ImageConfig {
                input: PathBuf::from("/tmp/x/in.png"),
                output_dir: PathBuf::from("/tmp/x"),
                max_width: 1280,
            },
            ocr: LlamaClient::new("http://127.0.0.1:9".to_string(), None, None),
        };
        let a = store.add(up("a.png"), placeholder());
        let b = store.add(up("b.png"), placeholder());
        assert_ne!(a, b);
        let views = store.views();
        assert_eq!(views.len(), 2);
        assert!(views.iter().all(|v| v.state == JobState::Queued));
        assert_eq!(store.get(a).unwrap().lock().unwrap().view.original_name, "a.png");
    }
}
