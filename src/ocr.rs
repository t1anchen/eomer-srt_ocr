//! Client for a llama.cpp server exposing the OpenAI-compatible chat API.

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::Deserialize;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct LlamaClient {
    http: reqwest::blocking::Client,
    base_url: String,
    model: Option<String>,
    prompt: String,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: Message,
}

#[derive(Debug, Deserialize)]
struct Message {
    #[serde(default)]
    content: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
}

#[derive(Debug, Clone)]
pub struct OcrReply {
    pub text: String,
    pub usage: Option<Usage>,
}

pub const DEFAULT_PROMPT: &str = "请只输出图片中的字幕文字，按原行逐行输出；没有文字时输出空内容。不要解释、不要翻译、不要添加任何其他标记。";

/// Prompt for a standalone image where we want all of its text, not only
/// subtitles. dots.ocr (and most OCR VLMs) return nothing when told to look
/// for subtitles in an image that has none.
pub const DEFAULT_IMAGE_PROMPT: &str = "OCR";

static ENGINE: std::sync::OnceLock<base64::engine::GeneralPurpose> = std::sync::OnceLock::new();

fn engine() -> &'static base64::engine::GeneralPurpose {
    ENGINE.get_or_init(|| B64)
}

impl LlamaClient {
    pub fn new(base_url: impl Into<String>, model: Option<String>, prompt: Option<String>) -> Self {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .expect("building http client");
        Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            model,
            prompt: prompt.unwrap_or_else(|| DEFAULT_PROMPT.to_string()),
        }
    }

    /// `GET /health` — true when the server reports readiness.
    pub fn healthy(&self) -> bool {
        self.http
            .get(format!("{}/health", self.base_url))
            .timeout(Duration::from_secs(3))
            .send()
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    /// Send one RGB image and return the recognised text.
    pub fn ocr_image(&self, img: &image::RgbImage) -> Result<OcrReply> {
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img.clone())
            .write_to(&mut buf, image::ImageFormat::Png)
            .context("encoding crop to PNG")?;
        let data_url = format!(
            "data:image/png;base64,{}",
            engine().encode(buf.into_inner())
        );
        let body = serde_json::json!({
            "temperature": 0,
            "max_tokens": 512,
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "image_url", "image_url": {"url": data_url}},
                    {"type": "text", "text": self.prompt},
                ]
            }],
        });
        let mut body = body;
        if let Some(model) = &self.model {
            body["model"] = serde_json::Value::String(model.clone());
        }

        let resp = self
            .http
            .post(format!("{}/v1/chat/completions", self.base_url))
            .json(&body)
            .send()
            .context("calling llama-server")?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().unwrap_or_default();
            bail!("llama-server returned {status}: {text}");
        }
        let parsed: ChatResponse = resp.json().context("decoding chat response")?;
        let choice = parsed
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("llama-server returned no choices"))?;
        Ok(OcrReply {
            text: choice.message.content,
            usage: parsed.usage,
        })
    }
}
