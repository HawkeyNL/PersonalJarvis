//! Subscription-backed Claude brain through a local unprivileged worker.
//! Core never executes the official CLI or reads its credential store.

use std::time::Duration;

use async_trait::async_trait;
use tokio::{io::AsyncReadExt, io::AsyncWriteExt, net::UnixStream};

use crate::{
    claude_worker_protocol::{
        ClaudeWorkerReply, ClaudeWorkerRequest, ClaudeWorkerState, WorkerImage, WorkerTask,
        MAX_CHAT_PROMPT_BYTES, MAX_REPLY_BYTES, MAX_REQUEST_BYTES, MAX_SYSTEM_BYTES, SOCKET,
    },
    types::{
        ChatMessage, ChatReply, ChatRequest, LlmError, ResearchRequest, Role, Tier, Usage,
        RESEARCH_SYSTEM_PROMPT,
    },
    LlmProvider,
};

const WORKER_TIMEOUT: Duration = Duration::from_secs(125);
/// Research runs search the web first; the worker stops them at 300 s.
const RESEARCH_WORKER_TIMEOUT: Duration = Duration::from_secs(310);
/// Typed tasks run without tools; the worker stops them at 90 s.
const TASK_WORKER_TIMEOUT: Duration = Duration::from_secs(100);

pub struct ClaudeCliProvider {
    model_default: String,
    model_hard: String,
    model_cheap: String,
    label: String,
}

impl ClaudeCliProvider {
    pub fn new(
        _legacy_binary: impl Into<String>,
        model_default: impl Into<String>,
        model_hard: impl Into<String>,
        model_cheap: impl Into<String>,
    ) -> Self {
        // Preserve constructor compatibility, never execute a caller path in Core.
        let model_default = model_default.into();
        Self {
            label: format!("claude-cli:{model_default}"),
            model_default,
            model_hard: model_hard.into(),
            model_cheap: model_cheap.into(),
        }
    }

    fn model_for(&self, tier: Tier) -> &str {
        match tier {
            Tier::Default => &self.model_default,
            Tier::Hard => &self.model_hard,
            Tier::Cheap => &self.model_cheap,
        }
    }

    /// A typed, fixed-prompt task sent straight to the Claude subscription
    /// worker. It never goes through the router: a busy worker (refused, never
    /// queued), a plan limit or an outage is an error for the caller, never a
    /// fallback to a paid API or another provider.
    pub async fn meme_task(
        &self,
        task: WorkerTask,
        input: &serde_json::Value,
    ) -> Result<ChatReply, LlmError> {
        CLAUDE_WORKER
            .send(self.task_request(task, input), TASK_WORKER_TIMEOUT)
            .await
    }

    fn task_request(&self, task: WorkerTask, input: &serde_json::Value) -> ClaudeWorkerRequest {
        let model = match task {
            WorkerTask::MemeDecision => &self.model_default,
            WorkerTask::MemeStrategyReview | WorkerTask::DependencyReview => &self.model_hard,
        };
        ClaudeWorkerRequest {
            protocol: 1,
            model: model.clone(),
            system: Some(task.system_prompt().to_owned()),
            prompt: input.to_string(),
            research: false,
            task: Some(task),
            images: Vec::new(),
        }
    }
}

#[async_trait]
impl LlmProvider for ClaudeCliProvider {
    fn label(&self) -> &str {
        &self.label
    }

    async fn chat(&self, req: &ChatRequest) -> Result<ChatReply, LlmError> {
        let model = req
            .model
            .clone()
            .unwrap_or_else(|| self.model_for(req.tier).to_owned());
        CLAUDE_WORKER.chat(model, req).await
    }

    async fn research(&self, req: &ResearchRequest) -> Result<ChatReply, LlmError> {
        let model = req.model.clone().unwrap_or_else(|| self.model_hard.clone());
        CLAUDE_WORKER.research(model, req).await
    }
}

/// One local subscription worker: a fixed socket speaking the finite worker
/// protocol. Errors are fixed, non-secret strings.
pub(crate) struct SubscriptionWorker {
    pub(crate) socket: &'static str,
    pub(crate) backend: &'static str,
    pub(crate) name: &'static str,
}

const CLAUDE_WORKER: SubscriptionWorker = SubscriptionWorker {
    socket: SOCKET,
    backend: "claude-cli",
    name: "Claude",
};

impl SubscriptionWorker {
    pub(crate) async fn chat(
        &self,
        model: String,
        req: &ChatRequest,
    ) -> Result<ChatReply, LlmError> {
        self.send(chat_worker_request(model, req), WORKER_TIMEOUT)
            .await
    }

