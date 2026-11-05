//! `eomer-srt-ocr` command line entry point.

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use eomer_srt_ocr::model::ModelManifest;
use eomer_srt_ocr::ocr::LlamaClient;
use eomer_srt_ocr::{PipelineConfig, PipelineResult, run_pipeline};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "eomer-srt-ocr",
    version,
    about = "Extract burnt-in subtitles from video using a local llama.cpp VLM"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// OCR a single image file.
    Image {
        /// Input image path (png/jpeg/webp).
        input: PathBuf,
        /// Output directory (default: ./out/<image-stem>).
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// llama-server base URL.
        #[arg(long, default_value = "http://127.0.0.1:8080")]
        server: String,
        /// Max width the image is scaled to before OCR.
        #[arg(long, default_value_t = 1920)]
        width: u32,
        /// Prompt override.
        #[arg(long)]
        prompt: Option<String>,
        /// Write a machine-readable summary.
        #[arg(long)]
        json: bool,
    },
    /// Extract subtitles from a video file.
    Video {
        /// Input video path.
        input: PathBuf,
        /// Output directory (default: ./out/<video-stem>).
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// llama-server base URL.
        #[arg(long, default_value = "http://127.0.0.1:8080")]
        server: String,
        /// Model name forwarded to the API.
        #[arg(long)]
        model: Option<String>,
        /// Model manifest JSON (validates GGUF/mmproj and fills defaults).
        #[arg(long)]
        manifest: Option<PathBuf>,
        /// Directory containing `*.json` manifests; `--model` matches by id.
        #[arg(long)]
        model_dir: Option<PathBuf>,
        /// Working resolution width for frames.
        #[arg(long, default_value_t = 1280)]
        width: u32,
        /// Sampling rate override (frames per second). Omitted = adaptive.
        #[arg(long)]
        fps: Option<f64>,
        /// Number of frames used to localise the subtitle band.
        #[arg(long, default_value_t = 24)]
        probe_frames: usize,
        /// Prompt override (useful for per-model tuning).
        #[arg(long)]
        prompt: Option<String>,
        /// Overlay lines to strip, e.g. a danmaku watermark text.
        #[arg(long = "ignore", value_name = "TEXT")]
        ignore: Vec<String>,
        /// Disable the built-in danmaku/watermark occlusion masks.
        #[arg(long)]
        no_masks: bool,
        /// Enable pixel-level overlay masking (grid OCR + black rectangles).
        /// Off by default; text post-processing handles overlays instead.
        #[arg(long)]
        mask: bool,
        /// Text-similarity threshold for merging near-identical lines (0 = exact).
        #[arg(long, default_value_t = 0.25, allow_hyphen_values = true)]
        similarity: f64,
        /// Minimum display duration of a cue in seconds. Short cues are
        /// extended into free time, never over the next cue.
        #[arg(long, default_value_t = 1.0, allow_hyphen_values = true)]
        min_cue_duration: f64,
        /// ffmpeg executable path.
        #[arg(long)]
        ffmpeg: Option<PathBuf>,
        /// ffprobe executable path.
        #[arg(long)]
        ffprobe: Option<PathBuf>,
        /// Write a machine-readable run summary.
        #[arg(long)]
        json: bool,
    },
    /// Check that the llama-server is reachable.
    Health {
        #[arg(long, default_value = "http://127.0.0.1:8080")]
        server: String,
    },
    /// List model manifests found in a directory.
    Models {
        #[arg(short, long)]
        dir: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Image {
            input,
            out,
            server,
            width,
            prompt,
            json,
        } => run_image_cmd(input, out, server, width, prompt, json),
        Cmd::Video {
            input,
            out,
            server,
            model,
            manifest,
            model_dir,
            width,
            fps,
            probe_frames,
            prompt,
            ignore,
            no_masks,
            mask,
            similarity,
            min_cue_duration,
            ffmpeg,
            ffprobe,
            json,
        } => run_video(VideoArgs {
            input,
            out,
            server,
            model,
            manifest,
            model_dir,
            width,
            fps,
            probe_frames,
            prompt,
            ignore,
            no_masks,
            mask,
            similarity,
            min_cue_duration,
            ffmpeg,
            ffprobe,
            json,
        }),
        Cmd::Health { server } => {
            let client = LlamaClient::new(server.clone(), None, None);
            if client.healthy() {
                println!("ok: {server}");
                Ok(())
            } else {
                bail!("llama-server not reachable at {server}")
            }
        }
        Cmd::Models { dir } => {
            let dir = dir.unwrap_or_else(|| PathBuf::from("models"));
            let found = ModelManifest::scan_dir(&dir)?;
            if found.is_empty() {
                println!("no manifests in {}", dir.display());
            }
            for m in found {
                println!("{}\t{}\t{}", m.id, m.display_name, m.model_path.display());
            }
            Ok(())
        }
    }
}

struct VideoArgs {
    input: PathBuf,
    out: Option<PathBuf>,
    server: String,
    model: Option<String>,
    manifest: Option<PathBuf>,
    model_dir: Option<PathBuf>,
    width: u32,
    fps: Option<f64>,
    probe_frames: usize,
    prompt: Option<String>,
    ignore: Vec<String>,
    no_masks: bool,
    mask: bool,
    similarity: f64,
    min_cue_duration: f64,
    ffmpeg: Option<PathBuf>,
    ffprobe: Option<PathBuf>,
    json: bool,
}

