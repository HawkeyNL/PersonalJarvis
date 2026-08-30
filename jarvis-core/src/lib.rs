//! Jarvis Core — application identity and future orchestration boundary.
//!
//! The Core deliberately owns no HTTP transport, policy rules, approval state,
//! sandbox, or executor. Those remain in their dedicated crates. Today it owns
//! the canonical Jarvis persona loaded by the native Home-Node process; future
//! orchestration may move here only when it has a real ownership boundary.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::Duration,
};

use jarvis_policy::Capability;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Fallback persona used when the protected on-disk persona cannot be read.
/// This keeps development and CI functional while production logs the failed
/// load at startup.
pub const JARVIS_SYSTEM_FALLBACK: &str = "Je bent Jarvis, de persoonlijke AI-assistent op het HUD-dashboard van de gebruiker. \
Antwoord in het Nederlands, kort en duidelijk, in een rustige en behulpzame toon. \
Je helpt met het systeem, de portfolio en trading-inzichten. \
Zeg het eerlijk wanneer je iets niet zeker weet in plaats van te gokken. \
Voer nooit trades of onomkeerbare acties uit — die vereisen altijd een expliciete bevestiging van de gebruiker.";

/// Load Jarvis' canonical persona from `path`.
///
/// A missing, unreadable, or empty file fails safely to the built-in persona.
/// The caller is responsible for recording that degraded startup state without
/// exposing the file contents or filesystem details to API clients.
pub fn load_persona(path: &str) -> (Arc<str>, bool) {
    match std::fs::read_to_string(path) {
        Ok(text) if !text.trim().is_empty() => (Arc::from(text.trim()), true),
        _ => (Arc::from(JARVIS_SYSTEM_FALLBACK), false),
    }
}

/// A validated private agent definition. The public runtime deliberately knows
/// only the schema; real instructions live in the separately provisioned,
/// root-owned bundle on the Home Node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDefinition {
    pub id: String,
    pub name: String,
    /// Optional non-secret presentation category copied into the public
    /// bundle manifest. It must never be derived from agent instructions.
    #[serde(default)]
    pub group: Option<String>,
    pub description: String,
    pub model_policy: String,
    pub instructions: String,
    #[serde(default)]
    pub requested_capabilities: Vec<Capability>,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub denied_actions: Vec<String>,
    pub limits: AgentLimits,
}

/// A deliberately small, provider-neutral request made by a private agent
/// profile. It selects only a quality/mode floor; the LLM router still applies
/// the root-owned exact model allowlist, health, budget and task policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentModelPolicy {
    Fast,
    Standard,
    Strong,
    Research,
}

impl AgentModelPolicy {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "fast" | "utility" => Some(Self::Fast),
            "default" | "standard" => Some(Self::Standard),
            "strong" | "frontier" | "coding" | "trading" => Some(Self::Strong),
            "research" => Some(Self::Research),
            _ => None,
        }
    }

    /// Names match `jarvis_llm::RoutingMode` without making Core own provider
    /// routing or credentials. The executor passes this only as a minimum
    /// quality request to the router.
    pub const fn routing_mode(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Standard => "auto",
            Self::Strong => "deep",
            Self::Research => "research",
        }
    }
}

/// Strict versioned frontmatter used only at the trusted private-bundle
/// deployment boundary. Runtime loads the resulting hash-checked JSON bundle.
/// Hard upper bounds declared by an agent profile. Core still applies its own
/// policy and deployment limits; a profile can only request less capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentLimits {
    pub max_runtime_seconds: u64,
    pub max_context_chars: usize,
    pub max_output_chars: usize,
    pub max_parallel_runs: u16,
}

/// Deterministic, non-secret description of an installed bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentBundleManifest {
    pub version: u32,
    pub bundle_id: String,
    pub agents: Vec<AgentBundleEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentBundleEntry {
    pub id: String,
    pub path: String,
    pub sha256: String,
    /// Safe presentation metadata. These fields are optional so existing
    /// immutable version-1 manifests remain loadable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_policy: Option<String>,
}

