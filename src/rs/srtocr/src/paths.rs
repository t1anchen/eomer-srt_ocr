//! Output-location policy.

use std::path::{Path, PathBuf};

/// Decide where a run writes its results.
///
/// * an explicit `--out` is honoured verbatim (`.` means the current
///   directory, which the caller may really want);
/// * otherwise results go below `<base>/out/<input-stem>/`, so a default run
///   never scatters files into the working directory.
pub fn resolve_output_dir(input: &Path, out: Option<&Path>, base: &Path) -> PathBuf {
    if let Some(explicit) = out {
        return explicit.to_path_buf();
    }
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "output".into());
    base.join("out").join(stem)
}
