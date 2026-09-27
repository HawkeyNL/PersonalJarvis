//! Read-only owner preflight. It never enables units or launches a workload.
//! Physical Kata/egress acceptance remains separate and must not be inferred
//! from merely seeing a running Docker/OpenSandbox service.

use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use serde_json::json;
use sha2::{Digest, Sha256};

use super::{CURRENT_RELEASE, RELEASES_ROOT};

const BROKER_ENV: &str = "/etc/jarvis/codex-broker.env";
const REGISTRY: &str = "/etc/jarvis/codex-repositories.json";
const MIRRORS: &str = "/var/lib/jarvis-codex-repositories";

#[derive(Debug, Subcommand)]
pub(super) enum SandboxCommand {
    /// Inspect non-secret local gates. Does not replace owner-run Kata tests.
    Verify,
}

pub(super) fn run(command: SandboxCommand, json_output: bool) -> Result<()> {
    match command {
        SandboxCommand::Verify => verify(json_output),
    }
}

fn verify(json_output: bool) -> Result<()> {
    let env_file = Path::new(BROKER_ENV);
    if !env_file.exists() {
        print(
            json_output,
            "disabled",
            &["broker configuration absent"],
            None,
        );
        return Ok(());
    }
    trusted_root_file(env_file)?;
    let raw = fs::read_to_string(env_file).context("read protected broker configuration")?;
    if raw.len() > 64 * 1024 {
        bail!("Codex broker configuration exceeds bound");
    }
    let configured = config_value(&raw, "JARVIS_CODEX_EXECUTION_ENABLED") == Some("1");
    if !configured {
        print(
            json_output,
            "disabled",
            &["execution not owner-enabled"],
            None,
        );
        return Ok(());
    }
    let mut issues = Vec::new();
    let endpoint = config_value(&raw, "JARVIS_CODEX_OPENSANDBOX_ENDPOINT");
    if !endpoint.is_some_and(loopback_endpoint) {
        issues.push("manager endpoint is not explicitly loopback");
    }
    if config_value(&raw, "JARVIS_CODEX_OPENSANDBOX_API_KEY").is_none_or(str::is_empty) {
        issues.push("manager authentication is not configured");
    }
    let image = config_value(&raw, "JARVIS_CODEX_WORKLOAD_IMAGE");
    if !image.is_some_and(digest_pinned_image) {
        issues.push("workload image is not registry-digest pinned");
    }
    if trusted_root_file(Path::new(REGISTRY)).is_err() {
        issues.push("trusted repository registry unavailable");
    }
    if trusted_root_directory(Path::new(MIRRORS)).is_err() {
        issues.push("trusted repository mirror root unavailable");
    }
    for (unit, message) in [
        ("jarvis-opensandbox.service", "OpenSandbox manager inactive"),
        ("jarvis-codex-broker.service", "Codex broker inactive"),
    ] {
        let active = Command::new("/usr/bin/systemctl")
            .args(["is-active", "--quiet", unit])
            .status()
            .is_ok_and(|status| status.success());
        if !active {
            issues.push(message);
        }
    }
    let safe_image = image.filter(|value| digest_pinned_image(value));
    if let Some(image) = safe_image {
        let present = Command::new("/usr/bin/docker")
            .args(["image", "inspect", "--format", "{{.Id}}", image])
            .output()
            .is_ok_and(|output| output.status.success() && output.stdout.starts_with(b"sha256:"));
        if !present {
            issues.push("reviewed workload digest is not present locally");
        } else if !workload_matches_release(image) {
            issues.push("workload runtime hash/user does not match active release");
        }
    }
    // Read-only checks cannot prove Kata selection, runtime resource quotas,
    // DNS rebinding defence or actual egress isolation. Never claim ready.
    print(json_output, "acceptance_required", &issues, safe_image);
    if issues.is_empty() {
        bail!("physical Kata/egress and subscription acceptance still required");
    }
    bail!("Codex/OpenSandbox preflight has unmet gates")
}

