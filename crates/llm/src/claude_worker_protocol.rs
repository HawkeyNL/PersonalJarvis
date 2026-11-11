//! Finite local protocol between Core and the subscription-only Claude worker.
//! Account mutation and credential material are deliberately absent.

use serde::{Deserialize, Serialize};

pub const SOCKET: &str = "/run/jarvis-claude.sock";
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
pub const MAX_REPLY_BYTES: usize = 256 * 1024;
/// Longest system text a worker accepts. Core keeps persona plus runtime
/// facts within it, so the whole request stays under [`MAX_REQUEST_BYTES`].
pub const MAX_SYSTEM_BYTES: usize = 12 * 1024;
/// Longest research prompt a worker accepts (the bounded question in UTF-8).
pub const MAX_RESEARCH_PROMPT_BYTES: usize = 4 * crate::types::MAX_RESEARCH_QUESTION_CHARS;
/// Longest task input: one JSON object of data.
pub const MAX_TASK_PROMPT_BYTES: usize = 16 * 1024;
/// Most photos one chat request carries.
pub const MAX_IMAGES: usize = 4;
/// Largest decoded photo: Core's sanitizer never stores a bigger one.
pub const MAX_IMAGE_BYTES: usize = 1024 * 1024;
/// Largest request line with photos. A request without photos keeps
/// [`MAX_REQUEST_BYTES`].
pub const MAX_IMAGE_REQUEST_BYTES: usize = 6 * 1024 * 1024;
const _: () = assert!(
    MAX_IMAGE_REQUEST_BYTES > MAX_REQUEST_BYTES + MAX_IMAGES * (MAX_IMAGE_BYTES * 4 / 3 + 64)
);

/// The reviewed CLI contract is Claude Code 2.1.248+ within its 2.1 line.
/// `--restricted` first appeared in 2.1.248. A later minor/major line needs
/// explicit review before the subscription worker can run it.
pub fn reviewed_claude_version(output: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(output) else {
        return false;
    };
    let versions = text
        .split(|ch: char| !(ch.is_ascii_digit() || ch == '.'))
        .filter_map(|candidate| semver::Version::parse(candidate).ok())
        .collect::<Vec<_>>();
    matches!(versions.as_slice(), [version] if version.major == 2 && version.minor == 1 && version.patch >= 248)
}

/// Conservative projection of official CLI JSON. Unknown billing metadata
/// never authorizes a subscription run.
pub fn claude_subscription_status(bytes: &[u8]) -> &'static str {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return "unhealthy";
    };
    if value.get("loggedIn").and_then(serde_json::Value::as_bool) != Some(true) {
        return "logged_out";
    }
    if value.get("authMethod").and_then(serde_json::Value::as_str) != Some("claude.ai")
        || value.get("apiProvider").and_then(serde_json::Value::as_str) != Some("firstParty")
    {
        return "wrong_auth_mode";
    }
    match value
        .get("subscriptionType")
        .and_then(serde_json::Value::as_str)
    {
        Some("pro" | "max" | "team" | "enterprise") => "connected",
        _ => "wrong_auth_mode",
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeWorkerRequest {
    pub protocol: u8,
    pub model: String,
    pub system: Option<String>,
    pub prompt: String,
    /// Owner-enabled research run: the worker allows only the provider-hosted
    /// web search tool. It carries only the bounded question and the fixed
    /// research prompt. Omitted when false, so ordinary requests keep their
    /// exact shape.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub research: bool,
    /// A typed task: its fixed system prompt, a JSON object as input and no
    /// tool at all. Omitted when absent, so ordinary requests keep their exact
    /// shape; a worker that predates the field refuses it as unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<WorkerTask>,
    /// The owner's photos for an ordinary chat run, oldest first: `[photo N]`
    /// in the prompt is the Nth. Never on a research run or a task. Omitted
    /// when empty, so a request without photos keeps its exact shape.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<WorkerImage>,
}

/// One photo on the wire: a base64 JPEG. Debug never prints the data.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerImage {
    pub media_type: String,
    pub data: String,
}

impl WorkerImage {
    pub fn jpeg(bytes: &[u8]) -> Self {
        use base64::Engine;
        Self {
            media_type: "image/jpeg".into(),
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
        }
    }

    /// The decoded photo, only when it is a whole, bounded JPEG: the worker
    /// re-checks what Core sent.
    pub fn decoded(&self) -> Option<Vec<u8>> {
        use base64::Engine;
        if self.media_type != "image/jpeg" || self.data.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4 {
            return None;
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&self.data)
            .ok()?;
        (bytes.len() <= MAX_IMAGE_BYTES
            && bytes.starts_with(&[0xFF, 0xD8, 0xFF])
            && bytes.ends_with(&[0xFF, 0xD9]))
        .then_some(bytes)
    }
}

