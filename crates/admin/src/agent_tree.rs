//! Safe, read-only projection of the active private agent bundle.
//!
//! This module deliberately reads only the root-owned manifest. Agent JSON
//! definitions and their private prompt bodies never cross this boundary.

use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

const AGENT_RELEASES_ROOT: &str = "/var/lib/jarvis/agents/releases";
const ACTIVE_AGENT_BUNDLE: &str = "/var/lib/jarvis/agents/current";
const MANIFEST_LIMIT: u64 = 1_048_576;
const MAX_AGENTS: usize = 512;

#[derive(Clone, Debug, Serialize)]
pub(super) struct AgentBundle {
    pub(super) id: String,
    pub(super) agent_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AgentTreeSnapshot {
    pub(super) bundle_id: String,
    pub(super) agents: Vec<AgentTreeAgent>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct AgentTreeAgent {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) group: Option<String>,
    pub(super) model_policy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) profile_lines: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) source_updated_at: Option<String>,
}

#[derive(Deserialize)]
struct SafeAgentManifest {
    version: u32,
    bundle_id: String,
    agents: Vec<SafeAgentManifestEntry>,
}

#[derive(Deserialize)]
struct SafeAgentManifestEntry {
    id: String,
    path: String,
    sha256: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    model_policy: Option<String>,
    #[serde(default)]
    profile_lines: Option<u32>,
    #[serde(default)]
    source_updated_at: Option<String>,
}

pub(super) fn active_bundle() -> Result<Option<AgentBundle>> {
    let target = fs::canonicalize(ACTIVE_AGENT_BUNDLE).ok();
    let Some(target) = target else {
        return Ok(None);
    };
    if !target.starts_with(AGENT_RELEASES_ROOT) {
        bail!("active agent bundle is outside the managed release root");
    }
    let data =
        fs::read_to_string(target.join("manifest.json")).context("read active agent manifest")?;
    let count = serde_json::from_str::<serde_json::Value>(&data)?
        .get("agents")
        .and_then(|value| value.as_array())
        .map_or(0, Vec::len);
    Ok(Some(AgentBundle {
        id: target
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or("unknown")
            .to_owned(),
        agent_count: count,
    }))
}

/// Read only the bounded, non-secret manifest projection used by the Agents
/// views. Agent JSON files are deliberately never opened here because they
/// contain private instructions.
pub(super) fn active_agent_tree() -> Result<Option<AgentTreeSnapshot>> {
    let target = fs::canonicalize(ACTIVE_AGENT_BUNDLE).ok();
    let Some(target) = target else {
        return Ok(None);
    };
    let releases = Path::new(AGENT_RELEASES_ROOT);
    if !target.starts_with(releases) {
        bail!("active agent bundle is outside the managed release root");
    }
    let manifest_path = target.join("manifest.json");
    let metadata = fs::symlink_metadata(&manifest_path).context("inspect active agent manifest")?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != 0
        || metadata.permissions().mode() & 0o022 != 0
        || metadata.len() > MANIFEST_LIMIT
    {
        bail!("active agent manifest is unsafe or too large");
    }
    let data = fs::read(&manifest_path).context("read active agent manifest")?;
    let expected_bundle = target
        .file_name()
        .and_then(OsStr::to_str)
        .context("active agent bundle name is invalid")?;
    Ok(Some(parse_safe_agent_manifest(&data, expected_bundle)?))
}

pub(super) fn parse_safe_agent_manifest(
    data: &[u8],
    expected_bundle: &str,
) -> Result<AgentTreeSnapshot> {
    let manifest: SafeAgentManifest =
        serde_json::from_slice(data).context("parse active agent manifest")?;
    if manifest.version != 1
        || !safe_agent_id(&manifest.bundle_id)
        || manifest.bundle_id != expected_bundle
        || manifest.agents.is_empty()
        || manifest.agents.len() > MAX_AGENTS
    {
        bail!("active agent manifest metadata is invalid");
    }

    let mut seen = BTreeSet::new();
    let mut agents = Vec::with_capacity(manifest.agents.len());
    for entry in manifest.agents {
        if !safe_agent_id(&entry.id)
            || !seen.insert(entry.id.clone())
            || entry.path != format!("agents/{}.json", entry.id)
            || entry.sha256.len() != 64
            || !entry.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            || entry
                .name
                .as_deref()
                .is_some_and(|value| !safe_agent_label(value))
            || entry
                .group
                .as_deref()
                .is_some_and(|value| !safe_agent_label(value))
            || entry.model_policy.as_deref().is_some_and(|value| {
                !matches!(
                    value.to_ascii_lowercase().as_str(),
                    "fast"
                        | "utility"
                        | "default"
                        | "standard"
                        | "strong"
                        | "frontier"
                        | "coding"
                        | "trading"
                        | "research"
                )
            })
            || entry
                .profile_lines
                .is_some_and(|value| value == 0 || value > 100_000)
            || entry
                .source_updated_at
                .as_deref()
                .is_some_and(|value| !safe_agent_timestamp(value))
        {
            bail!("active agent manifest contains unsafe presentation metadata");
        }
        agents.push(AgentTreeAgent {
            name: entry.name.unwrap_or_else(|| entry.id.clone()),
            id: entry.id,
            group: entry.group,
            model_policy: entry.model_policy,
            profile_lines: entry.profile_lines,
            source_updated_at: entry.source_updated_at,
        });
    }
    Ok(AgentTreeSnapshot {
        bundle_id: manifest.bundle_id,
        agents,
    })
}

fn safe_agent_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn safe_agent_label(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value.chars().count() <= 80
        && value
            .chars()
            .all(|character| !character.is_control() && character != '\u{1b}')
}

fn safe_agent_timestamp(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|byte| {
            byte.is_ascii_digit() || matches!(byte, b'-' | b':' | b'.' | b'+' | b'T' | b'Z')
        })
}
