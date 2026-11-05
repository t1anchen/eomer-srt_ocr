//! eomer-srt-ocr: extract burnt-in subtitles from video frames using a local VLM.
//!
//! Pipeline: ffmpeg frame sampling -> subtitle band detection -> VLM OCR via
//! llama.cpp `/v1/chat/completions` -> text cleanup/dedup -> SRT output.

pub mod band;
pub mod ffmpeg;
pub mod image_ocr;
pub mod model;
pub mod ocr;
pub mod paths;
pub mod pipeline;
pub mod postproc;
pub mod timeline;
pub mod validate;

pub use pipeline::{PipelineConfig, PipelineResult, run_pipeline};
