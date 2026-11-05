//! Subtitle band localization.
//!
//! Burnt-in subtitles occupy a stable horizontal strip, usually somewhere in the
//! lower half. We find it *without* OCR by looking for text-like strokes:
//! bright pixels surrounded by strong local contrast, aggregated across many
//! sampled frames. Permanently overlaid watermarks/logos also score on
//! brightness, so the scoring additionally favours rows whose bright content
//! *changes over time* (subtitle lines come and go; logos stay identical).

/// A vertical slice of the frame (in pixel coordinates of the working image).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Band {
    pub y0: u32,
    pub y1: u32,
}

impl Band {
    pub fn height(&self) -> u32 {
        self.y1.saturating_sub(self.y0)
    }
}

/// Aggregated per-row statistics used to score candidate subtitle rows.
#[derive(Debug, Default)]
pub struct RowStats {
    /// Mean fraction of "text-like" (bright + edged) pixels per row.
    pub text_density: Vec<f32>,
    /// Mean pixel change versus the previous sample, per row (0..255).
    pub motion: Vec<f32>,
}

impl RowStats {
    pub fn new(height: u32) -> Self {
        Self {
            text_density: vec![0.0; height as usize],
            motion: vec![0.0; height as usize],
        }
    }

    pub fn add_frame(&mut self, gray: &Gray, prev: Option<&Gray>) {
        let (w, h) = gray.dims();
        let rows = (h as usize).min(self.text_density.len());
        for y in 0..rows {
            let row = &gray.px[y * w..(y + 1) * w];
            let mut hits = 0usize;
            let mut total = 0usize;
            for (x, &v) in row.iter().enumerate() {
                if v < 190 {
                    continue;
                }
                // horizontal contrast around this pixel
                let l = if x > 0 { row[x - 1] as i32 } else { v as i32 };
                let r = if x + 1 < w {
                    row[x + 1] as i32
                } else {
                    v as i32
                };
                let contrast = ((v as i32) - (l + r) / 2).unsigned_abs();
                // vertical contrast against the neighbouring rows
                let up = neighbor(gray, x, y as i32 - 1, v);
                let down = neighbor(gray, x, y as i32 + 1, v);
                let vcontrast = ((v as i32) - (up + down) / 2).unsigned_abs();
                total += 1;
                if contrast + vcontrast > 60 && (contrast > 20 || vcontrast > 20) {
                    hits += 1;
                }
            }
            if total > 0 {
                self.text_density[y] += hits as f32 / total as f32;
            }
            if let Some(p) = prev {
                let prow = &p.px[y * w..(y + 1) * w];
                let mut sum = 0u64;
                for (&a, &b) in row.iter().zip(prow.iter()) {
                    sum += a.abs_diff(b) as u64;
                }
                self.motion[y] += sum as f32 / total.max(1) as f32;
            }
        }
    }

    pub fn finalize(&mut self, frames: usize) {
        if frames == 0 {
            return;
        }
        for v in self.text_density.iter_mut() {
            *v /= frames as f32;
        }
        for v in self.motion.iter_mut() {
            *v /= frames as f32;
        }
    }
}

fn neighbor(g: &Gray, x: usize, y: i32, fallback: u8) -> i32 {
    if y < 0 || y as usize >= g.dims().1 {
        return fallback as i32;
    }
    g.px[y as usize * g.dims().0 + x] as i32
}

/// Grayscale working image plus dimensions, kept flat for speed.
pub struct Gray {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

impl Gray {
    pub fn new(img: &image::RgbImage) -> Self {
        let (w, h) = img.dimensions();
        let mut px = Vec::with_capacity((w * h) as usize);
        for p in img.pixels() {
            let l = (0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32) as u8;
            px.push(l);
        }
        Self {
            w: w as usize,
            h: h as usize,
            px,
        }
    }
    fn dims(&self) -> (usize, usize) {
        (self.w, self.h)
    }
}

/// Locate the subtitle band given the aggregated row statistics.
///
/// Strategy: score each row in the lower part of the frame, pick the peak row,
/// then grow a contiguous run of rows around it whose score is a meaningful
/// fraction of the peak. Returns `None` when no row shows enough evidence.
pub fn locate_band(stats: &RowStats, height: u32, min_height: u32) -> Option<Band> {
    let h = height as usize;
    if stats.text_density.len() < h {
        return None;
    }
    // Rows are only relevant in the bottom 60% of the picture.
    let search_start = (h as f32 * 0.40) as usize;

    let score: Vec<f32> = (0..h)
        .map(|y| {
            // text density is the primary signal, motion breaks ties against static logos
            stats.text_density[y] + 0.004 * stats.motion[y]
        })
        .collect();

    let (mut best_y, mut best_score) = (0usize, 0.0f32);
    for y in search_start..h {
        if score[y] > best_score {
            best_score = score[y];
            best_y = y;
        }
    }
    // A genuine text row should have a small but non-trivial bright fraction.
    if best_score < 0.004 {
        return None;
    }

    let threshold = best_score * 0.35;
    let mut y0 = best_y;
    while y0 > search_start && score[y0 - 1] >= threshold {
        y0 -= 1;
    }
    let mut y1 = best_y;
    while y1 + 1 < h && score[y1 + 1] >= threshold {
        y1 += 1;
    }

    // Padding: subtitles routinely have descenders and outlines that fail the
    // row threshold; widen by ~10% on each side.
    let pad = ((y1 - y0) as f32 * 0.10).max(6.0) as usize;
    let y0 = y0.saturating_sub(pad);
    let y1 = (y1 + pad).min(h - 1);
    let mut band = Band {
        y0: y0 as u32,
        y1: (y1 + 1) as u32,
    };
    if band.height() < min_height {
        let center = (band.y0 + band.y1) / 2;
        let half = min_height / 2;
        let start = center.saturating_sub(half);
        let end = (center + half).min(height);
        band = Band { y0: start, y1: end };
    }
    Some(band)
}

/// Default high-quality fallback when band detection is inconclusive:
/// the classic "bottom third" used by most subtitles.
pub fn default_band(height: u32) -> Band {
    Band {
        y0: (height as f32 * 0.70) as u32,
        y1: height,
    }
}
