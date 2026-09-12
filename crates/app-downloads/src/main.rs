//! Read-only retention planner and fixed-path trusted candidate importer.
use std::io::{self, Read};

use jarvis_app_downloads::{plan, Entry};

const MAX_INPUT_BYTES: u64 = 2 * 1024 * 1024;

#[tokio::main]
async fn main() {
    let result = if std::env::args().skip(1).collect::<Vec<_>>() == ["sync-ios"] {
        sync_ios().await
    } else {
        run()
    };
    if let Err(error) = result {
        eprintln!("jarvis-app-downloads: {error}");
        std::process::exit(1);
    }
}

async fn sync_ios() -> Result<(), &'static str> {
    use jarvis_app_downloads::mirror::{self, Config, Registry, Store};
    use std::path::Path;
    use zeroize::Zeroizing;
    // The service has fixed protected paths; no caller-selected executable,
    // token path, repository, redirect host or public destination is accepted.
    if unsafe { libc::geteuid() } != 0 {
        return Err("sync-ios requires the trusted root administration path");
    }
    for path in [
        "/etc",
        "/etc/jarvis",
        "/etc/jarvis/app-downloads",
        "/var",
        "/var/lib",
    ] {
        mirror::validate_directory(Path::new(path), 0)?;
    }
    let config: Config = serde_json::from_slice(&mirror::read_protected(
        Path::new("/etc/jarvis/app-downloads/config.json"),
        0,
        false,
        65536,
    )?)
    .map_err(|_| "invalid mirror configuration")?;
    config.validate()?;
    let store = Store::open(Path::new("/var/lib/jarvis-public-downloads"), 0)?;
    let bytes = Zeroizing::new(mirror::read_protected(
        Path::new("/etc/jarvis/app-downloads/ghcr.token"),
        0,
        true,
        1024,
    )?);
    if bytes.is_empty() || !bytes.iter().all(|b| b.is_ascii_graphic()) {
        return Err("invalid GHCR credential format");
    }
    let secret = Zeroizing::new(
        String::from_utf8(bytes.to_vec()).map_err(|_| "invalid GHCR credential format")?,
    );
    let registry = Registry::authenticate(&config.github_username, &secret).await?;
    drop(secret);
    drop(bytes);
    let manifest = registry.manifest(&config.manifest_digest).await?;
    let (ipa, descriptor_layer) = mirror::parse_manifest(&manifest, &config)?;
    let descriptor = registry.descriptor(&descriptor_layer).await?;
    mirror::validate_candidate(&descriptor, &descriptor_layer, &ipa, &config)?;
    let mut stage = store.stage(&config)?;
    registry.download(&ipa, &mut stage.file).await?;
    store.publish(stage, &config, &ipa, &manifest, &descriptor)?;
    println!("Verified iOS candidate published locally; owner signing required.");
    Ok(())
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
