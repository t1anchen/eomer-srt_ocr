//! Resumable-run bookkeeping.
//!
//! A long video can take a while, and a crashed or interrupted run should not
//! start over from zero. After every batch of frames a small progress record is
//! written next to the results; on the next start it is read back and used to
//! skip the frames that are already done.
//!
//! A record is only valid when it was produced with the same settings that
//! would be used now — a different width, sampling rate or input must start
//! fresh, otherwise the old and new results would not match.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// File name of the progress record inside the output directory.
pub const PROGRESS_FILE: &str = "progress.json";

/// Everything that must stay identical for a record to be reused.
///
/// Deliberately excludes cosmetic settings (prompt wording, minimum cue
/// duration): they change how results are assembled, not which frames were
/// sent to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunIdentity {
    /// Input path as given by the user.
    pub input: String,
    pub width: u32,
    pub fps: f64,
    /// Whether overlays were masked before sending frames.
    pub masked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressRecord {
    pub identity: RunIdentity,
    /// Frames already processed, in sampling order.
    pub done: Vec<usize>,
    /// Total number of frames the run is expected to have.
    pub total: usize,
    /// Observations already produced, positionally aligned with `done`.
    pub observations: Vec<serde_json::Value>,
}

impl ProgressRecord {
    pub fn new(identity: RunIdentity, total: usize) -> Self {
        Self {
            identity,
            done: Vec::new(),
            total,
            observations: Vec::new(),
        }
    }

    pub fn path(output_dir: &Path) -> PathBuf {
        output_dir.join(PROGRESS_FILE)
    }

    pub fn save(&self, output_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(output_dir)?;
        let path = Self::path(output_dir);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self)?)
            .with_context(|| format!("writing progress to {}", tmp.display()))?;
        std::fs::rename(&tmp, &path)
            .with_context(|| format!("moving progress into {}", path.display()))?;
        Ok(())
    }

    /// Load a record, returning `None` when there is nothing usable.
    pub fn load(output_dir: &Path) -> Option<Self> {
        let raw = std::fs::read(Self::path(output_dir)).ok()?;
        serde_json::from_slice(&raw).ok()
    }

    /// True when the record describes the same run as `identity`.
    pub fn matches(&self, identity: &RunIdentity) -> bool {
        &self.identity == identity
    }

    /// Frame indices still to be processed.
    pub fn remaining(&self) -> Vec<usize> {
        let mut done = self.done.clone();
        done.sort_unstable();
        done.dedup();
        (0..self.total)
            .filter(|i| done.binary_search(i).is_err())
            .collect()
    }

    pub fn is_complete(&self) -> bool {
        self.done.len() >= self.total
    }
}

/// Decide how a new run should treat an existing output directory.
#[derive(Debug, Clone, PartialEq)]
pub enum ResumeDecision {
    /// Nothing usable found; start from the beginning.
    Fresh,
    /// Continue from the stored record.
    Resume,
    /// The record exists but was made with different settings.
    Stale,
}

pub fn decide(existing: Option<&ProgressRecord>, identity: &RunIdentity) -> ResumeDecision {
    match existing {
        None => ResumeDecision::Fresh,
        Some(record) if record.matches(identity) => ResumeDecision::Resume,
        Some(_) => ResumeDecision::Stale,
    }
}
