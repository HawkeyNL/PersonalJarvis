//! Resource & agent registry — Jarvis' "instant memory" of what it can use.
//!
//! Detects the host (hardware/software) and the available *brains* (the Claude
//! plan via the CLI, the metered API, local Ollama), with each one's cost tier
//! and availability. Feeds the cost-aware router (ADR-027) so work can go to the
//! cheapest capable backend, and gives the UI something to show.
//!
//! Probes are best-effort: a missing tool yields `present: false`, never an
//! error — collecting the registry always succeeds.

use serde::Serialize;
use tokio::process::Command;

/// A full snapshot of what Jarvis can run and run on.
#[derive(Debug, Clone, Serialize)]
pub struct Registry {
    pub host: HostInfo,
    pub software: Vec<SoftwareItem>,
    pub brains: Vec<Brain>,
    /// The models Jarvis can pick from (catalog — ADR-028). Curated cloud models
    /// per keyed provider + the local Ollama models actually installed.
    pub models: Vec<ModelEntry>,
    /// Label of the brain currently wired as the router's primary chain.
    pub active_brain: String,
}

/// What a model is good for — drives "cheapest sufficient" model choice (ADR-028).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelClass {
    /// Small/fast — the default for most tasks.
    Light,
    /// Balanced general work.
    Mid,
    /// Strong — for hard tasks and planning.
    Heavy,
    /// Explicit reasoning models (o-series, deepseek-reasoner).
    Reasoning,
}

/// A rough cost band for a model (exact prices live in `jarvis-usage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelCost {
    /// Runs locally, no marginal cost.
    Local,
    /// Cheap per-token (e.g. DeepSeek, mini/haiku tiers).
    Cheap,
    /// Mid per-token.
    Mid,
    /// Expensive per-token (top models).
    Pricey,
}

/// One model in the catalog.
#[derive(Debug, Clone, Serialize)]
pub struct ModelEntry {
    pub id: String,
    /// Which backend serves it (`anthropic-api`, `openai-api`, `deepseek-api`,
    /// `ollama`).
    pub backend: String,
    pub class: ModelClass,
    pub cost: ModelCost,
    pub available: bool,
}

/// The machine Jarvis runs on.
#[derive(Debug, Clone, Serialize)]
pub struct HostInfo {
    pub os: String,
    pub arch: String,
    pub cpu: String,
    pub cpu_cores: usize,
    pub mem_total_gb: f32,
    pub gpu: String,
}

/// A relevant tool/model on the host.
#[derive(Debug, Clone, Serialize)]
pub struct SoftwareItem {
    pub name: String,
    pub present: bool,
    pub version: Option<String>,
    pub detail: Option<String>,
}

/// How a brain is paid for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CostTier {
    /// Flat subscription (your Claude plan).
    Plan,
    /// Per-token API billing.
    Metered,
    /// Runs locally, no marginal cost.
    Local,
}

/// A usable brain/agent and its economics.
#[derive(Debug, Clone, Serialize)]
pub struct Brain {
    pub id: String,
    pub label: String,
    pub cost: CostTier,
    pub available: bool,
    pub note: String,
}

/// What the api service already knows; the registry probes the rest.
#[derive(Debug, Clone, Default)]
pub struct CollectInput {
    pub llm_provider: String,
    pub claude_cli_bin: String,
    pub has_api_key: bool,
    pub anthropic_model: String,
    pub anthropic_model_hard: String,
    pub anthropic_model_cheap: String,
    pub ollama_model: String,
    /// OpenAI: whether a key is set + its per-tier models (for the catalog).
    pub has_openai_key: bool,
    pub openai_model: String,
    pub openai_model_hard: String,
    pub openai_model_cheap: String,
    /// DeepSeek: whether a key is set + its per-tier models.
    pub has_deepseek_key: bool,
    pub deepseek_model: String,
    pub deepseek_model_hard: String,
    pub deepseek_model_cheap: String,
    pub speech_provider: String,
    pub whisper_model: Option<String>,
    /// The built router's label (e.g. `claude-cli:…→anthropic:…`).
    pub active_brain: String,
}

