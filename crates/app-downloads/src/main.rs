//! Rootless, read-only retention planner. It deliberately cannot delete files.
use std::io::{self, Read};

use jarvis_app_downloads::{plan, Entry};

const MAX_INPUT_BYTES: u64 = 2 * 1024 * 1024;

fn main() {
    if let Err(error) = run() {
        eprintln!("jarvis-app-downloads: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), &'static str> {
    if std::env::args().skip(1).collect::<Vec<_>>() != ["plan"] {
        return Err(
            "usage: jarvis-app-downloads plan < verified-public-inventory.json (read-only)",
        );
    }
    let mut bytes = Vec::new();
    io::stdin()
        .lock()
        .take(MAX_INPUT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "could not read inventory")?;
    if bytes.len() as u64 > MAX_INPUT_BYTES {
        return Err("inventory exceeds 2 MiB limit");
    }
    let entries: Vec<Entry> = serde_json::from_slice(&bytes).map_err(|_| "invalid inventory")?;
    let result =
        plan(entries).map_err(|_| "invalid, duplicate or oversized inventory; no plan produced")?;
    serde_json::to_writer_pretty(io::stdout().lock(), &result).map_err(|_| "could not write plan")
}