    /// A research run: the bounded question and the fixed research prompt,
    /// nothing else.
    pub(crate) async fn research(
        &self,
        model: String,
        req: &ResearchRequest,
    ) -> Result<ChatReply, LlmError> {
        self.send(research_worker_request(model, req), RESEARCH_WORKER_TIMEOUT)
            .await
    }

    async fn send(
        &self,
        request: ClaudeWorkerRequest,
        timeout: Duration,
    ) -> Result<ChatReply, LlmError> {
        if !request.valid() {
            return Err(LlmError::NotConfigured(format!(
                "{} worker request exceeds the bounded context policy",
                self.name
            )));
        }
        let model = request.model.clone();
        let payload = serde_json::to_vec(&request).map_err(|_| self.failure())?;
        if payload.len() + 1 > request.max_line_bytes() {
            return Err(self.failure());
        }
        let reply = tokio::time::timeout(timeout, async {
            let mut socket = UnixStream::connect(self.socket)
                .await
                .map_err(|_| self.failure())?;
            socket
                .write_all(&payload)
                .await
                .map_err(|_| self.failure())?;
            socket.write_all(b"\n").await.map_err(|_| self.failure())?;
            socket.shutdown().await.map_err(|_| self.failure())?;
            let mut response = Vec::new();
            socket
                .take((MAX_REPLY_BYTES + 1) as u64)
                .read_to_end(&mut response)
                .await
                .map_err(|_| self.failure())?;
            if response.len() > MAX_REPLY_BYTES {
                return Err(self.failure());
            }
            serde_json::from_slice::<ClaudeWorkerReply>(&response).map_err(|_| self.failure())
        })
        .await
        .map_err(|_| LlmError::Api {
            status: 504,
            body: format!("{} worker timed out", self.name),
        })??;
        self.reply_to_chat(reply, model)
    }

    fn failure(&self) -> LlmError {
        LlmError::Api {
            status: 503,
            body: format!("{} subscription worker unavailable", self.name),
        }
    }

    fn reply_to_chat(
        &self,
        reply: ClaudeWorkerReply,
        model: String,
    ) -> Result<ChatReply, LlmError> {
        if reply.protocol != 1 {
            return Err(self.failure());
        }
        match reply.state {
            ClaudeWorkerState::SubscriptionUnavailable => Err(LlmError::Api {
                status: 503,
                body: format!("{} subscription unavailable", self.name),
            }),
            ClaudeWorkerState::PlanLimit => Err(LlmError::Api {
                status: 429,
                body: format!("{} subscription plan limit reached", self.name),
            }),
            ClaudeWorkerState::IncompatibleRuntime => Err(LlmError::Api {
                status: 503,
                body: format!("{} subscription runtime is incompatible", self.name),
            }),
            // Bounded and final for this model: never a reason to try a paid
            // API; the router only moves on to the owner's next chain entry.
            ClaudeWorkerState::ModelUnavailable => Err(LlmError::NotConfigured(format!(
                "{} subscription model {model} is unavailable for this account",
                self.name
            ))),
            ClaudeWorkerState::RuntimeFailure => Err(self.failure()),
            ClaudeWorkerState::ToolUseRefused => Err(LlmError::Api {
                status: 502,
                body: format!("{} run attempted tool use and was refused", self.name),
            }),
            ClaudeWorkerState::Completed => {
                let text = reply
                    .text
                    .filter(|text| !text.trim().is_empty())
                    .ok_or(LlmError::Empty)?;
                Ok(ChatReply {
                    text,
                    model,
                    backend: Some(self.backend.into()),
                    requested_route: None,
                    actual_provider: None,
                    fallback_count: 0,
                    stop_reason: Some("end_turn".into()),
                    usage: Some(Usage {
                        input_tokens: reply.input_tokens.unwrap_or(0),
                        output_tokens: reply.output_tokens.unwrap_or(0),
                        cache_read_tokens: reply.cache_read_tokens.unwrap_or(0),
                        cache_write_tokens: reply.cache_write_tokens.unwrap_or(0),
                    }),
                })
            }
        }
    }
}

/// An ordinary chat run. The turns already say `[photo N]`; the photos go
/// along oldest first, so the Nth photo is `[photo N]`.
fn chat_worker_request(model: String, req: &ChatRequest) -> ClaudeWorkerRequest {
    ClaudeWorkerRequest {
        protocol: 1,
        model,
        system: req.system.clone(),
        prompt: build_bounded_prompt(&req.messages, system_excess(req.system.as_deref())),
        research: false,
        task: None,
        images: req
            .messages
            .iter()
            .flat_map(|message| &message.images)
            .map(|image| WorkerImage::jpeg(image.bytes()))
            .collect(),
    }
}