/// Collect a fresh registry snapshot. Always succeeds (probes degrade to absent).
pub async fn collect(input: &CollectInput) -> Registry {
    let host = host_info();

    let claude_ver = probe_version(&input.claude_cli_bin, "--version").await;
    let ollama_ver = probe_version("ollama", "--version").await;
    let ollama_models = if ollama_ver.is_some() {
        probe_ollama_models().await
    } else {
        Vec::new()
    };
    let cmake_ver = probe_version("cmake", "--version").await;
    let whisper_present = input
        .whisper_model
        .as_deref()
        .map(|p| std::path::Path::new(p).exists())
        .unwrap_or(false);

    let software = vec![
        SoftwareItem {
            name: "claude CLI".into(),
            present: claude_ver.is_some(),
            version: claude_ver.clone(),
            detail: Some("brein via je abonnement".into()),
        },
        SoftwareItem {
            name: "ollama".into(),
            present: ollama_ver.is_some(),
            version: ollama_ver,
            detail: (!ollama_models.is_empty()).then(|| format!("{} lokaal model(len)", ollama_models.len())),
        },
        SoftwareItem {
            name: "cmake".into(),
            present: cmake_ver.is_some(),
            version: cmake_ver,
            detail: Some("nodig voor whisper-STT".into()),
        },
        SoftwareItem {
            name: "whisper-model".into(),
            present: whisper_present,
            version: None,
            detail: input.whisper_model.clone(),
        },
    ];

    let brains = derive_brains(input, claude_ver.is_some(), &ollama_models);
    let models = derive_models(input, &ollama_models);

    Registry {
        host,
        software,
        brains,
        models,
        active_brain: input.active_brain.clone(),
    }
}

/// Build the model catalog (pure — testable): curated cloud models per keyed
/// provider (class by tier role, cost by band) + the local Ollama models that
/// are actually installed. Deduplicated per backend (a provider often reuses the
/// same model across tiers). See ADR-028.
fn derive_models(input: &CollectInput, ollama_models: &[String]) -> Vec<ModelEntry> {
    let mut out = Vec::new();

    // Class from a model's tier role, sharpened to Reasoning by name.
    let class_of = |name: &str, role_default: ModelClass| {
        let n = name.to_ascii_lowercase();
        if n.contains("reason") || n.contains("-o1") || n.contains("-o3") || n.contains("o4-mini") {
            ModelClass::Reasoning
        } else {
            role_default
        }
    };

    // Add the cheap/default/hard trio for one keyed cloud provider.
    let mut add_cloud =
        |backend: &str, available: bool, cheap: &str, default: &str, hard: &str, cheap_cost: ModelCost| {
            let trio = [
                (cheap, ModelClass::Light, cheap_cost),
                (default, ModelClass::Mid, ModelCost::Mid),
                (hard, ModelClass::Heavy, ModelCost::Pricey),
            ];
            for (id, role_class, cost) in trio {
                if id.is_empty() || out.iter().any(|m: &ModelEntry| m.id == id && m.backend == backend) {
                    continue; // skip blanks + duplicates (e.g. default == hard)
                }
                out.push(ModelEntry {
                    id: id.to_string(),
                    backend: backend.to_string(),
                    class: class_of(id, role_class),
                    // DeepSeek is cheap across the board; others keep the band.
                    cost: if backend == "deepseek-api" { ModelCost::Cheap } else { cost },
                    available,
                });
            }
        };

    add_cloud(
        "anthropic-api",
        input.has_api_key,
        &input.anthropic_model_cheap,
        &input.anthropic_model,
        &input.anthropic_model_hard,
        ModelCost::Cheap,
    );
    add_cloud(
        "openai-api",
        input.has_openai_key,
        &input.openai_model_cheap,
        &input.openai_model,
        &input.openai_model_hard,
        ModelCost::Cheap,
    );
    add_cloud(
        "deepseek-api",
        input.has_deepseek_key,
        &input.deepseek_model_cheap,
        &input.deepseek_model,
        &input.deepseek_model_hard,
        ModelCost::Cheap,
    );

    // Local Ollama models actually installed — all free; treat as light by default.
    for id in ollama_models {
        out.push(ModelEntry {
            id: id.clone(),
            backend: "ollama".into(),
            class: ModelClass::Light,
            cost: ModelCost::Local,
            available: true,
        });
    }

    out
}

