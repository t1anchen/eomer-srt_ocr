//! Model manifest: one JSON descriptor per OCR model on disk.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelManifest {
    /// Stable slug used on the command line (`--model dots-ocr`).
    pub id: String,
    pub display_name: String,
    /// Backend kind: `llamacpp-vlm` for a llama-server managed by this tool.
    #[serde(default = "default_backend")]
    pub backend: String,
    /// Path to the main GGUF file.
    pub model_path: PathBuf,
    /// Vision projector; required for VLM models.
    #[serde(default)]
    pub mmproj_path: Option<PathBuf>,
    /// Prompt sent with every frame.
    #[serde(default)]
    pub prompt: Option<String>,
    /// llama-server binary used to launch this model.
    #[serde(default)]
    pub llama_server: Option<PathBuf>,
    /// Extra llama-server arguments (e.g. `["-ngl", "99"]`).
    #[serde(default)]
    pub args: Vec<String>,
    /// Last known serving port; informational.
    #[serde(default)]
    pub last_port: Option<u16>,
    /// "bottom third" style hint: fraction of frame height where subtitles start.
    #[serde(default)]
    pub subtitle_region_hint: Option<f32>,
}

fn default_backend() -> String {
    "llamacpp-vlm".into()
}

impl ModelManifest {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading model manifest {}", path.display()))?;
        let m: ModelManifest = serde_json::from_str(&raw)
            .with_context(|| format!("parsing model manifest {}", path.display()))?;
        m.validate()?;
        Ok(m)
    }

    pub fn validate(&self) -> Result<()> {
        if self.id.trim().is_empty() {
            bail!("manifest: `id` must not be empty");
        }
        if !self.model_path.is_file() {
            bail!(
                "manifest: model_path not found: {}",
                self.model_path.display()
            );
        }
        match &self.mmproj_path {
            Some(p) if !p.is_file() => bail!("manifest: mmproj_path not found: {}", p.display()),
            None if self.backend == "llamacpp-vlm" => {
                bail!("manifest: backend `llamacpp-vlm` requires mmproj_path")
            }
            _ => {}
        }
        Ok(())
    }

    /// Scan a directory for `*.json` model manifests.
    pub fn scan_dir(dir: &Path) -> Result<Vec<Self>> {
        let mut out = Vec::new();
        if !dir.exists() {
            return Ok(out);
        }
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                match ModelManifest::load(&path) {
                    Ok(m) => out.push(m),
                    Err(e) => eprintln!("warning: skipping {}: {e:#}", path.display()),
                }
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    pub fn find(models: &[ModelManifest], id: &str) -> Result<Self> {
        models.iter().find(|m| m.id == id).cloned().ok_or_else(|| {
            let known: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
            anyhow::anyhow!("model `{id}` not found; known models: {known:?}")
        })
    }
}