/// How far a system text runs past [`MAX_SYSTEM_BYTES`] on the wire.
fn system_excess(system: Option<&str>) -> usize {
    system.map_or(0, |system| {
        wire_len(system).saturating_sub(MAX_SYSTEM_BYTES)
    })
}

fn research_worker_request(model: String, req: &ResearchRequest) -> ClaudeWorkerRequest {
    ClaudeWorkerRequest {
        protocol: 1,
        model,
        system: Some(RESEARCH_SYSTEM_PROMPT.to_owned()),
        prompt: req.question().to_owned(),
        research: true,
        task: None,
        images: Vec::new(),
    }
}

/// Most prompt bytes of a chat run, counted as escaped on the JSON wire: under
/// the worker's chat prompt limit, and with a [`MAX_SYSTEM_BYTES`] system text
/// still inside one request line. A system text longer than that on the wire
/// takes its excess from the earlier turns' share.
const MAX_PROMPT_BYTES: usize = 44 * 1024;
/// Earlier turns always keep this much of the prompt: only past it is the
/// newest turn (a long message or a page turn) cut.
const MIN_HISTORY_BYTES: usize = 12 * 1024;
/// Most turns in a prompt, the same as Core's context loader sends: the byte
/// budget is the real limit.
const MAX_PROMPT_MESSAGES: usize = 32;
/// Room for the oldest omitted-turns note and the closing cue.
const FRAME_BYTES: usize = 64;
/// Room for one omitted-turns note between kept turns.
const NOTE_BYTES: usize = 40;
/// A plain earlier turn is cut to no less than this, or left out.
const MIN_CUT_BYTES: usize = 512;
/// The longest newest turn (in [`wire_len`] bytes) that is never cut, however
/// long the earlier turns are.
pub const MAX_WHOLE_TURN_BYTES: usize =
    MAX_PROMPT_BYTES - FRAME_BYTES - MIN_HISTORY_BYTES - "Jarvis: ".len() - 2;
const _: () = assert!(
    MAX_PROMPT_BYTES <= MAX_CHAT_PROMPT_BYTES
        && MAX_PROMPT_BYTES + MAX_SYSTEM_BYTES + 4 * 1024 <= MAX_REQUEST_BYTES
);

/// A character's length in a JSON string, as serde_json writes it.
fn wire_char_len(c: char) -> usize {
    match c {
        '"' | '\\' | '\u{8}' | '\u{c}' | '\n' | '\r' | '\t' => 2,
        '\0'..='\u{1f}' => 6,
        c => c.len_utf8(),
    }
}

/// The text's length inside a JSON string on the worker wire.
pub fn wire_len(text: &str) -> usize {
    text.chars().map(wire_char_len).sum()
}

/// The longest start of `text` whose [`wire_len`] is at most `max`.
pub fn wire_prefix(text: &str, max: usize) -> &str {
    let mut used = 0;
    for (index, c) in text.char_indices() {
        used += wire_char_len(c);
        if used > max {
            return &text[..index];
        }
    }
    text
}

/// The longest end of `text` whose [`wire_len`] is at most `max`.
fn wire_suffix(text: &str, max: usize) -> &str {
    let mut used = 0;
    for (index, c) in text.char_indices().rev() {
        used += wire_char_len(c);
        if used > max {
            return &text[index + c.len_utf8()..];
        }
    }
    text
}

/// A turn with web text in a `<web_result>` fence, or one that names the
/// tag: never cut, so the fence keeps its "untrusted" opening.
fn fenced(message: &ChatMessage) -> bool {
    message.content.contains("<web_result>")
}