/// Registry loaded from a versioned, immutable private agent bundle.
#[derive(Debug, Clone)]
pub struct AgentRegistry {
    bundle_id: String,
    agents: Vec<AgentDefinition>,
}

/// Immutable registry generation. A reload constructs a complete new snapshot;
/// callers holding an older snapshot (including active runs) remain valid.
#[derive(Debug, Clone)]
pub struct AgentRegistrySnapshot {
    pub generation: u64,
    pub registry: Arc<AgentRegistry>,
}

/// Core-owned loader for the staged bundle only. It neither knows nor accepts
/// a Git checkout, credentials, or arbitrary private source path.
#[derive(Debug)]
pub struct AgentLoader {
    bundle_path: PathBuf,
    current: RwLock<Arc<AgentRegistrySnapshot>>,
}

/// A model-generated change can be shown to an owner, but represents no write
/// capability. Deployment remains a separately authenticated root operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentChangeProposal {
    pub id: Uuid,
    pub agent_id: String,
    pub base_bundle_id: String,
    pub summary: String,
    pub unified_diff: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AgentRegistryError {
    #[error("agent bundle is unavailable")]
    MissingBundle,
    #[error("agent bundle is malformed")]
    MalformedBundle,
    #[error("agent bundle contains an unsafe path")]
    UnsafePath,
    #[error("agent definition is invalid: {0}")]
    InvalidDefinition(&'static str),
    #[error("agent bundle hash mismatch")]
    HashMismatch,
    #[error("agent run limit exceeded")]
    ConcurrencyLimit,
}

impl AgentRegistry {
    /// Loads only files explicitly named by `manifest.json`; symlinks and path
    /// traversal fail closed. The installer is responsible for making the
    /// release tree root-owned and immutable to the `jarvis` service account.
    pub fn load(bundle_dir: impl AsRef<Path>) -> Result<Self, AgentRegistryError> {
        // `current` is intentionally an owner-controlled symlink to a
        // versioned release. Resolve it once, then reject symlinks inside the
        // bundle itself so an agent file cannot escape the release tree.
        let bundle_dir =
            fs::canonicalize(bundle_dir).map_err(|_| AgentRegistryError::MissingBundle)?;
        let metadata =
            fs::symlink_metadata(&bundle_dir).map_err(|_| AgentRegistryError::MissingBundle)?;
        if !metadata.is_dir() {
            return Err(AgentRegistryError::UnsafePath);
        }
        let manifest_path = bundle_dir.join("manifest.json");
        let manifest_metadata =
            fs::symlink_metadata(&manifest_path).map_err(|_| AgentRegistryError::MissingBundle)?;
        if manifest_metadata.file_type().is_symlink() || !manifest_metadata.is_file() {
            return Err(AgentRegistryError::UnsafePath);
        }
        let manifest: AgentBundleManifest = serde_json::from_slice(
            &fs::read(&manifest_path).map_err(|_| AgentRegistryError::MissingBundle)?,
        )
        .map_err(|_| AgentRegistryError::MalformedBundle)?;
        if manifest.version != 1 || !is_safe_id(&manifest.bundle_id) || manifest.agents.is_empty() {
            return Err(AgentRegistryError::MalformedBundle);
        }

        let mut seen = BTreeSet::new();
        let mut agents = Vec::with_capacity(manifest.agents.len());
        for entry in &manifest.agents {
            if !is_safe_id(&entry.id)
                || !seen.insert(entry.id.clone())
                || !is_safe_bundle_path(&entry.path)
            {
                return Err(AgentRegistryError::UnsafePath);
            }
            if entry.sha256.len() != 64
                || !entry.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
                || entry
                    .name
                    .as_deref()
                    .is_some_and(|value| !is_safe_label(value))
                || entry
                    .group
                    .as_deref()
                    .is_some_and(|value| !is_safe_label(value))
                || entry
                    .model_policy
                    .as_deref()
                    .is_some_and(|value| AgentModelPolicy::parse(value).is_none())
            {
                return Err(AgentRegistryError::MalformedBundle);
            }
            let file = bundle_dir.join(&entry.path);
            let file_metadata =
                fs::symlink_metadata(&file).map_err(|_| AgentRegistryError::MissingBundle)?;
            if file_metadata.file_type().is_symlink() || !file_metadata.is_file() {
                return Err(AgentRegistryError::UnsafePath);
            }
            let bytes = fs::read(file).map_err(|_| AgentRegistryError::MissingBundle)?;
            if sha256_hex(&bytes) != entry.sha256 {
                return Err(AgentRegistryError::HashMismatch);
            }
            let agent: AgentDefinition =
                serde_json::from_slice(&bytes).map_err(|_| AgentRegistryError::MalformedBundle)?;
            validate_definition(&agent)?;
            if agent.id != entry.id
                || entry.name.as_ref().is_some_and(|name| *name != agent.name)
                || entry
                    .group
                    .as_ref()
                    .is_some_and(|group| agent.group.as_ref() != Some(group))
                || entry
                    .model_policy
                    .as_ref()
                    .is_some_and(|policy| *policy != agent.model_policy)
            {
                return Err(AgentRegistryError::MalformedBundle);
            }
            agents.push(agent);
        }
        Ok(Self {
            bundle_id: manifest.bundle_id,
            agents,
        })
    }

    pub fn bundle_id(&self) -> &str {
        &self.bundle_id
    }
    pub fn agents(&self) -> &[AgentDefinition] {
        &self.agents
    }
    pub fn get(&self, id: &str) -> Option<&AgentDefinition> {
        self.agents.iter().find(|agent| agent.id == id)
    }

    /// Computes effective rights by intersection only. The profile never grants
    /// a capability absent from the trusted Core/user/task policy context.
    pub fn effective_capabilities(
        &self,
        id: &str,
        core_allowed: &[Capability],
        device_allowed: &[Capability],
        task_allowed: &[Capability],
    ) -> Option<Vec<Capability>> {
        let agent = self.get(id)?;
        Some(
            agent
                .requested_capabilities
                .iter()
                .copied()
                .filter(|capability| {
                    core_allowed.contains(capability)
                        && device_allowed.contains(capability)
                        && task_allowed.contains(capability)
                })
                .collect(),
        )
    }
}

impl AgentLoader {
    /// Parses a private Markdown profile without executing any instructions.
    /// Only an exact `---` YAML frontmatter block followed by non-empty
    /// Markdown is accepted; unsupported fields and capabilities fail closed.
    pub fn parse_markdown(markdown: &str) -> Result<AgentDefinition, AgentRegistryError> {
        let remainder = markdown
            .strip_prefix("---\n")
            .ok_or(AgentRegistryError::MalformedBundle)?;
        let (frontmatter, instructions) = remainder
            .split_once("\n---\n")
            .ok_or(AgentRegistryError::MalformedBundle)?;
        let frontmatter = parse_frontmatter(frontmatter)?;
        let agent = AgentDefinition {
            id: required_frontmatter(&frontmatter, "id")?.to_string(),
            name: required_frontmatter(&frontmatter, "name")?.to_string(),
            group: frontmatter.get("group").cloned(),
            description: required_frontmatter(&frontmatter, "description")?.to_string(),
            model_policy: required_frontmatter(&frontmatter, "model_policy")?.to_string(),
            instructions: instructions.trim().to_string(),
            requested_capabilities: parse_capabilities(&frontmatter)?,
            allowed_tools: Vec::new(),
            denied_actions: Vec::new(),
            limits: AgentLimits {
                max_runtime_seconds: required_frontmatter(&frontmatter, "max_runtime_seconds")
                    .and_then(parse_number)?,
                max_context_chars: required_frontmatter(&frontmatter, "max_context_chars")
                    .and_then(parse_number)?,
                max_output_chars: required_frontmatter(&frontmatter, "max_output_chars")
                    .and_then(parse_number)?,
                max_parallel_runs: required_frontmatter(&frontmatter, "max_parallel_runs")
                    .and_then(parse_number)?,
            },
        };
        validate_definition(&agent)?;
        Ok(agent)
    }

    pub fn load(bundle_path: impl Into<PathBuf>) -> Result<Self, AgentRegistryError> {
        let bundle_path = bundle_path.into();
        let registry = Arc::new(AgentRegistry::load(&bundle_path)?);
        Ok(Self {
            bundle_path,
            current: RwLock::new(Arc::new(AgentRegistrySnapshot {
                generation: 1,
                registry,
            })),
        })
    }

    pub fn snapshot(&self) -> Arc<AgentRegistrySnapshot> {
        self.current
            .read()
            .expect("agent registry lock poisoned")
            .clone()
    }

    /// Loads and validates a whole replacement first. If it fails, the active
    /// snapshot is unchanged; no partial registry can become visible.
    pub fn reload(&self) -> Result<Arc<AgentRegistrySnapshot>, AgentRegistryError> {
        let registry = Arc::new(AgentRegistry::load(&self.bundle_path)?);
        let mut current = self.current.write().expect("agent registry lock poisoned");
        let next = Arc::new(AgentRegistrySnapshot {
            generation: current.generation.saturating_add(1),
            registry,
        });
        *current = next.clone();
        Ok(next)
    }
}

fn parse_frontmatter(
    input: &str,
) -> Result<std::collections::BTreeMap<String, String>, AgentRegistryError> {
    const ALLOWED: &[&str] = &[
        "schema_version",
        "id",
        "name",
        "group",
        "description",
        "model_policy",
        "requested_capabilities",
        "max_runtime_seconds",
        "max_context_chars",
        "max_output_chars",
        "max_parallel_runs",
    ];
    let mut values = std::collections::BTreeMap::new();
    let mut active_list = false;
    for line in input.lines() {
        if let Some(item) = line.strip_prefix("  - ") {
            if !active_list || item.trim().is_empty() {
                return Err(AgentRegistryError::MalformedBundle);
            }
            values
                .entry("requested_capabilities".into())
                .and_modify(|value: &mut String| {
                    value.push(',');
                    value.push_str(item.trim());
                });
            continue;
        }
        active_list = false;
        let (key, value) = line
            .split_once(':')
            .ok_or(AgentRegistryError::MalformedBundle)?;
        let key = key.trim();
        if !ALLOWED.contains(&key) || values.contains_key(key) {
            return Err(AgentRegistryError::MalformedBundle);
        }
        let value = value.trim();
        if key == "requested_capabilities" {
            if !value.is_empty() {
                return Err(AgentRegistryError::MalformedBundle);
            }
            active_list = true;
            values.insert(key.to_string(), String::new());
        } else if value.is_empty() {
            return Err(AgentRegistryError::MalformedBundle);
        } else {
            values.insert(key.to_string(), value.to_string());
        }
    }
    if values.get("schema_version").map(String::as_str) != Some("1") {
        return Err(AgentRegistryError::MalformedBundle);
    }
    Ok(values)
}

fn required_frontmatter<'a>(
    values: &'a std::collections::BTreeMap<String, String>,
    key: &'static str,
) -> Result<&'a str, AgentRegistryError> {
    values
        .get(key)
        .map(String::as_str)
        .ok_or(AgentRegistryError::MalformedBundle)
}

