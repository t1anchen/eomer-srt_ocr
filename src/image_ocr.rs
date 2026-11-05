//! Single-image OCR: the simplest possible path through the model.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ImageConfig {
    pub input: PathBuf,
    pub output_dir: PathBuf,
    /// Max width the image is scaled to before sending.
    pub max_width: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageResult {
    pub input: PathBuf,
    pub width: u32,
    pub height: u32,
    pub lines: Vec<String>,
    pub text: String,
    pub latency_ms: u64,
    pub txt_path: PathBuf,
}

/// Run OCR over a single image file and write the text next to it.
pub fn run_image(cfg: &ImageConfig, ocr: &crate::ocr::LlamaClient) -> Result<ImageResult> {
    let img = image::open(&cfg.input)
        .with_context(|| format!("opening image {}", cfg.input.display()))?;
    let rgb = img.to_rgb8();
    let (mut w, mut h) = rgb.dimensions();
    let mut send = rgb;
    if w > cfg.max_width {
        h = (h as f64 * cfg.max_width as f64 / w as f64).round() as u32;
        w = cfg.max_width;
        send = image::imageops::resize(&send, w, h, image::imageops::FilterType::Lanczos3);
    }
    if w == 0 || h == 0 {
        bail!("image {} has zero dimensions", cfg.input.display());
    }

    let started = std::time::Instant::now();
    let reply = ocr.ocr_image(&send)?;
    let latency_ms = started.elapsed().as_millis() as u64;
    let lines = crate::timeline::clean_lines(&reply.text);
    if lines.is_empty() && !reply.text.trim().is_empty() {
        bail!("model returned no usable text for {}", cfg.input.display());
    }

    std::fs::create_dir_all(&cfg.output_dir)?;
    let stem = Path::new(&cfg.input)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "image".into());
    let txt_path = cfg.output_dir.join(format!("{stem}.txt"));
    std::fs::write(&txt_path, lines.join("\n"))?;

    Ok(ImageResult {
        input: cfg.input.clone(),
        width: w,
        height: h,
        text: lines.join("\n"),
        lines,
        latency_ms,
        txt_path,
    })
}
