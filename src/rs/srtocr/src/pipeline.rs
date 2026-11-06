//! End-to-end extraction pipeline.

use crate::band::{self, Band};
use crate::ffmpeg::{Ffmpeg, MediaInfo};
use crate::ocr::LlamaClient;
use crate::timeline::{self, Cue, FrameObservation, TimelineConfig};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Number of equal-height horizontal strips the frame is divided into during
/// subtitle-band discovery. Eight strips give 135 px at 1080p.
const STRIP_COUNT: u32 = 8;
/// Frames processed between progress writes to disk.
const PROGRESS_BATCH: usize = 10;
/// Only strips in the lower part of the frame are candidates (subtitles live
/// below the middle); expressed as a strip index.
const FIRST_CANDIDATE_STRIP: u32 = 4;
/// Context above the detected band, so the VLM still sees layout and can
/// tell subtitles apart from overlays by itself. Fraction of band height.
const BAND_CONTEXT_PAD: f32 = 0.55;
/// Below the band, keep a small margin so descenders/outlines are not cut.
const BAND_BOTTOM_PAD: f32 = 0.25;

#[derive(Debug, Clone)]
pub struct PipelineConfig {
    pub input: PathBuf,
    pub output_dir: PathBuf,
    /// Working width frames are scaled to before analysis/OCR.
    pub max_width: u32,
    /// Samples per second. `None` = adaptive.
    pub fps: Option<f64>,
    /// Frames used for band detection (before OCR starts).
    pub probe_frames: usize,
    pub timeline: TimelineConfig,
    /// Optional manual band override in working-image pixels.
    pub band_override: Option<Band>,
    /// Model name forwarded to the API.
    pub model_name: Option<String>,
    /// Prompt override.
    pub prompt: Option<String>,
    /// Known overlay lines to strip (e.g. danmaku watermarks).
    pub ignore_patterns: Vec<String>,
    /// Occlusion rectangles (fractions of frame W/H) covering permanently
    /// overlaid UI — danmaku bubbles, app watermarks — that must not be OCR'd.
    pub masks: Vec<MaskRect>,
    /// Detect static overlays automatically from probe frames and mask them.
    pub auto_mask: bool,
    /// Ignore any stored progress and start from the first frame.
    pub fresh: bool,
    /// A line seen in at least this share of frames is treated as an overlay.
    pub overlay_min_share: f64,
    /// ...and it must appear in at least this many frames.
    pub overlay_min_frames: usize,
}

/// A rectangle in normalised frame coordinates (`x`, `y` in `0.0..=1.0`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct MaskRect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl MaskRect {
    /// Pixel bounds inside an image of the given size.
    pub fn pixel_bounds(&self, w: u32, h: u32) -> (u32, u32, u32, u32) {
        let x0 = (self.x0.clamp(0.0, 1.0) * w as f32) as u32;
        let y0 = (self.y0.clamp(0.0, 1.0) * h as f32) as u32;
        let x1 = (self.x1.clamp(0.0, 1.0) * w as f32) as u32;
        let y1 = (self.y1.clamp(0.0, 1.0) * h as f32) as u32;
        // Normalise each axis independently: min/max must pair x with x and y
        // with y, otherwise an inverted rectangle swaps its edges.
        (x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1))
    }
}

/// Fallback occlusions for Bilibili-style player overlays, used only when
/// automatic watermark discovery finds nothing: the "IP属地 / 江西" bubble
/// lives near the bottom-left corner, the counter near the top-right.
pub fn default_masks() -> Vec<MaskRect> {
    vec![
        // bottom-left danmaku/watermark bubble (measured on Bilibili captures)
        MaskRect {
            x0: 0.11,
            y0: 0.90,
            x1: 0.17,
            y1: 0.96,
        },
        // bottom-right UI counter
        MaskRect {
            x0: 0.94,
            y0: 0.0,
            x1: 1.0,
            y1: 0.05,
        },
    ]
}