impl std::fmt::Debug for WorkerImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "<{} base64 {} bytes>",
            if self.media_type == "image/jpeg" {
                "jpeg"
            } else {
                "other"
            },
            self.data.len()
        )
    }
}

/// The fixed-prompt tasks the Claude worker runs. Each one shares the single
/// background slot with research, so chat always keeps a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerTask {
    MemeDecision,
    MemeStrategyReview,
    DependencyReview,
}

impl WorkerTask {
    /// The only system text a request for this task may carry.
    pub fn system_prompt(self) -> &'static str {
        match self {
            Self::MemeDecision => crate::meme_prompts::MEME_DECISION_SYSTEM_PROMPT,
            Self::MemeStrategyReview => crate::meme_prompts::MEME_STRATEGY_REVIEW_SYSTEM_PROMPT,
            Self::DependencyReview => crate::job_prompts::DEPENDENCY_REVIEW_SYSTEM_PROMPT,
        }
    }
}

impl ClaudeWorkerRequest {
    pub fn valid(&self) -> bool {
        let prompt_limit = if self.research {
            MAX_RESEARCH_PROMPT_BYTES
        } else if self.task.is_some() {
            MAX_TASK_PROMPT_BYTES
        } else {
            48 * 1024
        };
        self.protocol == 1
            && !self.prompt.trim().is_empty()
            && self.prompt.len() <= prompt_limit
            // Nothing but the fixed research prompt rides along with a search.
            && (!self.research
                || self.system.as_deref() == Some(crate::types::RESEARCH_SYSTEM_PROMPT))
            // A task carries only its fixed prompt and one JSON object, never
            // a web search.
            && self.task.is_none_or(|task| {
                !self.research
                    && self.system.as_deref() == Some(task.system_prompt())
                    && serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(
                        &self.prompt,
                    )
                    .is_ok()
            })
            && self
                .system
                .as_deref()
                .is_none_or(|value| value.len() <= MAX_SYSTEM_BYTES)
            && valid_worker_model(&self.model)
            // Photos only on an ordinary chat run, each a bounded JPEG.
            && (self.images.is_empty()
                || (!self.research
                    && self.task.is_none()
                    && self.images.len() <= MAX_IMAGES
                    && self.images.iter().all(|image| image.decoded().is_some())))
    }

    /// The largest request line a worker reads for this request.
    pub fn max_line_bytes(&self) -> usize {
        if self.images.is_empty() {
            MAX_REQUEST_BYTES
        } else {
            MAX_IMAGE_REQUEST_BYTES
        }
    }
}