fn parse_number<T: std::str::FromStr>(value: &str) -> Result<T, AgentRegistryError> {
    value
        .parse()
        .map_err(|_| AgentRegistryError::MalformedBundle)
}

fn parse_capabilities(
    values: &std::collections::BTreeMap<String, String>,
) -> Result<Vec<Capability>, AgentRegistryError> {
    values
        .get("requested_capabilities")
        .map_or(Ok(Vec::new()), |value| {
            value
                .split(',')
                .filter(|part| !part.is_empty())
                .map(|capability| {
                    serde_json::from_value(serde_json::Value::String(capability.to_string()))
                        .map_err(|_| AgentRegistryError::MalformedBundle)
                })
                .collect()
        })
}

/// A temporary Core-owned agent invocation. It is not a Linux service and
/// cannot recursively create other runs through this primitive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRun {
    pub id: Uuid,
    pub agent_id: String,
    pub bundle_id: String,
    pub registry_generation: u64,
    pub effective_capabilities: Vec<Capability>,
    /// Profile-requested quality floor, not a provider/model grant.
    pub model_policy: AgentModelPolicy,
    pub timeout: Duration,
    pub max_context_chars: usize,
    pub max_output_chars: usize,
}

impl AgentRun {
    pub fn new(
        registry: &AgentRegistry,
        agent_id: &str,
        core_allowed: &[Capability],
        device_allowed: &[Capability],
        task_allowed: &[Capability],
        active_runs_for_agent: u16,
    ) -> Result<Self, AgentRegistryError> {
        let agent = registry
            .get(agent_id)
            .ok_or(AgentRegistryError::MissingBundle)?;
        if active_runs_for_agent >= agent.limits.max_parallel_runs {
            return Err(AgentRegistryError::ConcurrencyLimit);
        }
        let effective_capabilities = registry
            .effective_capabilities(agent_id, core_allowed, device_allowed, task_allowed)
            .ok_or(AgentRegistryError::MissingBundle)?;
        let model_policy = AgentModelPolicy::parse(&agent.model_policy).ok_or(
            AgentRegistryError::InvalidDefinition("unknown model policy"),
        )?;
        Ok(Self {
            id: Uuid::now_v7(),
            agent_id: agent_id.to_string(),
            bundle_id: registry.bundle_id.clone(),
            registry_generation: 0,
            effective_capabilities,
            model_policy,
            timeout: Duration::from_secs(agent.limits.max_runtime_seconds),
            max_context_chars: agent.limits.max_context_chars,
            max_output_chars: agent.limits.max_output_chars,
        })
    }