/// Build the brain list (pure — testable) from config + probe results.
fn derive_brains(input: &CollectInput, claude_present: bool, ollama_models: &[String]) -> Vec<Brain> {
    let ollama_available = !ollama_models.is_empty();
    vec![
        Brain {
            id: "claude-cli".into(),
            label: format!("Claude-abonnement · {}", input.anthropic_model),
            cost: CostTier::Plan,
            available: claude_present,
            note: if claude_present {
                "headless `claude` CLI (plan)".into()
            } else {
                format!("`{}` niet gevonden", input.claude_cli_bin)
            },
        },
        Brain {
            id: "anthropic-api".into(),
            label: format!("Anthropic API · {}", input.anthropic_model),
            cost: CostTier::Metered,
            available: input.has_api_key,
            note: if input.has_api_key {
                "per-token (vangnet)".into()
            } else {
                "geen API-key gezet".into()
            },
        },
        Brain {
            id: "openai-api".into(),
            label: format!("OpenAI API · {}", input.openai_model),
            cost: CostTier::Metered,
            available: input.has_openai_key,
            note: if input.has_openai_key {
                "per-token".into()
            } else {
                "geen API-key gezet".into()
            },
        },
        Brain {
            id: "deepseek-api".into(),
            label: format!("DeepSeek API · {}", input.deepseek_model),
            cost: CostTier::Metered,
            available: input.has_deepseek_key,
            note: if input.has_deepseek_key {
                "per-token (goedkoop)".into()
            } else {
                "geen API-key gezet".into()
            },
        },
        Brain {
            id: "ollama".into(),
            label: format!("Ollama lokaal · {}", input.ollama_model),
            cost: CostTier::Local,
            available: ollama_available,
            note: if ollama_available {
                format!("{} lokaal model(len)", ollama_models.len())
            } else {
                "ollama niet actief".into()
            },
        },
    ]
}

fn host_info() -> HostInfo {
    use sysinfo::System;
    let sys = System::new_all();
    let cpu = sys
        .cpus()
        .first()
        .map(|c| c.brand().trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "onbekend".into());
    let cores = sys.cpus().len();
    let mem_gb = sys.total_memory() as f32 / 1024.0 / 1024.0 / 1024.0;
    let arch = std::env::consts::ARCH.to_string();
    let os = System::long_os_version()
        .or_else(System::name)
        .unwrap_or_else(|| std::env::consts::OS.to_string());
    let gpu = if std::env::consts::OS == "macos" && arch == "aarch64" {
        "Apple Silicon (geïntegreerde GPU)".into()
    } else {
        "onbekend".into()
    };
    HostInfo {
        os,
        arch,
        cpu,
        cpu_cores: cores,
        mem_total_gb: (mem_gb * 10.0).round() / 10.0,
        gpu,
    }
}

/// Run `bin arg` and return its first stdout line, or `None` if it isn't there.
async fn probe_version(bin: &str, arg: &str) -> Option<String> {
    let out = Command::new(bin).arg(arg).output().await.ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
}

async fn probe_ollama_models() -> Vec<String> {
    match Command::new("ollama").arg("list").output().await {
        Ok(out) if out.status.success() => {
            parse_ollama_list(&String::from_utf8_lossy(&out.stdout))
        }
        _ => Vec::new(),
    }
}