fn workload_matches_release(image: &str) -> bool {
    let Ok(release) = fs::canonicalize(CURRENT_RELEASE) else {
        return false;
    };
    if !release.starts_with(RELEASES_ROOT) {
        return false;
    }
    let runtime = release.join("jarvis-codex-runtime");
    if trusted_root_file(&runtime).is_err() {
        return false;
    }
    if !fs::metadata(&runtime)
        .is_ok_and(|metadata| metadata.len() > 0 && metadata.len() <= 32 * 1024 * 1024)
    {
        return false;
    }
    let Ok(bytes) = fs::read(&runtime) else {
        return false;
    };
    if bytes.is_empty() || bytes.len() > 32 * 1024 * 1024 {
        return false;
    }
    let expected = hex::encode(Sha256::digest(bytes));
    let Ok(output) = Command::new("/usr/bin/docker")
        .args([
            "image",
            "inspect",
            "--format",
            "{{index .Config.Labels \"nl.jarvis.codex-runtime.sha256\"}} {{.Config.User}}",
            image,
        ])
        .output()
    else {
        return false;
    };
    output.status.success() && output.stdout == format!("{expected} 65532:65532\n").as_bytes()
}

fn print(json_output: bool, state: &str, issues: &[&str], image: Option<&str>) {
    if json_output {
        println!(
            "{}",
            json!({
                "state":state,
                "issues":issues,
                "workload_image":image,
                "physical_acceptance_required":state != "disabled",
                "mutated":false,
            })
        );
    } else {
        println!("Codex sandbox: {state}");
        for issue in issues {
            println!("- {issue}");
        }
        if state != "disabled" {
            println!("Physical Kata, egress, quota and subscription checks remain owner-run.");
        }
    }
}

fn config_value<'a>(raw: &'a str, name: &str) -> Option<&'a str> {
    let mut found = raw
        .lines()
        .filter_map(|line| line.split_once('='))
        .filter_map(|(key, value)| {
            (key == name && !value.contains(['\r', '\n']) && !value.starts_with(' '))
                .then_some(value)
        });
    let value = found.next()?;
    found.next().is_none().then_some(value)
}

fn loopback_endpoint(value: &str) -> bool {
    let port = value
        .strip_prefix("http://127.0.0.1:")
        .or_else(|| value.strip_prefix("http://[::1]:"));
    let Some(port) = port.and_then(|value| value.strip_suffix('/')) else {
        return false;
    };
    !port.is_empty()
        && port.len() <= 5
        && port.bytes().all(|byte| byte.is_ascii_digit())
        && port.parse::<u16>().is_ok_and(|port| port != 0)
}

fn digest_pinned_image(value: &str) -> bool {
    let Some((registry, digest)) = value.split_once("@sha256:") else {
        return false;
    };
    !registry.is_empty()
        && registry.len() <= 160
        && registry
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'/' | b'-' | b'_'))
        && digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn trusted_root_file(path: &Path) -> Result<()> {
    trusted_ancestors(path)?;
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_file() || meta.uid() != 0 || meta.mode() & 0o022 != 0 {
        bail!("unsafe protected file");
    }
    Ok(())
}

fn trusted_root_directory(path: &Path) -> Result<()> {
    trusted_ancestors(path)?;
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_dir() || meta.uid() != 0 || meta.mode() & 0o022 != 0 {
        bail!("unsafe protected directory");
    }
    Ok(())
}

fn trusted_ancestors(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        bail!("protected path is not absolute");
    }
    let mut current = PathBuf::from("/");
    for part in path
        .components()
        .skip(1)
        .take(path.components().count().saturating_sub(2))
    {
        current.push(part);
        let meta = fs::symlink_metadata(&current)?;
        if !meta.file_type().is_dir() || meta.uid() != 0 || meta.mode() & 0o022 != 0 {
            bail!("unsafe protected directory ancestor");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preflight_refuses_mutable_images_and_broad_endpoints() {
        assert!(!digest_pinned_image("codex:latest"));
        assert!(!digest_pinned_image(&format!(
            "image@sha256:{}",
            "A".repeat(64)
        )));
        assert!(digest_pinned_image(&format!(
            "registry.example/codex@sha256:{}",
            "a".repeat(64)
        )));
        assert_eq!(
            config_value(
                "JARVIS_CODEX_EXECUTION_ENABLED=0\n",
                "JARVIS_CODEX_EXECUTION_ENABLED"
            ),
            Some("0")
        );
        assert_eq!(config_value("X=1\nX=0\n", "X"), None);
        assert!(loopback_endpoint("http://127.0.0.1:8090/"));
        assert!(!loopback_endpoint("http://127.0.0.1:8090@evil/"));
    }
}
