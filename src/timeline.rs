//! Turn frame-level OCR results into a deduplicated subtitle timeline.

use serde::{Deserialize, Serialize};

/// One OCR observation for a single sampled frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameObservation {
    pub index: usize,
    /// Timestamp in seconds, on the sampling grid.
    pub t: f64,
    /// Raw model output.
    pub raw: String,
    /// Cleaned, non-empty output lines.
    pub lines: Vec<String>,
    /// CPU-side processing + model latency in milliseconds.
    pub latency_ms: u64,
}

/// One subtitle entry in the final timeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cue {
    /// Canonical text (lines joined with '\n').
    pub text: String,
    pub start: f64,
    pub end: f64,
    /// Number of frames that supported this cue.
    pub support: usize,
}

#[derive(Debug, Clone)]
pub struct TimelineConfig {
    /// Levenshtein distance below which two texts count as the same subtitle.
    pub similarity_threshold: f64,
    /// Consecutive empty frames at most this long are bridged inside one cue.
    pub min_gap_frames: usize,
    /// Text changes that last at most this many frames are treated as flicker.
    pub flicker_window: usize,
    /// Never emit a cue shorter than this (seconds).
    pub min_cue_secs: f64,
}

impl Default for TimelineConfig {
    fn default() -> Self {
        Self {
            similarity_threshold: 0.25,
            min_gap_frames: 2,
            flicker_window: 1,
            min_cue_secs: 1.0,
        }
    }
}

impl TimelineConfig {
    fn same(&self, a: &str, b: &str) -> bool {
        distance(a, b) <= self.similarity_threshold
            // A subtitle split into two lines by the model is the same cue as
            // the single-line reading, even when the plain edit distance is
            // dominated by the prepended line.
            || (a.lines().count() != b.lines().count() && lines_compatible(a, b))
    }
}

/// True when either text's lines contain the other's as a complete line
/// sequence, i.e. one reading is a prefix/suffix extension of the other.
fn lines_compatible(a: &str, b: &str) -> bool {
    let al: Vec<&str> = a.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let bl: Vec<&str> = b.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if al.is_empty() || bl.is_empty() {
        return false;
    }
    let (short, long) = if al.len() <= bl.len() {
        (al, bl)
    } else {
        (bl, al)
    };
    long.windows(short.len()).any(|w| w == short.as_slice())
}

/// Normalise a raw OCR result into display lines.
pub fn clean_lines(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        // Drop markdown/code fences the model likes to invent, including the
        // opening fence which carries a language tag (```text). Check before
        // stripping backticks, otherwise the fence becomes a bare word.
        if trimmed.starts_with("```") || trimmed == "---" {
            continue;
        }
        let l = trimmed.trim_matches(['"', '“', '”', '\'', '`']).trim();
        if l.is_empty() {
            continue;
        }
        out.push(l.to_string());
    }
    out
}

fn normalized(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Levenshtein-based normalised distance in `0.0..=1.0`.
pub fn distance(a: &str, b: &str) -> f64 {
    let a: Vec<char> = normalized(a).chars().collect();
    let b: Vec<char> = normalized(b).chars().collect();
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let max = a.len().max(b.len()) as f64;
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()] as f64 / max
}

/// Internal segment: a closed run of frames that showed one subtitle.
#[derive(Debug)]
struct Segment {
    /// Every distinct reading and how often it appeared in this segment.
    votes: std::collections::HashMap<String, usize>,
    start: f64,
    end: f64,
    frames: usize,
}

impl Segment {
    fn new(text: String, t: f64, end: f64) -> Self {
        let mut votes = std::collections::HashMap::new();
        votes.insert(text, 1);
        Self {
            votes,
            start: t,
            end,
            frames: 1,
        }
    }

    fn record(&mut self, text: String, end: f64) {
        *self.votes.entry(text).or_insert(0) += 1;
        self.end = end;
        self.frames += 1;
    }

    /// Majority reading; ties go to the variant with more lines, since a
    /// dropped second line is the common OCR failure and must not win.
    fn canonical(&self) -> String {
        let mut best: Option<(&String, usize)> = None;
        for (text, count) in &self.votes {
            let better = match best {
                None => true,
                Some((best_text, best_count)) => {
                    count > &best_count
                        || (count == &best_count
                            && text.lines().count() > best_text.lines().count())
                }
            };
            if better {
                best = Some((text, *count));
            }
        }
        best.map(|(t, _)| t.clone()).unwrap_or_default()
    }
}

