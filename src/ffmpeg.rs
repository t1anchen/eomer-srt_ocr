//! Thin wrapper around ffmpeg / ffprobe.
//!
//! The binaries are discovered from, in order: an explicit path in config, the
//! `FFMPEG` / `FFPROBE` environment variables, `ffmpeg` / `ffprobe` on PATH,
//! then the common local install location on this machine.

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Deserialize)]
pub struct MediaInfo {
    pub width: u32,
    pub height: u32,
    pub duration_secs: f64,
    pub has_audio: bool,
    /// Average frame rate if known (used to pick the sampling rate).
    pub fps: Option<f64>,
}

/// ffmpeg tooling resolved to concrete executable paths.
#[derive(Debug, Clone)]
pub struct Ffmpeg {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

impl Ffmpeg {
    pub fn discover(ffmpeg: Option<&Path>, ffprobe: Option<&Path>) -> Result<Self> {
        let ffmpeg = ffmpeg
            .map(Path::to_path_buf)
            .or_else(|| std::env::var_os("FFMPEG").map(PathBuf::from))
            .or_else(|| which("ffmpeg"))
            .or_else(|| fallback_bin("ffmpeg").ok())
            .ok_or_else(|| anyhow!("ffmpeg not found (set --ffmpeg or FFMPEG)"))?;
        let ffprobe = ffprobe
            .map(Path::to_path_buf)
            .or_else(|| std::env::var_os("FFPROBE").map(PathBuf::from))
            .or_else(|| which("ffprobe"))
            .or_else(|| {
                // Derive from the explicit ffmpeg path's sibling directory when possible.
                ffmpeg.parent().map(|d| {
                    d.join(if cfg!(windows) {
                        "ffprobe.exe"
                    } else {
                        "ffprobe"
                    })
                })
            })
            .filter(|p| p.exists())
            .or_else(|| fallback_bin("ffprobe").ok())
            .ok_or_else(|| anyhow!("ffprobe not found (set --ffprobe or FFPROBE)"))?;
        Ok(Self { ffmpeg, ffprobe })
    }

    pub fn probe(&self, input: &Path) -> Result<MediaInfo> {
        let out = Command::new(&self.ffprobe)
            .args([
                "-v",
                "error",
                "-show_entries",
                "format=duration",
                "-show_entries",
                "stream=codec_type,width,height,avg_frame_rate,r_frame_rate",
                "-of",
                "json",
            ])
            .arg(input)
            .output()
            .with_context(|| format!("running ffprobe on {}", input.display()))?;
        if !out.status.success() {
            bail!("ffprobe failed: {}", String::from_utf8_lossy(&out.stderr));
        }
        let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
        let duration_secs = v["format"]["duration"]
            .as_str()
            .and_then(|s| s.parse::<f64>().ok())
            .ok_or_else(|| anyhow!("ffprobe: missing duration"))?;

        let streams = v["streams"].as_array().cloned().unwrap_or_default();
        let mut width = 0u32;
        let mut height = 0u32;
        let mut fps = None;
        let mut has_audio = false;
        for s in &streams {
            match s["codec_type"].as_str() {
                Some("video") if width == 0 => {
                    width = s["width"].as_u64().unwrap_or(0) as u32;
                    height = s["height"].as_u64().unwrap_or(0) as u32;
                    fps = parse_rate(s["avg_frame_rate"].as_str())
                        .or_else(|| parse_rate(s["r_frame_rate"].as_str()));
                }
                Some("audio") => has_audio = true,
                _ => {}
            }
        }
        if width == 0 || height == 0 {
            bail!("ffprobe: no video stream found");
        }
        Ok(MediaInfo {
            width,
            height,
            duration_secs,
            has_audio,
            fps,
        })
    }

    /// Grab a single frame at `t` seconds, scaled so its width is at most
    /// `max_width` (aspect preserved). Returns decoded RGB pixels.
    pub fn frame(&self, input: &Path, t: f64, max_width: u32) -> Result<image::RgbImage> {
        let scale = format!("scale={max_width}:-2:flags=bicubic");
        let out = Command::new(&self.ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-ss"])
            .arg(format!("{t:.3}"))
            .arg("-i")
            .arg(input)
            .args([
                "-frames:v",
                "1",
                "-vf",
                &scale,
                "-f",
                "image2pipe",
                "-c:v",
                "png",
                "-",
            ])
            .output()
            .with_context(|| format!("ffmpeg extracting frame at {t:.2}s"))?;
        if !out.status.success() {
            bail!(
                "ffmpeg failed at {t:.2}s: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let img = image::load_from_memory(&out.stdout)
            .with_context(|| format!("decoding frame at {t:.2}s"))?;
        Ok(img.to_rgb8())
    }
}

fn parse_rate(s: Option<&str>) -> Option<f64> {
    let s = s?;
    let (num, den) = s.split_once('/')?;
    let num: f64 = num.parse().ok()?;
    let den: f64 = den.parse().ok()?;
    if den == 0.0 || num == 0.0 || !num.is_finite() {
        return None;
    }
    Some(num / den)
}

fn which(bin: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        format!("{bin}.exe")
    } else {
        bin.to_string()
    };
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(&exe))
            .find(|p| p.is_file())
    })
}

fn fallback_bin(name: &str) -> Result<PathBuf> {
    let exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let candidates: Vec<PathBuf> = if cfg!(windows) {
        vec![
            PathBuf::from(r"C:\Users\austi\lib\ffmpeg-v8.1\bin").join(&exe),
            PathBuf::from(r"C:\ffmpeg\bin").join(&exe),
        ]
    } else {
        vec![PathBuf::from("/usr/bin").join(&exe)]
    };
    candidates
        .into_iter()
        .find(|p| p.is_file())
        .ok_or_else(|| anyhow!("{name} not found"))
}
