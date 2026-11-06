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

    /// llama.cpp's own default address, which is where an operator who just
    /// started the server will already have it.
    pub fn default_server() -> String {
        "http://127.0.0.1:8080".to_string()
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
    let listener = bind(&state).await?;
    serve_on(listener, state).await
}

/// Bind the configured address.
///
/// Takes `&AppState` so the caller keeps ownership: the address may be an
/// ephemeral `:0` port, whose real number is only known after binding.
pub async fn bind(state: &AppState) -> anyhow::Result<tokio::net::TcpListener> {
    Ok(tokio::net::TcpListener::bind(state.cfg.addr).await?)
}

/// Serve on an already-bound listener.
///
/// Separate from [`serve`] so tests can hand in `127.0.0.1:0`, learn the real
/// port from the listener and drive the real router over TCP instead of
/// racing on a hard-coded port.
pub async fn serve_on(
    listener: tokio::net::TcpListener,
    state: AppState,
) -> anyhow::Result<()> {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    {
        let store = state.store.clone();
        let gate = state.gate.clone();
        std::thread::Builder::new()
            .name("ocr-worker".to_string())
            .spawn(move || jobs::worker(store, gate, rx))?;
    }

    let app = routes::router(state, tx);
    axum::serve(listener, app).await?;
    Ok(())
}