/// Group consecutive frames into cues.
///
/// Runs of frames showing (nearly) the same text form one cue. Short OCR
/// drop-outs inside a cue are bridged, one-sample blips are discarded, and
/// real gaps end the cue.
pub fn build_timeline(obs: &[FrameObservation], frame_dt: f64, cfg: &TimelineConfig) -> Vec<Cue> {
    if obs.is_empty() {
        return Vec::new();
    }
    let max_gap = cfg.min_gap_frames as i64;
    let mut segments: Vec<Segment> = Vec::new();
    let mut current: Option<Segment> = None;
    let mut empty_run = 0i64;

    for o in obs {
        let text = o.lines.join("\n");
        let frame_end = o.t + frame_dt;
        if text.is_empty() {
            empty_run += 1;
            // A run longer than the bridge budget closes the open segment.
            if empty_run > max_gap {
                if let Some(seg) = current.take() {
                    if !seg.votes.is_empty() {
                        segments.push(seg);
                    }
                }
            }
            continue;
        }

        let extends = current
            .as_ref()
            .map(|seg| {
                // Gap between this frame and the previous non-empty one, in frames.
                let gap = empty_run;
                gap <= max_gap && seg.votes.keys().any(|seen| cfg.same(seen, &text))
            })
            .unwrap_or(false);

        if extends {
            let mut seg = current.take().unwrap();
            seg.record(text, frame_end);
            current = Some(seg);
        } else {
            if let Some(seg) = current.take() {
                segments.push(seg);
            }
            current = Some(Segment::new(text, o.t, frame_end));
        }
        empty_run = 0;
    }
    if let Some(seg) = current.take() {
        segments.push(seg);
    }

    // Post-pass: drop one-sample blips whose neighbours read the same text.
    let mut merged: Vec<Segment> = Vec::new();
    for seg in segments {
        let canonical = seg.canonical();
        let is_blip = seg.frames <= cfg.flicker_window.max(1)
            && merged
                .last()
                .map(|prev| cfg.same(&prev.canonical(), &canonical))
                .unwrap_or(false);
        if is_blip {
            continue;
        }
        merged.push(seg);
    }

    let mut cues: Vec<Cue> = merged
        .into_iter()
        .map(|s| {
            let end = s.end;
            (s.canonical(), (s.start, end, s.frames))
        })
        .filter(|(text, _)| !text.is_empty())
        .map(|(text, (start, end, support))| Cue {
            text,
            start,
            end,
            support,
        })
        .collect();

    extend_to_min_duration(&mut cues, cfg.min_cue_secs);
    cues
}

/// Grow cues shorter than `min` seconds so they stay on screen long enough.
///
/// The start time is kept (it is the moment the subtitle was seen), and the
/// cue is lengthened only into free time: never past the start of the next
/// cue, and never past the end of the media.
fn extend_to_min_duration(cues: &mut [Cue], min: f64) {
    let n = cues.len();
    for i in 0..n {
        let start = cues[i].start;
        let mut end = cues[i].end;
        if end - start + 1e-9 >= min {
            continue;
        }
        // The next cue may start at the very next sample; leave a hair of gap
        // so the two cues never share a timestamp.
        let limit = if i + 1 < n {
            cues[i + 1].start
        } else {
            f64::INFINITY
        };
        end = (start + min).min(limit);
        if i + 1 < n && start + min > limit {
            end = limit;
        }
        // Never shrink a cue that was already longer than the target.
        cues[i].end = end.max(cues[i].end);
    }
}

/// Render cues as SRT with hh:mm:ss,mmm timestamps.
pub fn to_srt(cues: &[Cue]) -> String {
    let mut out = String::new();
    for (i, c) in cues.iter().enumerate() {
        out.push_str(&format!(
            "{}\n{} --> {}\n{}\n\n",
            i + 1,
            fmt_ts(c.start),
            fmt_ts(c.end),
            c.text
        ));
    }
    out
}

pub fn fmt_ts(secs: f64) -> String {
    let total_ms = (secs.max(0.0) * 1000.0).round() as u64;
    let ms = total_ms % 1000;
    let s = (total_ms / 1000) % 60;
    let m = (total_ms / 60_000) % 60;
    let h = total_ms / 3_600_000;
    format!("{h:02}:{m:02}:{s:02},{ms:03}")
}