/// The model ids a subscription worker accepts. The owner's `jarvis models
/// register` applies the same rule, so a registered pair is always usable.
pub fn valid_worker_model(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 80
        // Never let a model id read as a CLI option.
        && !model.starts_with('-')
        && model
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeWorkerState {
    Completed,
    SubscriptionUnavailable,
    PlanLimit,
    IncompatibleRuntime,
    /// The subscription account cannot use the requested model (for example
    /// a staged model rollout). Never a reason to try a paid API instead.
    ModelUnavailable,
    RuntimeFailure,
    /// The run tried a tool (command, search, MCP, file change), searched for
    /// a query the identifier check flags, or emitted an event the worker
    /// cannot prove harmless; it was stopped and its answer discarded. The
    /// Codex chat worker returns this, and the Claude worker in research runs.
    ToolUseRefused,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeWorkerReply {
    pub protocol: u8,
    pub state: ClaudeWorkerState,
    pub text: Option<String>,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub cache_read_tokens: Option<u32>,
    pub cache_write_tokens: Option<u32>,
}

impl ClaudeWorkerReply {
    pub fn failure(state: ClaudeWorkerState) -> Self {
        Self {
            protocol: 1,
            state,
            text: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_protocol_has_no_account_mutation_or_host_execution_fields() {
        let malicious =
            r#"{"protocol":1,"model":"claude","prompt":"hello","system":null,"login":true}"#;
        assert!(serde_json::from_str::<ClaudeWorkerRequest>(malicious).is_err());
        let request = ClaudeWorkerRequest {
            protocol: 1,
            model: "claude-sonnet-5".into(),
            system: None,
            prompt: "hello".into(),
            research: false,
            task: None,
            images: Vec::new(),
        };
        assert!(request.valid());
        assert!(!serde_json::to_string(&request)
            .unwrap()
            .contains("research"));
        let research = ClaudeWorkerRequest {
            protocol: 1,
            model: "claude-sonnet-5".into(),
            system: Some(crate::types::RESEARCH_SYSTEM_PROMPT.into()),
            prompt: "x".repeat(MAX_RESEARCH_PROMPT_BYTES),
            research: true,
            task: None,
            images: Vec::new(),
        };
        assert!(research.valid());
        assert!(!ClaudeWorkerRequest {
            prompt: "x".repeat(MAX_RESEARCH_PROMPT_BYTES + 1),
            system: Some(crate::types::RESEARCH_SYSTEM_PROMPT.into()),
            ..research
        }
        .valid());
        // A research run carries no persona or other system text.
        for system in [
            None,
            Some("You are Jarvis. The owner lives in ...".to_owned()),
        ] {
            assert!(!ClaudeWorkerRequest {
                protocol: 1,
                model: "claude-sonnet-5".into(),
                system,
                prompt: "hello".into(),
                research: true,
                task: None,
                images: Vec::new(),
            }
            .valid());
        }
        assert!(!ClaudeWorkerRequest {
            model: "x;sh".into(),
            ..request
        }
        .valid());
        assert!(!ClaudeWorkerRequest {
            protocol: 1,
            model: "-c".into(),
            system: None,
            prompt: "hello".into(),
            research: false,
            task: None,
            images: Vec::new(),
        }
        .valid());
    }

    fn task_request(task: WorkerTask, prompt: String) -> ClaudeWorkerRequest {
        ClaudeWorkerRequest {
            protocol: 1,
            model: "claude-sonnet-5".into(),
            system: Some(task.system_prompt().into()),
            prompt,
            research: false,
            task: Some(task),
            images: Vec::new(),
        }
    }

    #[test]
    fn task_requests_carry_only_their_fixed_prompt_and_a_bounded_json_object() {
        for task in [
            WorkerTask::MemeDecision,
            WorkerTask::MemeStrategyReview,
            WorkerTask::DependencyReview,
        ] {
            let ok = task_request(task, r#"{"c1":{"liq_usd":1200.5}}"#.into());
            assert!(ok.valid());
            let wire = serde_json::to_value(&ok).unwrap();
            assert!(wire["task"].is_string());
            assert!(wire.get("research").is_none());

            let mut wrong_system = task_request(task, "{}".into());
            for system in [
                None,
                Some("You are Jarvis.".to_owned()),
                Some(crate::types::RESEARCH_SYSTEM_PROMPT.to_owned()),
                Some(format!("{} ", task.system_prompt())),
            ] {
                wrong_system.system = system;
                assert!(!wrong_system.valid());
            }
            for prompt in ["[1,2]", "42", "\"x\"", "not json", "{} {}", " "] {
                assert!(!task_request(task, prompt.into()).valid(), "{prompt}");
            }
            let padding = MAX_TASK_PROMPT_BYTES - r#"{"p":""}"#.len();
            let largest = format!(r#"{{"p":"{}"}}"#, "x".repeat(padding));
            assert!(task_request(task, largest.clone()).valid());
            let oversize = format!(r#"{{"p":"{}"}}"#, "x".repeat(padding + 1));
            assert!(!task_request(task, oversize).valid());
            assert!(!ClaudeWorkerRequest {
                research: true,
                ..task_request(task, "{}".into())
            }
            .valid());
        }
        // The other task's prompt is not this task's prompt.
        let mut swapped = task_request(WorkerTask::MemeDecision, "{}".into());
        swapped.system = Some(WorkerTask::MemeStrategyReview.system_prompt().into());
        assert!(!swapped.valid());
        let mut swapped = task_request(WorkerTask::DependencyReview, "{}".into());
        swapped.system = Some(WorkerTask::MemeStrategyReview.system_prompt().into());
        assert!(!swapped.valid());
        let wire = serde_json::to_value(task_request(WorkerTask::DependencyReview, "{}".into()));
        assert_eq!(wire.unwrap()["task"], "dependency_review");
        // Unknown task names are refused at the wire.
        let unknown = r#"{"protocol":1,"model":"m","system":null,"prompt":"{}","task":"shell"}"#;
        assert!(serde_json::from_str::<ClaudeWorkerRequest>(unknown).is_err());
    }

    #[test]
    fn requests_without_a_task_keep_their_wire_shape_both_ways() {
        // What an older Core sends still parses, as an ordinary request.
        let old = r#"{"protocol":1,"model":"m","system":null,"prompt":"hi"}"#;
        let request = serde_json::from_str::<ClaudeWorkerRequest>(old).unwrap();
        assert!(request.task.is_none() && request.images.is_empty() && request.valid());
        assert_eq!(serde_json::to_string(&request).unwrap(), old);
        assert_eq!(request.max_line_bytes(), MAX_REQUEST_BYTES);
    }

    /// A tiny whole JPEG: start and end markers around a marker byte.
    fn jpeg(marker: u8) -> Vec<u8> {
        vec![0xFF, 0xD8, 0xFF, 0xE0, marker, 0xFF, 0xD9]
    }

    fn photo_request(images: Vec<WorkerImage>) -> ClaudeWorkerRequest {
        ClaudeWorkerRequest {
            protocol: 1,
            model: "claude-sonnet-5".into(),
            system: Some("You are Jarvis.".into()),
            prompt: "User: [photo 1]\nWhat is this?\nJarvis:".into(),
            research: false,
            task: None,
            images,
        }
    }

    #[test]
    fn photos_ride_only_on_chat_runs_as_bounded_jpegs() {
        let four = (0..4)
            .map(|n| WorkerImage::jpeg(&jpeg(n)))
            .collect::<Vec<_>>();
        let request = photo_request(four.clone());
        assert!(request.valid());
        assert_eq!(request.max_line_bytes(), MAX_IMAGE_REQUEST_BYTES);
        assert_eq!(four[2].decoded(), Some(jpeg(2)));

        let mut five = four.clone();
        five.push(WorkerImage::jpeg(&jpeg(4)));
        assert!(!photo_request(five).valid());
        // Never with a web search or a typed task.
        assert!(!ClaudeWorkerRequest {
            research: true,
            system: Some(crate::types::RESEARCH_SYSTEM_PROMPT.into()),
            prompt: "question".into(),
            ..photo_request(four.clone())
        }
        .valid());
        let task = WorkerTask::MemeDecision;
        assert!(!ClaudeWorkerRequest {
            task: Some(task),
            system: Some(task.system_prompt().into()),
            prompt: "{}".into(),
            ..photo_request(four.clone())
        }
        .valid());

        let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0xFF, 0xD9];
        let mut largest = jpeg(0);
        largest.splice(4..5, vec![0; MAX_IMAGE_BYTES - largest.len() + 1]);
        assert_eq!(largest.len(), MAX_IMAGE_BYTES);
        assert!(photo_request(vec![WorkerImage::jpeg(&largest)]).valid());
        let mut oversized = largest.clone();
        oversized.insert(4, 0);
        let mut wrong_type = WorkerImage::jpeg(&jpeg(0));
        wrong_type.media_type = "image/png".into();
        for bad in [
            WorkerImage::jpeg(&png),
            WorkerImage::jpeg(&jpeg(0)[..5]),
            WorkerImage::jpeg(&oversized),
            WorkerImage::jpeg(&[]),
            wrong_type,
            WorkerImage {
                media_type: "image/jpeg".into(),
                data: "not base64!".into(),
            },
        ] {
            assert!(bad.decoded().is_none(), "{bad:?}");
            assert!(!photo_request(vec![bad]).valid());
        }
    }

    #[test]
    fn photo_data_is_never_printed_and_unknown_photo_fields_are_refused() {
        let image = WorkerImage::jpeg(&jpeg(7));
        let printed = format!("{:?}", photo_request(vec![image.clone()]));
        assert!(printed.contains("<jpeg base64 12 bytes>"), "{printed}");
        assert!(!printed.contains(&image.data), "{printed}");
        let wire = serde_json::to_value(photo_request(vec![image])).unwrap();
        assert_eq!(wire["images"][0]["media_type"], "image/jpeg");
        let mut extra = wire.clone();
        extra["images"][0]["url"] = "https://example.org/x.jpg".into();
        assert!(serde_json::from_value::<ClaudeWorkerRequest>(extra).is_err());
    }

    #[test]
    fn runtime_version_review_gate_is_fail_closed() {
        assert!(reviewed_claude_version(b"2.1.248 (Claude Code)"));
        assert!(reviewed_claude_version(b"Claude Code v2.1.300\n"));
        assert!(!reviewed_claude_version(b"2.1.247 (Claude Code)"));
        assert!(!reviewed_claude_version(b"2.2.0 (Claude Code)"));
        assert!(!reviewed_claude_version(b"2.1.248 9.9.9"));
        assert!(!reviewed_claude_version(b"unrecognized"));
    }
}
