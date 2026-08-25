//! Structured logging/tracing initialisation shared by all Jarvis services.

use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Initialise the global tracing subscriber.
///
/// Honours the `RUST_LOG` env var (falls back to `info`). When `json` is
/// `true`, logs are emitted as one JSON object per line (production); otherwise
/// a compact human-readable format is used (development).
///
/// Call exactly once, early in `main`.
pub fn init(json: bool) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let registry = tracing_subscriber::registry().with(filter);

    if json {
        registry
            .with(fmt::layer().json().flatten_event(true))
            .init();
    } else {
        registry.with(fmt::layer().compact()).init();
    }
}