/// Parse `ollama list` (header row then `NAME  ID  SIZE  MODIFIED`) → model names.
fn parse_ollama_list(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .skip(1) // header
        .filter_map(|l| l.split_whitespace().next())
        .filter(|n| !n.is_empty())
        .map(|n| n.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> CollectInput {
        CollectInput {
            llm_provider: "claude-cli".into(),
            claude_cli_bin: "claude".into(),
            has_api_key: true,
            anthropic_model: "claude-sonnet-5".into(),
            anthropic_model_hard: "claude-opus-5".into(),
            anthropic_model_cheap: "claude-haiku-4-5".into(),
            ollama_model: "llama3.2".into(),
            has_openai_key: true,
            openai_model: "gpt-4o".into(),
            openai_model_hard: "gpt-4o".into(),
            openai_model_cheap: "gpt-4o-mini".into(),
            has_deepseek_key: true,
            deepseek_model: "deepseek-chat".into(),
            deepseek_model_hard: "deepseek-reasoner".into(),
            deepseek_model_cheap: "deepseek-chat".into(),
            speech_provider: "whisper".into(),
            whisper_model: Some("models/ggml-base.bin".into()),
            active_brain: "claude-cli:…→anthropic:…".into(),
        }
    }

    fn brain<'a>(brains: &'a [Brain], id: &str) -> &'a Brain {
        brains.iter().find(|b| b.id == id).expect("brain present")
    }

    #[test]
    fn parses_ollama_list() {
        let out = "NAME            ID          SIZE    MODIFIED\nllama3.2:latest a1b2  2.0 GB  2 days ago\nqwen2.5:7b   c3d4  4.7 GB  1 week ago\n";
        assert_eq!(parse_ollama_list(out), vec!["llama3.2:latest", "qwen2.5:7b"]);
    }

    #[test]
    fn brains_reflect_availability_and_cost() {
        let brains = derive_brains(&input(), true, &["llama3.2:latest".to_string()]);
        let cli = brain(&brains, "claude-cli");
        assert_eq!(cli.cost, CostTier::Plan);
        assert!(cli.available);
        assert_eq!(brain(&brains, "anthropic-api").cost, CostTier::Metered);
        assert!(brain(&brains, "anthropic-api").available); // has_api_key
        assert!(brain(&brains, "openai-api").available); // has_openai_key
        assert!(brain(&brains, "deepseek-api").available); // has_deepseek_key
        assert_eq!(brain(&brains, "ollama").cost, CostTier::Local);
        assert!(brain(&brains, "ollama").available); // one ollama model present
    }

    #[test]
    fn missing_tools_are_unavailable() {
        let mut i = input();
        i.has_api_key = false;
        i.has_openai_key = false;
        i.has_deepseek_key = false;
        let brains = derive_brains(&i, false, &[]);
        assert!(!brain(&brains, "claude-cli").available); // no claude binary
        assert!(brain(&brains, "claude-cli").note.contains("niet gevonden"));
        assert!(!brain(&brains, "anthropic-api").available); // no api key
        assert!(!brain(&brains, "openai-api").available); // no openai key
        assert!(!brain(&brains, "deepseek-api").available); // no deepseek key
        assert!(!brain(&brains, "ollama").available); // no ollama models
    }

    #[test]
    fn model_catalog_classes_and_dedups() {
        let models = derive_models(&input(), &["llama3.2:latest".to_string()]);
        let get = |id: &str| models.iter().find(|m| m.id == id).expect("model present");
        // Class by tier role, sharpened for reasoners.
        assert_eq!(get("claude-haiku-4-5").class, ModelClass::Light);
        assert_eq!(get("claude-opus-5").class, ModelClass::Heavy);
        assert_eq!(get("deepseek-reasoner").class, ModelClass::Reasoning);
        // DeepSeek is cheap across the board; Ollama is local.
        assert_eq!(get("deepseek-chat").cost, ModelCost::Cheap);
        assert_eq!(get("llama3.2:latest").cost, ModelCost::Local);
        // gpt-4o is default==hard for this config → appears once.
        assert_eq!(models.iter().filter(|m| m.id == "gpt-4o").count(), 1);
    }

    #[test]
    fn model_catalog_marks_unkeyed_providers_unavailable() {
        let mut i = input();
        i.has_openai_key = false;
        let models = derive_models(&i, &[]);
        // Known models stay visible ("ecosystem"), just flagged unavailable.
        let openai: Vec<_> = models.iter().filter(|m| m.backend == "openai-api").collect();
        assert!(!openai.is_empty());
        assert!(openai.iter().all(|m| !m.available));
        assert!(models
            .iter()
            .filter(|m| m.backend == "anthropic-api")
            .all(|m| m.available));
    }

    #[tokio::test]
    async fn collect_always_succeeds() {
        // Even with bogus tool names, collection returns a populated registry.
        let mut i = input();
        i.claude_cli_bin = "definitely-not-a-real-binary-xyz".into();
        let reg = collect(&i).await;
        assert_eq!(reg.brains.len(), 5);
        assert!(reg.host.cpu_cores >= 1);
        assert!(!reg.host.os.is_empty());
    }
}