impl PipelineConfig {
    pub fn default_output_dir(input: &Path) -> PathBuf {
        let stem = input
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "output".into());
        PathBuf::from("out").join(stem)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineResult {
    pub media: MediaSummary,
    pub sampling_fps: f64,
    pub band: BandInfo,
    pub frames_processed: usize,
    pub frames_with_text: usize,
    pub cues: Vec<Cue>,
    pub srt_path: PathBuf,
    pub jsonl_path: PathBuf,
    pub elapsed_secs: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaSummary {
    pub width: u32,
    pub height: u32,
    pub duration_secs: f64,
    pub has_audio: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BandInfo {
    pub y0: u32,
    pub y1: u32,
    /// Height of the crop actually sent to the model (includes context above).
    pub crop_y0: u32,
    pub crop_y1: u32,
    pub width: u32,
    pub height: u32,
}

/// Choose a sampling rate when the user did not supply one.
///
/// Subtitles change roughly once per dialogue line (1-6 s); 2 fps gives every
/// line at least two samples (needed to debounce OCR flicker and to build
/// timelines) without drowning the model.
pub fn choose_fps(media: &MediaInfo, media_fps: Option<f64>) -> f64 {
    let mut fps = 2.0;
    // Long media: halve the sampling rate so a full episode stays manageable.
    if media.duration_secs > 3600.0 {
        fps = 1.0;
    }
    // Never sample faster than the source itself.
    if let Some(native) = media_fps {
        if native > 0.0 && native < fps {
            fps = native.max(0.5);
        }
    }
    fps
}

pub fn run_pipeline(
    cfg: &PipelineConfig,
    ff: &Ffmpeg,
    ocr: &LlamaClient,
    progress: &mut dyn FnMut(&str),
) -> Result<PipelineResult> {
    let started = std::time::Instant::now();
    let media = ff.probe(&cfg.input)?;
    crate::validate::check_duration(media.duration_secs)?;
    progress(&format!(
        "media: {}x{} {:.1}s audio={}",
        media.width, media.height, media.duration_secs, media.has_audio
    ));

    let fps = cfg.fps.unwrap_or_else(|| choose_fps(&media, media.fps));
    progress(&format!("sampling at {fps} fps"));

    // --- Pass 1: probe frames for band + overlay discovery -----------------
    // Pixel heuristics proved unreliable on real footage (bright windows, white
    // shirts), so the OCR model itself tells us which horizontal strip holds
    // subtitle-like text, and which grid cells hold repetitive overlays.
    let probe_times: Vec<f64> = {
        let n = cfg.probe_frames.max(6);
        let span = (media.duration_secs * 0.9).max(1.0);
        (0..n).map(|i| span * (i as f64 + 0.5) / n as f64).collect()
    };
    let mut probe_frames: Vec<image::RgbImage> = Vec::with_capacity(probe_times.len());
    for &t in &probe_times {
        probe_frames.push(ff.frame(&cfg.input, t, cfg.max_width)?);
    }
    let (h_img, w_img) = probe_frames[0].dimensions();

    // Masks are opt-in (`auto_mask`). The default strategy is text-level
    // post-processing (see `overlay_lines_from`): it needs no pixel geometry,
    // survives resizing/borders, and a misjudged tag is harmless while a
    // mis-placed mask silently deletes real subtitle text.
    let mut masks = cfg.masks.clone();
    if cfg.auto_mask {
        let found = discover_overlays(&probe_frames, ocr)?;
        for m in &found {
            let (x0, y0, x1, y1) = m.pixel_bounds(w_img, h_img);
            progress(&format!("overlay masked: x={x0}..{x1} y={y0}..{y1}"));
            masks.push(*m);
        }
        if found.is_empty() {
            progress("overlays: none found");
        }
    }
    let probe_frames: Vec<image::RgbImage> = probe_frames
        .into_iter()
        .map(|mut f| {
            apply_masks(&mut f, &masks);
            f
        })
        .collect();

    let band = if let Some(b) = cfg.band_override {
        progress(&format!("band: y={}..{} [manual]", b.y0, b.y1));
        b
    } else {
        let found = discover_band(&probe_frames, ocr, &cfg.ignore_patterns)
            .context("discovering subtitle band")?;
        let band = found
            .map(|c| c.band.padded(STANDARD_PADDING_FRAC, h_img))
            .unwrap_or_else(|| band::default_band(h_img));
        progress(&format!(
            "band: y={}..{} ({}px of {}x{}){}",
            band.y0,
            band.y1,
            band.height(),
            w_img,
            h_img,
            if found.is_some() {
                ""
            } else {
                " [default: no strip looked like subtitles]"
            }
        ));
        band
    };

    // --- Pass 2: OCR every sampled frame ----------------------------------
    let n_frames = ((media.duration_secs * fps).floor() as usize).max(1);
    let frame_dt = 1.0 / fps;
    let mut observations: Vec<FrameObservation> = Vec::with_capacity(n_frames);
    let mut frames_with_text;
    let progress_every = (n_frames / 20).max(1);

    // Resume: reuse frames already processed by an interrupted run with the
    // same settings, and skip them below.
    let identity = crate::progress::RunIdentity {
        input: cfg.input.display().to_string(),
        width: cfg.max_width,
        fps,
        masked: !masks.is_empty(),
    };
    let mut record = crate::progress::ProgressRecord::new(identity.clone(), n_frames);
    let mut skip: std::collections::HashSet<usize> = std::collections::HashSet::new();
    if !cfg.fresh {
        match crate::progress::ProgressRecord::load(&cfg.output_dir) {
            None => {}
            Some(previous) => match crate::progress::decide(Some(&previous), &identity) {
                crate::progress::ResumeDecision::Resume => {
                    for (idx, value) in previous.done.iter().zip(&previous.observations) {
                        if let Ok(obs) = serde_json::from_value::<FrameObservation>(value.clone()) {
                            if obs.index < n_frames {
                                skip.insert(*idx);
                                observations.push(obs);
                            }
                        }
                    }
                    record.done = previous.done.clone();
                    record.observations = previous.observations.clone();
                    progress(&format!(
                        "resume: {} of {} frames already processed",
                        skip.len(),
                        n_frames
                    ));
                }
                crate::progress::ResumeDecision::Stale => {
                    progress("previous progress was made with different settings; starting over");
                }
                crate::progress::ResumeDecision::Fresh => {}
            },
        }
    }
    // Keep observations in frame order as they arrive out of order on resume.
    observations.sort_by_key(|o| o.index);
    frames_with_text = observations.iter().filter(|o| !o.lines.is_empty()).count();

    for i in 0..n_frames {
        if skip.contains(&i) {
            continue;
        }
        let t = i as f64 * frame_dt;
        let frame = ff.frame(&cfg.input, t, cfg.max_width)?;
        let (w, h) = frame.dimensions();

        // Crop: the detected band plus layout context above and below it.
        let pad_top = (band.height() as f32 * BAND_CONTEXT_PAD) as u32;
        let pad_bottom = (band.height() as f32 * BAND_BOTTOM_PAD) as u32;
        let crop_y0 = band.y0.saturating_sub(pad_top);
        let crop_y1 = (band.y1 + pad_bottom).min(h);
        // Blank out permanent overlays *in full-frame coordinates* before
        // cropping; the VLM otherwise transcribes them as subtitles, and a
        // prompt telling it to ignore them just yields empty output.
        let mut masked = frame.clone();
        apply_masks(&mut masked, &masks);
        let crop = image::imageops::crop_imm(&masked, 0, crop_y0, w, crop_y1 - crop_y0).to_image();

        let t0 = std::time::Instant::now();
        let reply = ocr
            .ocr_image(&crop)
            .with_context(|| ocr_failure_message(i, n_frames, t))?;
        let latency_ms = t0.elapsed().as_millis() as u64;

        let mut lines = timeline::clean_lines(&reply.text);
        let before = lines.len();
        lines.retain(|l| !is_ignored(l, &cfg.ignore_patterns));
        let changed = before != lines.len();

        let raw = if changed && lines.is_empty() {
            String::new()
        } else {
            reply.text.clone()
        };
        let observation = FrameObservation {
            index: i,
            t,
            raw,
            lines,
            latency_ms,
        };
        if !observation.lines.is_empty() {
            frames_with_text += 1;
        }
        record.done.push(i);
        record
            .observations
            .push(serde_json::to_value(&observation)?);
        observations.push(observation);
        if record.done.len() % PROGRESS_BATCH == 0 {
            record.save(&cfg.output_dir)?;
        }

        if i % progress_every == 0 || i + 1 == n_frames {
            progress(&format!(
                "ocr {}/{} frames ({:.0}%)",
                i + 1,
                n_frames,
                (i + 1) as f64 / n_frames as f64 * 100.0
            ));
        }
    }

    // --- Emit results ------------------------------------------------------
    // All frames done: persist the final record before any later failure.
    record.save(&cfg.output_dir)?;

    // --- Overlay removal (post-processing) --------------------------------
    // Overlays such as danmaku tags and "IP属地" watermarks repeat in a large
    // share of frames, far more than any dialogue line. Detect them from the
    // whole run's statistics and strip them before the timeline is built, so a
    // recurring tag never becomes its own cue or pollutes a real one.
    let detected = overlay_lines_from(&observations, cfg.overlay_min_share, cfg.overlay_min_frames);
    let mut final_obs = observations.clone();
    if !detected.is_empty() {
        let mut sorted: Vec<&String> = detected.iter().collect();
        sorted.sort();
        progress(&format!(
            "overlays removed: {}",
            sorted
                .iter()
                .map(|s| format!("{s:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        for o in &mut final_obs {
            let before = o.lines.len();
            o.lines.retain(|l| !detected.contains(l));
            if o.lines.len() != before {
                o.raw = o.lines.join("\n");
            }
        }
    }

    let cues = timeline::build_timeline(&final_obs, frame_dt, &cfg.timeline);
    std::fs::create_dir_all(&cfg.output_dir)?;
    let srt_path = cfg.output_dir.join("output.srt");
    let jsonl_path = cfg.output_dir.join("frames.jsonl");
    std::fs::write(&srt_path, timeline::to_srt(&cues))?;
    let mut jsonl = String::new();
    for o in &observations {
        jsonl.push_str(&serde_json::to_string(o)?);
        jsonl.push('\n');
    }
    std::fs::write(&jsonl_path, jsonl)?;

    let work_w = cfg.max_width.min(media.width);
    let work_h = if work_w == media.width {
        media.height
    } else {
        (media.height as f64 * work_w as f64 / media.width as f64).round() as u32
    };
    Ok(PipelineResult {
        media: MediaSummary {
            width: media.width,
            height: media.height,
            duration_secs: media.duration_secs,
            has_audio: media.has_audio,
        },
        sampling_fps: fps,
        band: BandInfo {
            y0: band.y0,
            y1: band.y1,
            crop_y0: band
                .y0
                .saturating_sub((band.height() as f32 * BAND_CONTEXT_PAD) as u32),
            crop_y1: (band.y1 + (band.height() as f32 * BAND_BOTTOM_PAD) as u32).min(work_h),
            width: work_w,
            height: work_h,
        },
        frames_processed: n_frames,
        frames_with_text,
        cues,
        srt_path,
        jsonl_path,
        elapsed_secs: started.elapsed().as_secs_f64(),
    })
}

fn is_ignored(line: &str, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return false;
    }
    let squashed: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    patterns.iter().any(|p| {
        let p: String = p.chars().filter(|c| !c.is_whitespace()).collect();
        !p.is_empty() && squashed.contains(&p)
    })
}

/// Default padding applied to a discovered band, as a fraction of its height.
const STANDARD_PADDING_FRAC: f32 = 0.25;

/// Paint every mask rectangle black. Masks are expressed in full-frame
/// normalised coordinates and must be applied to the *uncropped* frame.
pub fn apply_masks(frame: &mut image::RgbImage, masks: &[MaskRect]) {
    let (w, h) = frame.dimensions();
    for m in masks {
        let (x0, y0, x1, y1) = m.pixel_bounds(w, h);
        for y in y0..y1 {
            for x in x0..x1 {
                frame.put_pixel(x, y, image::Rgb([0, 0, 0]));
            }
        }
    }
}

impl Band {
    /// Grow the band symmetrically by `frac` of its height, clamped to the frame.
    pub fn padded(&self, frac: f32, img_height: u32) -> Band {
        let pad = ((self.height() as f32) * frac) as u32;
        Band {
            y0: self.y0.saturating_sub(pad),
            y1: (self.y1 + pad).min(img_height),
        }
    }
}

/// A band candidate found by the strip scan, carrying its evidence.
#[derive(Debug, Clone, Copy)]
pub struct BandCandidate {
    pub band: Band,
    pub hits: usize,
    pub probes: usize,
}

/// Discover the subtitle band by asking the model to OCR horizontal strips.
///
/// For each probe frame the picture is cut into `STRIP_COUNT` equal strips.
/// A strip "hits" when the model returns subtitle-like text: one or two lines,
/// short, and not one of the known overlay strings. The best strip (and one
/// neighbour, for two-line subtitles that straddle a boundary) forms the band.
pub fn discover_band(
    frames: &[image::RgbImage],
    ocr: &LlamaClient,
    ignore_patterns: &[String],
) -> Result<Option<BandCandidate>> {
    if frames.is_empty() {
        return Ok(None);
    }
    let (w, h) = frames[0].dimensions();
    let strip_h = h / STRIP_COUNT;
    if strip_h < 8 {
        return Ok(None);
    }

    let mut hits = vec![0usize; STRIP_COUNT as usize];
    for frame in frames {
        for strip in FIRST_CANDIDATE_STRIP..STRIP_COUNT {
            let y0 = strip * strip_h;
            let crop = image::imageops::crop_imm(frame, 0, y0, w, strip_h).to_image();
            let reply = ocr
                .ocr_image(&crop)
                .with_context(|| format!("OCR failed while probing subtitle strip {strip}"))?;
            let lines: Vec<String> = timeline::clean_lines(&reply.text)
                .into_iter()
                .filter(|l| !is_ignored(l, ignore_patterns))
                .collect();
            if looks_like_subtitle(&lines) {
                hits[strip as usize] += 1;
            }
        }
    }

    let (best, best_hits) = hits
        .iter()
        .enumerate()
        .skip(FIRST_CANDIDATE_STRIP as usize)
        .fold(
            (0usize, 0usize),
            |acc, (i, &n)| if n > acc.1 { (i, n) } else { acc },
        );

    // Require the strip to fire on at least a third of probes to count as real.
    if best_hits == 0 || best_hits * 3 < frames.len() {
        return Ok(None);
    }

    // A two-line subtitle may straddle the boundary with the strip below it.
    let lower = best + 1;
    let next_hits = *hits.get(lower).unwrap_or(&0);
    let y1 = if next_hits * 2 >= best_hits && lower < STRIP_COUNT as usize {
        (lower as u32 + 1) * strip_h
    } else {
        (best as u32 + 1) * strip_h
    };
    Ok(Some(BandCandidate {
        band: Band {
            y0: best as u32 * strip_h,
            y1,
        },
        hits: best_hits,
        probes: frames.len(),
    }))
}

/// Decide whether OCR lines look like a subtitle rather than scenery text.
pub fn looks_like_subtitle(lines: &[String]) -> bool {
    if lines.is_empty() || lines.len() > 2 {
        return false;
    }
    let total: usize = lines.iter().map(|l| l.chars().count()).sum();
    // Subtitles are short lines; long paragraphs are document/scenery text.
    lines.iter().all(|l| l.chars().count() <= 30) && total >= 2
}

/// Grid used by [`discover_overlays`]: rows x cols over the bottom strip.
const OVERLAY_GRID_ROWS: u32 = 4;
const OVERLAY_GRID_COLS: u32 = 4;
/// The bottom fraction of the frame searched for static overlays.
const OVERLAY_SEARCH_TOP: f32 = 0.88;

/// Find permanently overlaid text (danmaku bubbles, "IP属地" tags, counters).
///
/// The bottom strip is split into a grid; each cell is OCR'd on every probe
/// frame. A cell whose reading is identical across most probes is an overlay,
/// because subtitles change much more often than that.
pub fn discover_overlays(frames: &[image::RgbImage], ocr: &LlamaClient) -> Result<Vec<MaskRect>> {
    if frames.len() < 3 {
        return Ok(Vec::new());
    }
    let (w, h) = frames[0].dimensions();
    let top = (h as f32 * OVERLAY_SEARCH_TOP) as u32;
    if top >= h - 4 {
        return Ok(Vec::new());
    }
    let row_h = (h - top) / OVERLAY_GRID_ROWS;
    let col_w = w / OVERLAY_GRID_COLS;
    if row_h < 8 || col_w < 8 {
        return Ok(Vec::new());
    }

    let mut masks = Vec::new();
    for r in 0..OVERLAY_GRID_ROWS {
        for c in 0..OVERLAY_GRID_COLS {
            let x0 = c * col_w;
            let y0 = top + r * row_h;
            // Full-frame normalised bounds, with a small safety margin.
            let margin = 0.004f32;
            let rect = MaskRect {
                x0: (x0 as f32 / w as f32 - margin).max(0.0),
                y0: (y0 as f32 / h as f32 - margin).max(0.0),
                x1: ((x0 + col_w) as f32 / w as f32 + margin).min(1.0),
                y1: ((y0 + row_h) as f32 / h as f32 + margin).min(1.0),
            };
            let mut readings: Vec<String> = Vec::new();
            for frame in frames {
                let cell = image::imageops::crop_imm(frame, x0, y0, col_w, row_h).to_image();
                let reply = ocr
                    .ocr_image(&cell)
                    .with_context(|| format!("OCR failed while probing overlay cell r{r}c{c}"))?;
                let text: String = timeline::clean_lines(&reply.text).join("\n");
                readings.push(text);
            }
            let non_empty: Vec<&String> = readings.iter().filter(|t| !t.is_empty()).collect();
            if non_empty.len() * 2 < frames.len() {
                continue;
            }
            // Identical (whitespace-insensitive) in nearly every probe frame?
            let canonical =
                |s: &String| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
            let first = canonical(non_empty[0]);
            let stable = non_empty.iter().all(|t| canonical(t) == first);
            // Very short readings (1-2 chars) are usually noise, not overlays.
            if stable && first.chars().count() >= 3 {
                masks.push(rect);
            }
        }
    }
    Ok(masks)
}

/// Collect the lines that behave like fixed overlays across the whole run.
///
/// A line is an overlay when it is very short and appears in a large share of
/// frames; real dialogue never occupies that many frames. Once a line is
/// established as an overlay, near-identical OCR misreadings of it
/// (`IP属地` / `IP屋地` / `IP 属地`) are removed as well, because the same
/// overlay is routinely misread differently from frame to frame.
pub fn overlay_lines_from(
    obs: &[FrameObservation],
    min_share: f64,
    min_frames: usize,
) -> std::collections::HashSet<String> {
    let index = crate::postproc::LineIndex::build(obs);
    let candidates = index.overlay_candidates(min_share, min_frames);
    // A line is a fixed overlay when it recurs far more often than the most
    // frequent *other* line. Short inputs make the absolute thresholds weak, so
    // the relative gap does the deciding there.
    let top_frequency = candidates.first().map(|c| c.frequency).unwrap_or(0.0);
    let anchors: Vec<String> = candidates
        .into_iter()
        // Overlays are short tags, not sentences. The bound is generous enough
        // to admit tags, and the frequency gap below rejects talkative lines
        // that happen to be short.
        .filter(|c| c.mean_len <= OVERLAY_MAX_CHARS as f64)
        // Must be close to the most frequent candidate: a fixed overlay is on
        // screen constantly, a talking line is not. Comparing against the top
        // frequency (rather than a constant) keeps the rule meaningful whether
        // the clip is dominated by a watermark or not.
        .filter(|c| c.frequency >= top_frequency * OVERLAY_RELATIVE_FACTOR)
        .map(|c| c.text)
        .collect();

    // Every observed line, so misreadings that never crossed the threshold are
    // still considered.
    let mut found: std::collections::HashSet<String> = anchors.iter().cloned().collect();
    if anchors.is_empty() {
        return found;
    }
    let all_lines: std::collections::HashSet<&String> =
        obs.iter().flat_map(|o| o.lines.iter()).collect();
    for line in all_lines {
        if line.chars().count() > OVERLAY_MAX_CHARS {
            continue;
        }
        // A rare misreading of a frequent anchor is still that overlay. Only
        // distance to an anchor is considered: dialogue lines never become
        // anchors, so a group of similar ones cannot pull each other in.
        if anchors
            .iter()
            .any(|a| crate::timeline::distance(a, line) <= OVERLAY_VARIANT_DISTANCE)
        {
            found.insert(line.clone());
        }
    }
    found
}

/// A qualifying line must reach this share of the most frequent candidate's
/// frequency. Fixed overlays sit on most frames; dialogue lines only on the few
/// where they are spoken, so they fall far below this bar.
const OVERLAY_RELATIVE_FACTOR: f64 = 0.5;

/// Edit distance under which two short strings are considered the same overlay.
const OVERLAY_VARIANT_DISTANCE: f64 = 0.34;

/// Upper bound on the character count of an overlay tag.
const OVERLAY_MAX_CHARS: usize = 12;

/// Message shown when the model call for a frame fails.
///
/// Carries the progress made so far, because an inference server that goes
/// quiet midway through a long video leaves the user with no idea how much
/// work has to be redone.
pub fn ocr_failure_message(frame_index: usize, total_frames: usize, time: f64) -> String {
    format!(
        "OCR failed on frame {} of {} (@ {time:.2}s); {} frames were already processed. \
         Check that the inference server is still running, then run again.",
        frame_index + 1,
        total_frames,
        frame_index,
    )
}