/// The newest turns within [`MAX_PROMPT_BYTES`]. The newest turn keeps its
/// end and leaves [`MIN_HISTORY_BYTES`] (less `system_excess`) to earlier
/// turns. Walking back from the newest, an earlier turn that fits is kept
/// whole. One that does not fit is, when fenced, left out and the walk goes
/// on; when plain, cut to its end in half the room left, so older turns keep
/// the other half. The walk stops when that half is under [`MIN_CUT_BYTES`].
/// Each run of left-out turns becomes one note in its place:
/// `[N earlier message(s) not shown]`.
fn build_bounded_prompt(messages: &[ChatMessage], system_excess: usize) -> String {
    let label = |role| match role {
        Role::User => "User: ",
        Role::Assistant => "Jarvis: ",
    };
    // Label, text and the escaped newline.
    let cost = |role, text: &str| label(role).len() + wire_len(text) + 2;
    let Some((newest, earlier)) = messages.split_last() else {
        return "Jarvis:".into();
    };
    let share = MIN_HISTORY_BYTES - system_excess.min(MIN_HISTORY_BYTES);
    let considered = earlier.len().min(MAX_PROMPT_MESSAGES - 1);
    let history: usize = earlier[earlier.len() - considered..]
        .iter()
        .map(|message| cost(message.role, &message.content))
        .sum();
    let mut room = MAX_WHOLE_TURN_BYTES + "Jarvis: ".len() + 2 + share;
    let newest_text = wire_suffix(
        &newest.content,
        room - history.min(share) - label(newest.role).len() - 2,
    );
    room -= cost(newest.role, newest_text);

    // The earlier turns' text, newest first; `None` where one is left out.
    let mut kept: Vec<Option<&str>> = Vec::new();
    for message in earlier.iter().rev().take(considered) {
        let text = if cost(message.role, &message.content) <= room {
            Some(message.content.as_str())
        } else if fenced(message) {
            if room < NOTE_BYTES {
                break;
            }
            None
        } else {
            let half = (room / 2).saturating_sub(label(message.role).len() + 2);
            if half < MIN_CUT_BYTES {
                break;
            }
            Some(wire_suffix(&message.content, half))
        };
        room -= text.map_or(NOTE_BYTES, |text| cost(message.role, text));
        kept.push(text);
    }

    let mut prompt = String::new();
    let mut gap = earlier.len() - kept.len();
    let shown = earlier[gap..].iter().zip(kept.into_iter().rev());
    for (message, text) in shown {
        let Some(text) = text else {
            gap += 1;
            continue;
        };
        push_note(&mut prompt, &mut gap);
        prompt.push_str(label(message.role));
        prompt.push_str(text);
        prompt.push('\n');
    }
    push_note(&mut prompt, &mut gap);
    prompt.push_str(label(newest.role));
    prompt.push_str(newest_text);
    prompt.push_str("\nJarvis:");
    prompt
}

