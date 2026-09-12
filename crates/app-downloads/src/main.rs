//! Read-only retention planner and fixed-path trusted release importers.
use std::io::{self, Read};

use jarvis_app_downloads::{plan, Entry};

const MAX_INPUT_BYTES: u64 = 2 * 1024 * 1024;

#[tokio::main]
async fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let result = if args == ["sync-release"] {
        sync_release().await
    } else if args == ["sync-ios"] {
        sync_ios().await
    } else {
        run()
    };
    if let Err(error) = result {
        eprintln!("jarvis-app-downloads: {error}");
        std::process::exit(1);
    }
}

async fn sync_release() -> Result<(), &'static str> {
    use jarvis_app_downloads::mirror::{
        self,
        release::{self, ReleaseConfig},
        release_store::ReleaseStore,
        Registry, Store,
    };
    use std::{io::Write, path::Path};
    use zeroize::Zeroizing;
    if unsafe { libc::geteuid() } != 0 {
        return Err("sync-release requires the trusted root administration path");
    }
    for parent in [
        "/etc",
        "/etc/jarvis",
        "/etc/jarvis/app-downloads",
        "/var",
        "/var/lib",
    ] {
        mirror::validate_directory(Path::new(parent), 0)?;
    }
    let mut config: ReleaseConfig = serde_json::from_slice(&mirror::read_protected(
        Path::new("/etc/jarvis/app-downloads/release.json"),
        0,
        true,
        65536,
    )?)
    .map_err(|_| "invalid signed release configuration")?;
    config.validate()?;
    let public = Path::new("/var/lib/jarvis-public-downloads");
    let public_store = Store::open(public, 0)?;
    let store = ReleaseStore::open(Path::new("/var/lib/jarvis-app-updates"), 0)?;
    let secret = Zeroizing::new(mirror::read_protected(
        Path::new("/etc/jarvis/app-downloads/ghcr.token"),
        0,
        true,
        1024,
    )?);
    if secret.is_empty() || !secret.iter().all(u8::is_ascii_graphic) {
        return Err("invalid GHCR credential format");
    }
    let registry = Registry::authenticate(
        &config.identity.github_username,
        std::str::from_utf8(&secret).map_err(|_| "invalid credential format")?,
    )
    .await?;
    drop(secret);
    let oci = if config.track_stable {
        let oci = registry.stable_manifest().await?;
        config.discover(&oci)?;
        oci
    } else {
        registry.manifest(&config.identity.manifest_digest).await?
    };
    let layers = release::parse_oci(&oci, &config.identity)?;
    let manifest_layer = layers
        .get("latest.json")
        .ok_or("missing release metadata")?;
    let sig_layer = layers
        .get("latest.json.sig")
        .ok_or("missing release signature")?;
    let manifest = registry.descriptor(manifest_layer).await?;
    let signature = registry.descriptor(sig_layer).await?;
    for (bytes, layer) in [(&manifest, manifest_layer), (&signature, sig_layer)] {
        if bytes.len() as u64 != layer.size {
            return Err("release metadata size mismatch");
        }
        mirror::verify_bytes(bytes, &layer.digest)?;
    }
    release::validate_document(
        &manifest,
        std::str::from_utf8(&signature).map_err(|_| "invalid release signature")?,
        &config,
        &layers,
    )?;
    let stage = store.stage()?;
    for (name, layer) in &layers {
        let mut file = store.stage_file(&stage, name)?;
        match name.as_str() {
            "latest.json" => file
                .write_all(&manifest)
                .map_err(|_| "cannot stage release metadata")?,
            "latest.json.sig" => file
                .write_all(&signature)
                .map_err(|_| "cannot stage release signature")?,
            _ => registry.download(layer, &mut file).await?,
        }
        file.sync_all().map_err(|_| "cannot sync staged release")?;
    }
    let destination = store.prepare(stage, &config, &oci, public).await?;
    public_store.render_index()?;
    store.activate(&destination)?;
    println!("Verified desktop and Android updates activated; iOS IPA published for local owner signing only.");
    Ok(())
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
            "usage: jarvis-app-downloads plan < verified-public-inventory.json (read-only), or sync-ios / sync-release through trusted root administration",
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
