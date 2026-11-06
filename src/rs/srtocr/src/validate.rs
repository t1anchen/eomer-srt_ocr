//! Validation of user-supplied options.
//!
//! Options are checked before any work starts so that a typo fails fast with a
//! readable message instead of producing a broken run halfway through a video.

use anyhow::{Result, bail};

/// Sampling rate bounds (frames per second).
pub const MIN_FPS: f64 = 0.1;
pub const MAX_FPS: f64 = 10.0;
/// Working width bounds (pixels).
pub const MIN_WIDTH: u32 = 320;
pub const MAX_WIDTH: u32 = 3840;
/// Maximum input file size accepted for a still image (bytes).
pub const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;

pub fn check_fps(fps: f64) -> Result<()> {
    if !fps.is_finite() || !(MIN_FPS..=MAX_FPS).contains(&fps) {
        bail!(
            "--fps must be between {MIN_FPS} and {MAX_FPS}, got {fps}; \
             omit the flag to let the sampling rate be chosen automatically"
        );
    }
    Ok(())
}

pub fn check_width(width: u32) -> Result<()> {
    if !(MIN_WIDTH..=MAX_WIDTH).contains(&width) {
        bail!("--width must be between {MIN_WIDTH} and {MAX_WIDTH}, got {width}");
    }
    Ok(())
}

pub fn check_similarity(similarity: f64) -> Result<()> {
    if !similarity.is_finite() || !(0.0..=1.0).contains(&similarity) {
        bail!("--similarity must be between 0.0 and 1.0, got {similarity}");
    }
    Ok(())
}

pub fn check_min_cue_duration(seconds: f64) -> Result<()> {
    if !seconds.is_finite() || !(0.0..=60.0).contains(&seconds) {
        bail!("--min-cue-duration must be between 0 and 60 seconds, got {seconds}");
    }
    Ok(())
}

/// Reject inputs that are absurdly large for an image path.
pub fn check_image_size(bytes: u64) -> Result<()> {
    if bytes > MAX_IMAGE_BYTES {
        bail!(
            "image is {} MiB, above the {} MiB limit",
            bytes / (1024 * 1024),
            MAX_IMAGE_BYTES / (1024 * 1024)
        );
    }
    Ok(())
}

/// Reject media shorter than one sampling step, which cannot yield a cue.
pub fn check_duration(duration_secs: f64) -> Result<()> {
    if !duration_secs.is_finite() || duration_secs < 0.5 {
        bail!(
            "input is too short to extract subtitles from ({duration_secs:.2}s); \
             needs at least 0.5s of video"
        );
    }
    Ok(())
}
