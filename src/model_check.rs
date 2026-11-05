//! Compare what a model manifest expects with what the server actually runs.
//!
//! The server reports the file it loaded, while the manifest names a file on
//! disk. Only the file name is compared: the server may report either an
//! absolute path, a relative one, or a bare name, and moving the model around
//! should not make a working setup look broken.

use crate::model::ModelManifest;
use serde::Deserialize;

/// The subset of the server's `/props` response that matters here.
#[derive(Debug, Clone, Deserialize)]
pub struct ServerProps {
    #[serde(default)]
    pub model_path: String,
    #[serde(default)]
    pub model_alias: String,
    #[serde(default)]
    pub model_ftype: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ModelMatch {
    /// The server is running the model the manifest names.
    Matches { file: String },
    /// The server is running something else.
    Mismatch { expected: String, actual: String },
    /// The server did not say which model it loaded.
    Unknown,
}

/// Compare a manifest against the server's reported state.
pub fn compare(manifest: &ModelManifest, props: &ServerProps) -> ModelMatch {
    let expected = file_name_of(&manifest.model_path.display().to_string());
    let actual = file_name_of(&props.model_path);
    if actual.is_empty() {
        // Some builds only fill in the alias; fall back to it.
        let alias = file_name_of(&props.model_alias);
        if alias.is_empty() {
            return ModelMatch::Unknown;
        }
        return classify(expected, alias);
    }
    classify(expected, actual)
}

fn classify(expected: String, actual: String) -> ModelMatch {
    if expected == actual {
        ModelMatch::Matches { file: actual }
    } else {
        ModelMatch::Mismatch { expected, actual }
    }
}

/// Last path component, handling both separators on every platform.
fn file_name_of(path: &str) -> String {
    path.rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Human-readable explanation for a mismatch.
pub fn mismatch_message(manifest: &ModelManifest, actual: &str) -> String {
    format!(
        "the server is running `{actual}` but model `{}` expects `{}`; \
         start the server with the configured model or pick a different --model",
        manifest.id,
        file_name_of(&manifest.model_path.display().to_string()),
    )
}