fn run_video(args: VideoArgs) -> Result<()> {
    // Validate options before touching the filesystem so a typo is reported as
    // a typo, not as a missing input file.
    use eomer_srt_ocr::validate;
    validate::check_width(args.width)?;
    if let Some(fps) = args.fps {
        validate::check_fps(fps)?;
    }
    validate::check_similarity(args.similarity)?;
    validate::check_min_cue_duration(args.min_cue_duration)?;

    if !args.input.is_file() {
        bail!("input not found: {}", args.input.display());
    }
    // Manifest resolution: explicit file, or id lookup inside a directory.
    let manifest = match (&args.manifest, &args.model_dir, &args.model) {
        (Some(path), _, _) => Some(ModelManifest::load(path)?),
        (None, Some(dir), Some(id)) => {
            Some(ModelManifest::find(&ModelManifest::scan_dir(dir)?, id)?)
        }
        _ => None,
    };
    let prompt = args
        .prompt
        .clone()
        .or_else(|| manifest.as_ref().and_then(|m| m.prompt.clone()));

    let ocr = LlamaClient::new(args.server.clone(), args.model.clone(), prompt);
    if !ocr.healthy() {
        bail!(
            "llama-server not reachable at {} — start it first or pass --server",
            args.server
        );
    }

    let ff =
        eomer_srt_ocr::ffmpeg::Ffmpeg::discover(args.ffmpeg.as_deref(), args.ffprobe.as_deref())?;
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let out_dir = eomer_srt_ocr::paths::resolve_output_dir(
        &args.input,
        args.out.as_deref(),
        &cwd,
    );
    eprintln!("[output: {}]", out_dir.display());
    warn_if_populated(&out_dir);

    let cfg = PipelineConfig {
        input: args.input.clone(),
        output_dir: out_dir.clone(),
        max_width: args.width,
        fps: args.fps,
        probe_frames: args.probe_frames,
        timeline: eomer_srt_ocr::timeline::TimelineConfig {
            similarity_threshold: args.similarity,
            min_cue_secs: args.min_cue_duration,
            ..Default::default()
        },
        band_override: None,
        model_name: args.model.clone(),
        prompt: None,
        ignore_patterns: args.ignore.clone(),
        // No masks unless explicitly requested: masks are geometry-dependent.
        masks: Vec::new(),
        auto_mask: args.mask && !args.no_masks,
        overlay_min_share: 0.10,
        overlay_min_frames: 10,
    };

    let result: PipelineResult = run_pipeline(&cfg, &ff, &ocr, &mut |msg| {
        eprintln!("[{msg}]");
    })?;

    println!(
        "done: {} cues, {}/{} frames with text, {:.1}s elapsed",
        result.cues.len(),
        result.frames_with_text,
        result.frames_processed,
        result.elapsed_secs
    );
    println!("srt:  {}", result.srt_path.display());
    println!("jsonl: {}", result.jsonl_path.display());
    if args.json {
        let summary_path = out_dir.join("result.json");
        std::fs::write(&summary_path, serde_json::to_string_pretty(&result)?)?;
        println!("summary: {}", summary_path.display());
    }
    Ok(())
}

fn run_image_cmd(
    input: PathBuf,
    out: Option<PathBuf>,
    server: String,
    width: u32,
    prompt: Option<String>,
    json: bool,
) -> Result<()> {
    eomer_srt_ocr::validate::check_width(width)?;
    if !input.is_file() {
        bail!("input not found: {}", input.display());
    }
    let bytes = std::fs::metadata(&input).map(|m| m.len()).unwrap_or(0);
    eomer_srt_ocr::validate::check_image_size(bytes)?;
    // A standalone image is not a subtitle frame: use the general OCR prompt
    // unless the caller supplied one.
    let ocr = LlamaClient::new(
        server.clone(),
        None,
        Some(prompt.unwrap_or_else(|| eomer_srt_ocr::ocr::DEFAULT_IMAGE_PROMPT.to_string())),
    );
    if !ocr.healthy() {
        bail!("llama-server not reachable at {server} — start it first or pass --server");
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let out_dir = eomer_srt_ocr::paths::resolve_output_dir(&input, out.as_deref(), &cwd);
    eprintln!("[output: {}]", out_dir.display());
    warn_if_populated(&out_dir);
    let cfg = eomer_srt_ocr::image_ocr::ImageConfig {
        input,
        output_dir: out_dir.clone(),
        max_width: width,
    };
    let result = eomer_srt_ocr::image_ocr::run_image(&cfg, &ocr)?;
    println!(
        "done: {} lines, {}x{} in {} ms",
        result.lines.len(),
        result.width,
        result.height,
        result.latency_ms
    );
    for line in &result.lines {
        println!("{line}");
    }
    println!("txt: {}", result.txt_path.display());
    if json {
        let path = out_dir.join("result.json");
        std::fs::write(&path, serde_json::to_string_pretty(&result)?)?;
        println!("summary: {}", path.display());
    }
    Ok(())
}

/// Tell the user when results are about to overwrite a populated directory.
fn warn_if_populated(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let count = entries.filter(|e| e.is_ok()).count();
    if count > 0 {
        eprintln!(
            "[output: {} already contains {} entries; files will be overwritten]",
            dir.display(),
            count
        );
    }
}
