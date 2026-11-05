//! Text-level filtering and the post-processing "semantic" analysis used to
//! separate subtitles from overlays (danmaku bubbles, "IP属地" tags, logos).

use crate::timeline::FrameObservation;
use std::collections::HashMap;

/// Repeated overlay text, with where and how often it was seen.
#[derive(Debug, Clone, PartialEq)]
pub struct OverlayCandidate {
    pub text: String,
    /// Fraction of frames that carried this exact line.
    pub frequency: f64,
    /// Mean line-length share among observations (short overlay vs long cue).
    pub mean_len: f64,
}

/// Statistics accumulated per distinct OCR line across a run.
#[derive(Debug, Default)]
pub struct LineIndex {
    counts: HashMap<String, usize>,
    total: usize,
}

impl LineIndex {
    pub fn build(obs: &[FrameObservation]) -> Self {
        let mut idx = Self {
            total: obs.len(),
            ..Default::default()
        };
        for o in obs {
            for line in &o.lines {
                *idx.counts.entry(line.clone()).or_insert(0) += 1;
            }
        }
        idx
    }

    pub fn total(&self) -> usize {
        self.total
    }

    /// Lines that appear in at least `min_share` of all frames AND persist
    /// nearly unchanged for a long stretch — the signature of a fixed overlay
    /// rather than a line of dialogue.
    ///
    /// Subtitles also repeat (`我問你` can be seen in consecutive frames) but
    /// they do not recur across the whole video; overlays do.
    pub fn overlay_candidates(&self, min_share: f64, min_frames: usize) -> Vec<OverlayCandidate> {
        if self.total == 0 {
            return Vec::new();
        }
        let mut out = Vec::new();
        for (text, &count) in &self.counts {
            let share = count as f64 / self.total as f64;
            if count < min_frames || share < min_share {
                continue;
            }
            let mean_len = mean_line_len(text);
            out.push(OverlayCandidate {
                text: text.clone(),
                frequency: share,
                mean_len,
            });
        }
        out.sort_by(|a, b| b.frequency.total_cmp(&a.frequency));
        out
    }

    /// True when a line recurs far beyond what a dialogue line would.
    pub fn is_overlay(&self, text: &str, min_share: f64, min_frames: usize) -> bool {
        let count = *self.counts.get(text).unwrap_or(&0);
        let share = count as f64 / self.total.max(1) as f64;
        count >= min_frames && share >= min_share
    }
}

fn mean_line_len(line: &str) -> f64 {
    line.chars().count() as f64
}

#[cfg(test)]
mod fallback {}