    pub fn from_snapshot(
        snapshot: &AgentRegistrySnapshot,
        agent_id: &str,
        core_allowed: &[Capability],
        device_allowed: &[Capability],
        task_allowed: &[Capability],
        active_runs_for_agent: u16,
    ) -> Result<Self, AgentRegistryError> {
        let mut run = Self::new(
            &snapshot.registry,
            agent_id,
            core_allowed,
            device_allowed,
            task_allowed,
            active_runs_for_agent,
        )?;
        run.registry_generation = snapshot.generation;
        Ok(run)
    }
}

fn is_safe_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn is_safe_label(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value.chars().count() <= 80
        && value
            .chars()
            .all(|character| !character.is_control() && character != '\u{1b}')
}

fn is_safe_bundle_path(value: &str) -> bool {
    let path = PathBuf::from(value);
    path.components().count() == 2
        && path.starts_with("agents")
        && path
            .extension()
            .is_some_and(|extension| extension == "json")
        && !path.is_absolute()
}

fn validate_definition(agent: &AgentDefinition) -> Result<(), AgentRegistryError> {
    if !is_safe_id(&agent.id)
        || !is_safe_label(&agent.name)
        || agent
            .group
            .as_deref()
            .is_some_and(|group| !is_safe_label(group))
        || agent.description.trim().is_empty()
        || agent.model_policy.trim().is_empty()
        || agent.instructions.trim().is_empty()
        || agent.limits.max_runtime_seconds == 0
        || agent.limits.max_runtime_seconds > 3600
        || agent.limits.max_context_chars == 0
        || agent.limits.max_context_chars > 200_000
        || agent.limits.max_output_chars == 0
        || agent.limits.max_output_chars > 100_000
        || agent.limits.max_parallel_runs == 0
        || agent.limits.max_parallel_runs > 16
    {
        return Err(AgentRegistryError::InvalidDefinition(
            "required fields or limits",
        ));
    }
    if AgentModelPolicy::parse(&agent.model_policy).is_none() {
        return Err(AgentRegistryError::InvalidDefinition(
            "unknown model policy",
        ));
    }
    let mut capabilities = BTreeSet::new();
    for capability in &agent.requested_capabilities {
        if !capabilities.insert(format!("{capability:?}")) {
            return Err(AgentRegistryError::InvalidDefinition(
                "duplicate capability",
            ));
        }
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persona_falls_back_when_file_is_absent() {
        let (text, loaded) = load_persona("does/not/exist/Jarvis.md");
        assert!(!loaded);
        assert_eq!(&*text, JARVIS_SYSTEM_FALLBACK);
    }

    #[test]
    fn persona_loads_from_file_when_present() {
        let path = std::env::temp_dir().join("jarvis_persona_test.md");
        std::fs::write(&path, "  Je bent Jarvis, de kern.  \n").unwrap();
        let (text, loaded) = load_persona(path.to_str().unwrap());
        assert!(loaded);
        assert_eq!(&*text, "Je bent Jarvis, de kern.");
        let _ = std::fs::remove_file(&path);
    }

    fn fixture_registry() -> (std::path::PathBuf, String) {
        let directory =
            std::env::temp_dir().join(format!("jarvis-agent-registry-{}", Uuid::now_v7()));
        std::fs::create_dir_all(directory.join("agents")).unwrap();
        let agent = serde_json::json!({
            "id": "research",
            "name": "Research",
            "group": "Development",
            "description": "Synthetic CI fixture.",
            "model_policy": "default",
            "instructions": "Summarise trusted input.",
            "requested_capabilities": ["read_data", "execute_code"],
            "allowed_tools": [],
            "denied_actions": [],
            "limits": {"max_runtime_seconds": 30, "max_context_chars": 1000, "max_output_chars": 500, "max_parallel_runs": 1}
        });
        let bytes = serde_json::to_vec(&agent).unwrap();
        std::fs::write(directory.join("agents/research.json"), &bytes).unwrap();
        let hash = sha256_hex(&bytes);
        std::fs::write(
            directory.join("manifest.json"),
            serde_json::to_vec(&serde_json::json!({
                "version": 1,
                "bundle_id": "bundle-test",
                "agents": [{
                    "id": "research",
                    "path": "agents/research.json",
                    "sha256": hash,
                    "name": "Research",
                    "group": "Development",
                    "model_policy": "default"
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        (directory, hash)
    }

    const VALID_MARKDOWN_AGENT: &str = "---\nschema_version: 1\nid: research\nname: Research\ngroup: Development\ndescription: Synthetic fixture\nmodel_policy: research\nrequested_capabilities:\n  - read_data\nmax_runtime_seconds: 30\nmax_context_chars: 1000\nmax_output_chars: 500\nmax_parallel_runs: 1\n---\n\n# Research\n\nSummarise trusted input.\n";

    #[test]
    fn strict_markdown_loader_accepts_only_versioned_frontmatter() {
        let agent = AgentLoader::parse_markdown(VALID_MARKDOWN_AGENT).unwrap();
        assert_eq!(agent.id, "research");
        assert_eq!(agent.group.as_deref(), Some("Development"));
        assert_eq!(agent.requested_capabilities, vec![Capability::ReadData]);
        assert!(agent.instructions.contains("Summarise"));
        assert!(matches!(
            AgentLoader::parse_markdown("# no frontmatter"),
            Err(AgentRegistryError::MalformedBundle)
        ));
        assert!(matches!(
            AgentLoader::parse_markdown(
                &VALID_MARKDOWN_AGENT.replace("schema_version: 1", "schema_version: 2")
            ),
            Err(AgentRegistryError::MalformedBundle)
        ));
        assert!(matches!(
            AgentLoader::parse_markdown(&VALID_MARKDOWN_AGENT.replace(
                "model_policy: research",
                "model_policy: research\nroot_shell: true"
            )),
            Err(AgentRegistryError::MalformedBundle)
        ));
    }

    #[test]
    fn registry_loads_a_hashed_synthetic_bundle_and_intersects_capabilities() {
        let (directory, _) = fixture_registry();
        let registry = AgentRegistry::load(&directory).unwrap();
        assert_eq!(registry.bundle_id(), "bundle-test");
        assert_eq!(registry.agents().len(), 1);
        assert_eq!(
            registry.effective_capabilities(
                "research",
                &[Capability::ReadData, Capability::ExecuteCode],
                &[Capability::ReadData, Capability::ExecuteCode],
                &[Capability::ReadData],
            ),
            Some(vec![Capability::ReadData])
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn registry_rejects_hash_mismatch_and_traversal() {
        let (directory, _) = fixture_registry();
        std::fs::write(directory.join("agents/research.json"), b"{}").unwrap();
        assert!(matches!(
            AgentRegistry::load(&directory),
            Err(AgentRegistryError::HashMismatch)
        ));
        let _ = std::fs::remove_dir_all(&directory);

        let (directory, _) = fixture_registry();
        let manifest = directory.join("manifest.json");
        let mut parsed: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
        parsed["agents"][0]["path"] = serde_json::Value::String("../outside.json".into());
        std::fs::write(&manifest, serde_json::to_vec(&parsed).unwrap()).unwrap();
        assert!(matches!(
            AgentRegistry::load(&directory),
            Err(AgentRegistryError::UnsafePath)
        ));
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn registry_rejects_manifest_metadata_that_disagrees_with_private_definition() {
        let (directory, _) = fixture_registry();
        let manifest = directory.join("manifest.json");
        let mut parsed: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
        parsed["agents"][0]["group"] = serde_json::Value::String("Trading".into());
        std::fs::write(&manifest, serde_json::to_vec(&parsed).unwrap()).unwrap();
        assert!(matches!(
            AgentRegistry::load(&directory),
            Err(AgentRegistryError::MalformedBundle)
        ));
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn agent_runs_are_bounded_and_cannot_exceed_profile_concurrency() {
        let (directory, _) = fixture_registry();
        let registry = AgentRegistry::load(&directory).unwrap();
        let run = AgentRun::new(
            &registry,
            "research",
            &[Capability::ReadData],
            &[Capability::ReadData],
            &[Capability::ReadData],
            0,
        )
        .unwrap();
        assert_eq!(run.timeout, Duration::from_secs(30));
        assert_eq!(run.effective_capabilities, vec![Capability::ReadData]);
        assert_eq!(run.model_policy, AgentModelPolicy::Standard);
        assert_eq!(run.model_policy.routing_mode(), "auto");
        assert_eq!(
            AgentRun::new(
                &registry,
                "research",
                &[Capability::ReadData],
                &[Capability::ReadData],
                &[Capability::ReadData],
                1,
            ),
            Err(AgentRegistryError::ConcurrencyLimit)
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn agent_model_policy_is_provider_neutral_and_fail_closed() {
        assert_eq!(
            AgentModelPolicy::parse("research"),
            Some(AgentModelPolicy::Research)
        );
        assert_eq!(AgentModelPolicy::parse("gpt-4.1"), None);
        assert_eq!(AgentModelPolicy::Strong.routing_mode(), "deep");
    }

    #[test]
    fn failed_reload_keeps_the_previous_generation_and_runs_are_pinned() {
        let (directory, _) = fixture_registry();
        let loader = AgentLoader::load(&directory).unwrap();
        let first = loader.snapshot();
        let run = AgentRun::from_snapshot(
            &first,
            "research",
            &[Capability::ReadData],
            &[Capability::ReadData],
            &[Capability::ReadData],
            0,
        )
        .unwrap();
        assert_eq!(run.registry_generation, 1);
        std::fs::write(directory.join("agents/research.json"), b"tampered").unwrap();
        assert!(matches!(
            loader.reload(),
            Err(AgentRegistryError::HashMismatch)
        ));
        assert_eq!(loader.snapshot().generation, 1);
        assert_eq!(run.bundle_id, first.registry.bundle_id());
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn change_proposal_has_no_write_capability() {
        let proposal = AgentChangeProposal {
            id: Uuid::now_v7(),
            agent_id: "research".into(),
            base_bundle_id: "bundle-test".into(),
            summary: "Request a wording change".into(),
            unified_diff: "--- a/agents/research.md\n+++ b/agents/research.md".into(),
        };
        assert_eq!(proposal.agent_id, "research");
        assert!(!proposal.unified_diff.is_empty());
    }
}
