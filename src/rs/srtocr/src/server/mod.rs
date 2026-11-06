//! Local HTTP service: upload, queue, progress, result download.

pub mod frames;
pub mod gate;
pub mod jobs;
pub mod routes;

use std::net::SocketAddr;
use std::path::PathBuf;

pub use frames::{StreamStore, StreamView};
pub use gate::InferenceGate;
pub use jobs::{JobId, JobKind, JobState, JobView, Store};

/// Everything the handlers need. Built once at startup and shared via `AppState`.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Loopback address to bind.
    pub addr: SocketAddr,
    /// llama-server base URL.
    pub server: String,
    /// Working width frames/images are scaled to.
    pub width: u32,
    /// Root for per-job working directories.
    pub work_dir: PathBuf,
    /// Skip the reachability check at startup (useful for UI-only work).
    pub skip_backend_check: bool,
}

impl ServerConfig {
    pub fn default_addr() -> SocketAddr {
        // Loopback on purpose: the service has no authentication and runs
        // arbitrary OCR jobs on uploaded files.
        SocketAddr::from(([127, 0, 0, 1], 8787))
    }
}

#[derive(Debug, Clone)]
pub struct AppState {
    pub cfg: ServerConfig,
    pub store: Store,
    /// Frame-streaming jobs, keyed by stream id.
    pub streams: StreamStore,
    /// Serialises every call to the inference backend across paths.
    pub gate: InferenceGate,
}

/// Bind the listener and serve until the process is stopped.
pub async fn serve(state: AppState) -> anyhow::Result<()> {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    {
        let store = state.store.clone();
        let gate = state.gate.clone();
        std::thread::Builder::new()
            .name("ocr-worker".to_string())
            .spawn(move || jobs::worker(store, gate, rx))?;
    }

    let app = routes::router(state.clone(), tx);
    let listener = tokio::net::TcpListener::bind(state.cfg.addr).await?;
    println!("listening on http://{}", state.cfg.addr);
    axum::serve(listener, app).await?;
    Ok(())
}