/// The note for `gap` left-out turns, if any, and a new count.
fn push_note(prompt: &mut String, gap: &mut usize) {
    if *gap > 0 {
        let plural = if *gap == 1 { "" } else { "s" };
        prompt.push_str(&format!("[{gap} earlier message{plural} not shown]\n"));
        *gap = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_worker_protocol::MAX_IMAGE_BYTES;

    fn turns(count: usize, text: &str) -> Vec<ChatMessage> {
        (0..count)
            .map(|number| {
                if number % 2 == 0 {
                    ChatMessage::user(format!("{text} {number}"))
                } else {
                    ChatMessage::assistant(format!("{text} {number}"))
                }
            })
            .collect()
    }

    #[test]
    fn wire_len_matches_serde_json() {
        for c in (0..=0x7f_u8).map(char::from).chain(['é', '€', '😀']) {
            let json = serde_json::to_string(&c.to_string()).unwrap();
            assert_eq!(wire_len(&c.to_string()), json.len() - 2, "{c:?}");
        }
        assert_eq!(wire_prefix("a\"é\n", 3), "a\"");
        assert_eq!(wire_prefix("a\"é\n", 4), "a\"");
        assert_eq!(wire_prefix("a\"é\n", 5), "a\"é");
        assert_eq!(wire_suffix("a\"é\n", 4), "é\n");
    }

    #[test]
    fn twenty_short_turns_are_all_kept() {
        let mut messages = turns(20, "turn");
        messages.push(ChatMessage::user("newest"));
        let prompt = build_bounded_prompt(&messages, 0);
        assert!(
            prompt.starts_with("User: turn 0\nJarvis: turn 1\n"),
            "{prompt}"
        );
        assert!(prompt.ends_with("Jarvis: turn 19\nUser: newest\nJarvis:"));
        assert!(!prompt.contains("not shown"));
    }

    #[test]
    fn dropped_turns_are_noted_and_the_message_cap_holds() {
        let mut messages = turns(40, "turn");
        messages.push(ChatMessage::user("newest"));
        let prompt = build_bounded_prompt(&messages, 0);
        assert!(prompt.starts_with("[9 earlier messages not shown]\nJarvis: turn 9\n"));
        assert_eq!(prompt.lines().count(), 1 + MAX_PROMPT_MESSAGES + 1);
        // Two fenced 30 KB rows: the older one does not fit and is left out.
        let fenced = ChatMessage::assistant(format!("<web_result>{}", "y".repeat(30_000)));
        let messages = [fenced.clone(), fenced, ChatMessage::user("newest")];
        assert!(build_bounded_prompt(&messages, 0).starts_with("[1 earlier message not shown]\n"));
    }

    #[test]
    fn empty_and_single_turn_prompts() {
        assert_eq!(build_bounded_prompt(&[], 0), "Jarvis:");
        assert_eq!(
            build_bounded_prompt(&[ChatMessage::user("hi")], 0),
            "User: hi\nJarvis:"
        );
    }

    #[test]
    fn a_long_plain_reply_is_cut_and_older_turns_stay() {
        let mut messages = turns(6, "old");
        messages.push(ChatMessage::assistant(format!(
            "{}REPLY-END",
            "r".repeat(20_000)
        )));
        messages.extend(turns(2, "recent"));
        messages.push(ChatMessage::user(format!("{}END", "x".repeat(100_000))));
        let prompt = build_bounded_prompt(&messages, 0);
        assert!(
            prompt.starts_with("User: old 0\nJarvis: old 1\n"),
            "{}",
            &prompt[..80]
        );
        assert!(prompt.contains("old 5\nJarvis: rrr"));
        assert!(prompt.contains("rREPLY-END\nUser: recent 0\nJarvis: recent 1\nUser: xxx"));
        assert!(!prompt.contains("not shown"));
        assert!(wire_len(&prompt) <= MAX_PROMPT_BYTES);
    }

    #[test]
    fn an_oversized_fenced_row_is_left_out_in_place_and_the_walk_goes_on() {
        let mut messages = turns(4, "old");
        messages.push(ChatMessage::assistant(format!(
            "Web search result: untrusted\n<web_result>\n{}\n</web_result>",
            "w".repeat(20_000)
        )));
        messages.extend(turns(2, "recent"));
        messages.push(ChatMessage::user("x".repeat(100_000)));
        let prompt = build_bounded_prompt(&messages, 0);
        assert!(prompt.starts_with("User: old 0\n"), "{}", &prompt[..80]);
        assert!(prompt.contains(
            "Jarvis: old 3\n[1 earlier message not shown]\nUser: recent 0\nJarvis: recent 1\n"
        ));
        assert!(!prompt.contains("<web_result>") && !prompt.contains("www"));
    }

    #[test]
    fn an_escape_heavy_system_text_takes_room_from_history_not_the_line() {
        let mut messages = turns(31, &"\"q\"\n".repeat(400));
        let page = "é\n".repeat(MAX_WHOLE_TURN_BYTES);
        let page = wire_prefix(&page, MAX_WHOLE_TURN_BYTES);
        messages.push(ChatMessage::user(page));
        let request = ChatRequest {
            // 12 KiB that doubles on the wire.
            system: Some("\"\n".repeat(MAX_SYSTEM_BYTES / 2)),
            messages,
            tier: Tier::Default,
            mode: crate::RoutingMode::Auto,
            max_tokens: 16,
            model: None,
            provider: None,
        };
        let worker = chat_worker_request("claude-sonnet-5".into(), &request);
        assert!(worker.valid());
        let line = serde_json::to_vec(&worker).unwrap().len() + 1;
        assert!(line <= MAX_REQUEST_BYTES, "{line}");
        // The newest turn still stays whole.
        assert!(worker.prompt.ends_with(&format!("User: {page}\nJarvis:")));
    }

    #[test]
    fn photos_keep_their_numbers_when_their_turn_is_left_out() {
        let photo = |marker: u8| {
            crate::ImageData::jpeg(vec![0xFF, 0xD8, 0xFF, 0xE0, marker, 0xFF, 0xD9]).unwrap()
        };
        let mut messages = vec![ChatMessage {
            images: vec![photo(1)],
            ..ChatMessage::user("[photo 1]\nold photo")
        }];
        messages.extend(turns(40, "turn"));
        messages.push(ChatMessage {
            images: vec![photo(2)],
            ..ChatMessage::user("[photo 2]\nnew photo")
        });
        let request = ChatRequest {
            system: None,
            messages,
            tier: Tier::Default,
            mode: crate::RoutingMode::Auto,
            max_tokens: 16,
            model: None,
            provider: None,
        };
        let worker = chat_worker_request("claude-sonnet-5".into(), &request);
        assert!(!worker.prompt.contains("[photo 1]"));
        assert!(worker
            .prompt
            .ends_with("User: [photo 2]\nnew photo\nJarvis:"));
        // The worker labels the Nth image `[photo N]`: Core's numbers, which
        // `[[photo:N]]` resolves, so every photo goes along in order.
        let decoded: Vec<_> = worker.images.iter().map(|i| i.decoded().unwrap()).collect();
        assert_eq!(decoded, [1, 2].map(|marker| photo(marker).bytes().to_vec()));
    }

    #[test]
    fn a_huge_newest_turn_leaves_room_for_history() {
        let mut messages = turns(200, &"h".repeat(500));
        messages.push(ChatMessage::user(format!("{}END", "x".repeat(100_000))));
        let prompt = build_bounded_prompt(&messages, 0);
        assert!(prompt.starts_with('['), "{}", &prompt[..80]);
        assert!(prompt.contains(" earlier messages not shown]\n"));
        assert!(prompt.ends_with("xxEND\nJarvis:"));
        let newest = prompt.rfind("User: x").unwrap();
        let history = &prompt[prompt.find('\n').unwrap() + 1..newest];
        assert!(
            wire_len(history) >= MIN_HISTORY_BYTES - 600,
            "{}",
            history.len()
        );
        assert!(wire_len(&prompt) <= MAX_PROMPT_BYTES);
        // Turns that fit are whole; the next is not cut to a scrap.
        assert!(history
            .lines()
            .all(|line| line.starts_with("User: h") || line.starts_with("Jarvis: h")));
    }

    #[test]
    fn multibyte_and_escaped_text_never_exceeds_the_byte_budget() {
        for text in ["é€😀", "\"\\\n", "\u{1}"] {
            let mut messages = turns(64, &text.repeat(700));
            messages.push(ChatMessage::user(text.repeat(40_000)));
            let prompt = build_bounded_prompt(&messages, 0);
            assert!(wire_len(&prompt) <= MAX_PROMPT_BYTES, "{text:?}");
            let json = serde_json::to_string(&prompt).unwrap();
            assert!(json.len() - 2 <= MAX_PROMPT_BYTES);
        }
    }

    #[test]
    fn a_page_turn_is_kept_whole_beside_history() {
        let mut messages = turns(31, &"\"q\"\n".repeat(400));
        let page = format!("<web_result>{}?", "é\n".repeat(MAX_WHOLE_TURN_BYTES));
        let page = wire_prefix(&page, MAX_WHOLE_TURN_BYTES);
        messages.push(ChatMessage::user(page));
        let prompt = build_bounded_prompt(&messages, 0);
        assert!(prompt.contains(&format!("User: {page}\nJarvis:")));
        let newest = prompt.rfind("User: <web_result>").unwrap();
        assert!(wire_len(&prompt[..newest]) >= MIN_HISTORY_BYTES - 1700);
        assert!(prompt.contains(" earlier messages not shown]\n"));
    }

    #[test]
    fn the_worst_case_chat_request_is_valid_and_fits_one_line() {
        let mut messages = turns(31, &"\"\\\n€".repeat(2_000));
        messages.push(ChatMessage::user("😀\"".repeat(30_000)));
        let request = ChatRequest {
            // Persona-like text: a newline and two quotes every 35 bytes.
            system: Some("A \"line\" of persona text, roughly.\n".repeat(MAX_SYSTEM_BYTES / 35)),
            messages,
            tier: Tier::Default,
            mode: crate::RoutingMode::Auto,
            max_tokens: 16,
            model: None,
            provider: None,
        };
        let worker = chat_worker_request("claude-sonnet-5".into(), &request);
        assert!(worker.valid());
        let line = serde_json::to_vec(&worker).unwrap().len() + 1;
        assert!(line <= worker.max_line_bytes(), "{line}");
    }

    #[test]
    fn research_request_carries_only_the_question_and_the_fixed_prompt() {
        let request = research_worker_request(
            "claude-opus-5".into(),
            &ResearchRequest::new("Wat is de laatste Rust-release?").unwrap(),
        );
        assert!(request.research);
        assert!(request.valid());
        assert_eq!(request.prompt, "Wat is de laatste Rust-release?");
        assert_eq!(request.system.as_deref(), Some(RESEARCH_SYSTEM_PROMPT));
        let wire = serde_json::to_value(&request).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({
                "protocol": 1,
                "model": "claude-opus-5",
                "system": RESEARCH_SYSTEM_PROMPT,
                "prompt": "Wat is de laatste Rust-release?",
                "research": true,
            })
        );
    }

    #[test]
    fn chat_requests_carry_the_photos_oldest_first_and_keep_their_text_shape() {
        let photo = |marker: u8| {
            crate::ImageData::jpeg(vec![0xFF, 0xD8, 0xFF, 0xE0, marker, 0xFF, 0xD9]).unwrap()
        };
        let with = |text: &str, images: Vec<crate::ImageData>| ChatMessage {
            images,
            ..ChatMessage::user(text)
        };
        let request = ChatRequest {
            system: Some("You are Jarvis.".into()),
            messages: vec![
                with("[photo 1]\nWhat is this?", vec![photo(1)]),
                ChatMessage::assistant("A cat."),
                with("[photo 2] [photo 3]\nAnd these?", vec![photo(2), photo(3)]),
            ],
            tier: Tier::Default,
            mode: crate::RoutingMode::Auto,
            max_tokens: 16,
            model: None,
            provider: None,
        };
        let worker = chat_worker_request("claude-sonnet-5".into(), &request);
        assert!(worker.valid());
        let decoded: Vec<_> = worker.images.iter().map(|i| i.decoded().unwrap()).collect();
        assert_eq!(
            decoded,
            [1, 2, 3].map(|marker| photo(marker).bytes().to_vec())
        );
        assert_eq!(worker.prompt, build_bounded_prompt(&request.messages, 0));

        // Without photos the wire shape is exactly the old one.
        let text = ChatRequest {
            messages: vec![ChatMessage::user("hi")],
            ..request
        };
        assert_eq!(
            serde_json::to_value(chat_worker_request("m".into(), &text)).unwrap(),
            serde_json::json!({
                "protocol": 1,
                "model": "m",
                "system": "You are Jarvis.",
                "prompt": "User: hi\nJarvis:",
            })
        );
    }

    #[tokio::test]
    async fn photo_requests_reach_the_worker_and_text_requests_keep_the_small_bound() {
        let reply = ClaudeWorkerReply {
            text: Some("A cat.".into()),
            ..ClaudeWorkerReply::failure(ClaudeWorkerState::Completed)
        };
        let (worker, server) = fake_worker(reply).await;
        // Four photos near the 1 MiB cap: far over the text bound.
        let mut big = vec![0xFF, 0xD8, 0xFF, 0xE0];
        big.resize(MAX_IMAGE_BYTES - 2, 0x42);
        big.extend([0xFF, 0xD9]);
        let image = crate::ImageData::jpeg(big).unwrap();
        let request = ChatRequest {
            system: None,
            messages: vec![ChatMessage {
                images: vec![image; 4],
                ..ChatMessage::user("[photo 1] [photo 2] [photo 3] [photo 4]")
            }],
            tier: Tier::Default,
            mode: crate::RoutingMode::Auto,
            max_tokens: 16,
            model: None,
            provider: None,
        };
        let answer = worker.chat("claude-sonnet-5".into(), &request).await;
        assert_eq!(answer.unwrap().text, "A cat.");
        let (wire, again) = server.await.unwrap();
        assert_eq!(wire["images"].as_array().map(Vec::len), Some(4));
        assert!(!again);

        // A text request over 64 KiB never leaves Core, whatever its shape.
        let escaped = ClaudeWorkerRequest {
            protocol: 1,
            model: "m".into(),
            system: None,
            prompt: "\u{1}".repeat(40 * 1024),
            research: false,
            task: None,
            images: Vec::new(),
        };
        assert!(escaped.valid());
        let dir = std::env::temp_dir().join(format!("jarvis-bound-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("worker.sock");
        let _ = std::fs::remove_file(&path);
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let quiet = SubscriptionWorker {
            socket: path.to_str().unwrap().to_owned().leak(),
            backend: "claude-cli",
            name: "Claude",
        };
        let error = quiet.send(escaped, WORKER_TIMEOUT).await;
        assert!(matches!(error, Err(LlmError::Api { status: 503, .. })));
        let connected = tokio::time::timeout(Duration::from_millis(100), listener.accept()).await;
        assert!(
            connected.is_err(),
            "an oversized text request reached the worker"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plan_limit_is_structured_and_non_secret() {
        let result = CLAUDE_WORKER.reply_to_chat(
            ClaudeWorkerReply::failure(ClaudeWorkerState::PlanLimit),
            "claude".into(),
        );
        assert!(matches!(result, Err(LlmError::Api { status: 429, .. })));
        let unavailable = CLAUDE_WORKER.reply_to_chat(
            ClaudeWorkerReply::failure(ClaudeWorkerState::ModelUnavailable),
            "claude".into(),
        );
        assert!(matches!(unavailable, Err(LlmError::NotConfigured(_))));
    }

    fn provider() -> ClaudeCliProvider {
        ClaudeCliProvider::new(
            "unused",
            "claude-sonnet-5",
            "claude-opus-5",
            "claude-haiku-5",
        )
    }

    #[test]
    fn decisions_use_the_default_model_and_reviews_the_hard_one() {
        let input = serde_json::json!({"candidates": {"c1": {"liq_usd": 1200.5}}});
        let decision = provider().task_request(WorkerTask::MemeDecision, &input);
        assert_eq!(decision.model, "claude-sonnet-5");
        let review = provider().task_request(WorkerTask::MemeStrategyReview, &input);
        assert_eq!(review.model, "claude-opus-5");
        assert!(decision.valid() && review.valid());
        let dependencies = provider().task_request(WorkerTask::DependencyReview, &input);
        assert_eq!(dependencies.model, "claude-opus-5");
        assert!(dependencies.valid());
        // A non-object input never leaves Core.
        let list = provider().task_request(WorkerTask::MemeDecision, &serde_json::json!([1]));
        assert!(!list.valid());
    }

    /// A fake worker on a private socket that answers exactly one request
    /// with `reply` and reports what it received plus whether a second
    /// connection followed. Never the real worker socket.
    async fn fake_worker(
        reply: ClaudeWorkerReply,
    ) -> (
        SubscriptionWorker,
        tokio::task::JoinHandle<(serde_json::Value, bool)>,
    ) {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "jarvis-task-test-{}-{}.sock",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let socket: &'static str = path.to_str().unwrap().to_owned().leak();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).await.unwrap();
            stream
                .write_all(&serde_json::to_vec(&reply).unwrap())
                .await
                .unwrap();
            drop(stream);
            let again = tokio::time::timeout(Duration::from_millis(200), listener.accept())
                .await
                .is_ok();
            let _ = std::fs::remove_file(&path);
            (serde_json::from_slice(&bytes).unwrap(), again)
        });
        let worker = SubscriptionWorker {
            socket,
            backend: "claude-cli",
            name: "Claude",
        };
        (worker, server)
    }

    #[tokio::test]
    async fn task_reaches_the_worker_once_with_its_exact_wire_shape() {
        let reply = ClaudeWorkerReply {
            text: Some(r#"{"actions":[{"action":"hold"}]}"#.into()),
            ..ClaudeWorkerReply::failure(ClaudeWorkerState::Completed)
        };
        let (worker, server) = fake_worker(reply).await;
        let input = serde_json::json!({"positions": {"p1": {"pnl_bps": -120}}});
        let request = provider().task_request(WorkerTask::MemeDecision, &input);
        let answer = worker.send(request, TASK_WORKER_TIMEOUT).await.unwrap();
        assert_eq!(answer.text, r#"{"actions":[{"action":"hold"}]}"#);
        assert_eq!(answer.backend.as_deref(), Some("claude-cli"));
        let (wire, again) = server.await.unwrap();
        assert_eq!(
            wire,
            serde_json::json!({
                "protocol": 1,
                "model": "claude-sonnet-5",
                "system": WorkerTask::MemeDecision.system_prompt(),
                "prompt": r#"{"positions":{"p1":{"pnl_bps":-120}}}"#,
                "task": "meme_decision",
            })
        );
        assert!(!again);
    }

    #[tokio::test]
    async fn busy_and_plan_limit_are_errors_without_retry_or_fallback() {
        // A busy background slot is refused by the worker as a runtime failure.
        for (state, status) in [
            (ClaudeWorkerState::RuntimeFailure, 503),
            (ClaudeWorkerState::PlanLimit, 429),
        ] {
            let (worker, server) = fake_worker(ClaudeWorkerReply::failure(state)).await;
            let request =
                provider().task_request(WorkerTask::MemeStrategyReview, &serde_json::json!({}));
            let error = worker.send(request, TASK_WORKER_TIMEOUT).await.unwrap_err();
            assert!(
                matches!(error, LlmError::Api { status: got, .. } if got == status),
                "{error:?}"
            );
            let (wire, again) = server.await.unwrap();
            assert_eq!(wire["task"], "meme_strategy_review");
            assert_eq!(wire["model"], "claude-opus-5");
            assert!(!again, "a failed task is never retried");
        }
    }
}
