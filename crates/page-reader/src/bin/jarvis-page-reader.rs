//! Page reader: fetches one public web page per request for Core, as its
//! own system user, behind a peer-uid checked Unix socket. It holds no
//! credentials and reads no Jarvis state.
//!
//! Environment:
//! - `JARVIS_PAGE_READER_ALLOWED_PEERS`: comma-separated uids or user names.
//! - `JARVIS_PAGE_READER_SOCKET` (optional): socket path.

use std::{path::PathBuf, process::ExitCode, sync::Arc};

use jarvis_page_reader::{
    fetch::{self, Fetcher, SystemResolver},
    server, DEFAULT_SOCKET,
};
use tokio::signal::unix::{signal, SignalKind};

/// Few threads: the unit caps tasks, and DNS lookups use the blocking pool.
const WORKER_THREADS: usize = 2;
const BLOCKING_THREADS: usize = 8;

fn main() -> ExitCode {
    jarvis_observability::init(true);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(WORKER_THREADS)
        .max_blocking_threads(BLOCKING_THREADS)
        .enable_all()
        .build();
    let result = match runtime {
        Ok(runtime) => runtime.block_on(run()),
        Err(error) => Err(format!("cannot start the runtime: {error}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "page reader stopped");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), String> {
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        return Err("jarvis-page-reader must not run as root".into());
    }
    // Without the interface list every address check fails closed: refuse
    // to start instead of refusing every page.
    if !fetch::interfaces_readable() {
        return Err("cannot list the network interfaces; every page would be refused".into());
    }
    let env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    let allowed = server::resolve_peers(
        &env("JARVIS_PAGE_READER_ALLOWED_PEERS")
            .ok_or("JARVIS_PAGE_READER_ALLOWED_PEERS missing")?,
    )?;
    let socket = PathBuf::from(env("JARVIS_PAGE_READER_SOCKET").unwrap_or(DEFAULT_SOCKET.into()));
    if !socket.is_absolute() {
        return Err("the socket must be an absolute path".into());
    }
    let listener = server::bind(&socket).map_err(|e| format!("bind reader socket: {e}"))?;
    let mut term = signal(SignalKind::terminate()).map_err(|e| e.to_string())?;
    let mut int = signal(SignalKind::interrupt()).map_err(|e| e.to_string())?;
    tracing::info!(peers = ?allowed, "page reader ready");
    let shutdown = async {
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
    };
    let fetcher = Arc::new(Fetcher::new(SystemResolver));
    let served = server::serve(listener, fetcher, allowed.into(), shutdown).await;
    let _ = std::fs::remove_file(&socket);
    served.map_err(|e| format!("reader socket failed: {e}"))
}
