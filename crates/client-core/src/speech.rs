//! Deterministic local speech preparation; no network, model or credentials.
use crate::realtime::Event;
use uuid::Uuid;

const MAX_RESPONSE: usize = 128 * 1024;
const PHRASE_CHARS: usize = 240;

#[derive(Debug, PartialEq, Eq)]
pub enum SpeechAction {
    Stop,
    Speak(String),
}

/// Native callers execute these actions with an offline engine. Preferences
/// default off. Text synchronization never depends on this gate or TTS success.
pub struct VoiceGate {
    device: Uuid,
    owner: Option<Uuid>,
    owner_run: Option<Uuid>,
    enabled: bool,
    buffer: SpeechBuffer,
}

impl VoiceGate {
    pub fn new(device: Uuid) -> Self {
        Self {
            device,
            owner: None,
            owner_run: None,
            enabled: false,
            buffer: SpeechBuffer::default(),
        }
    }
    pub fn set_enabled(&mut self, enabled: bool) -> SpeechAction {
        self.enabled = enabled;
        self.buffer.stop();
        SpeechAction::Stop
    }
    pub fn event(&mut self, event: &Event) -> Vec<SpeechAction> {
        match event {
            Event::VoiceOwnerChanged { device_id, run_id } => {
                self.owner = *device_id;
                self.owner_run = *run_id;
                self.buffer.stop();
                vec![SpeechAction::Stop]
            }
            Event::AssistantStarted(run)
                if self.enabled
                    && self.owner == Some(self.device)
                    && self.owner_run == Some(run.run_id) =>
            {
                self.buffer.start(run.run_id);
                vec![SpeechAction::Stop]
            }
            Event::AssistantDelta { run, text }
                if self.enabled
                    && self.owner == Some(self.device)
                    && self.owner_run == Some(run.run_id) =>
            {
                self.buffer
                    .delta(run.run_id, text)
                    .into_iter()
                    .map(SpeechAction::Speak)
                    .collect()
            }
            Event::AssistantCompleted { run, message }
                if self.enabled
                    && self.owner == Some(self.device)
                    && self.owner_run == Some(run.run_id) =>
            {
                if self.buffer.run == Some(run.run_id)
                    && (!message.content.starts_with(&self.buffer.received)
                        || message.content.len() > MAX_RESPONSE)
                {
                    self.buffer.stop();
                    return vec![SpeechAction::Stop];
                }
                self.buffer
                    .complete(run.run_id, &message.content)
                    .into_iter()
                    .map(SpeechAction::Speak)
                    .collect()
            }
            Event::AssistantFailed { run, .. } if self.buffer.run == Some(run.run_id) => {
                self.buffer.stop();
                vec![SpeechAction::Stop]
            }
            Event::ConnectionReady { .. } => {
                self.owner = None;
                self.owner_run = None;
                self.buffer.stop();
                vec![SpeechAction::Stop]
            }
            _ => vec![],
        }
    }
}

/// Per-run native speech state. The owner is checked for every event; callers
/// must stop the OS engine when this state is reset or ownership is lost.
#[derive(Default)]
pub struct SpeechBuffer {
    run: Option<Uuid>,
    received: String,
    pending: String,
    fence: Option<(u8, usize)>,
    line_start: bool,
}

impl SpeechBuffer {
    pub fn start(&mut self, run: Uuid) {
        *self = Self {
            run: Some(run),
            line_start: true,
            ..Self::default()
        };
    }
    pub fn stop(&mut self) {
        *self = Self::default();
    }

    pub fn delta(&mut self, run: Uuid, text: &str) -> Vec<String> {
        if self.run != Some(run) || self.received.len().saturating_add(text.len()) > MAX_RESPONSE {
            return vec![];
        }
        self.received.push_str(text);
        self.pending.push_str(text);
        self.flush(false)
    }

    pub fn complete(&mut self, run: Uuid, canonical: &str) -> Vec<String> {
        if self.run != Some(run) || canonical.len() > MAX_RESPONSE {
            return vec![];
        }
        // A final-only provider has no received prefix. Streamed providers
        // speak only the unsaid suffix, never the entire answer a second time.
        let Some(suffix) = canonical.strip_prefix(&self.received) else {
            self.stop();
            return vec![];
        };
        self.pending.push_str(suffix);
        let result = self.flush(true);
        self.stop();
        result
    }

    fn flush(&mut self, complete: bool) -> Vec<String> {
        let mut phrases = Vec::new();
        loop {
            if self.pending.is_empty() {
                break;
            }
            let line_end = self
                .pending
                .find('\n')
                .map(|i| i + 1)
                .or_else(|| complete.then_some(self.pending.len()));
            let line = &self.pending[..line_end.unwrap_or(self.pending.len())];
            let trimmed = line.trim_start_matches(' ');
            let indent = line.len() - trimmed.len();
            let marker = trimmed
                .as_bytes()
                .first()
                .copied()
                .filter(|c| matches!(c, b'`' | b'~'));
            let count = marker.map_or(0, |m| trimmed.bytes().take_while(|c| *c == m).count());
            let opening = self.line_start && indent <= 3 && count >= 3;
            if self.fence.is_some() || opening {
                // Fences are line-oriented: embedded backticks in code never
                // close a block. Partial marker/label lines wait for more data.
                let Some(end) = line_end else { break };
                if let Some((expected, minimum)) = self.fence {
                    if opening
                        && marker == Some(expected)
                        && count >= minimum
                        && trimmed[count..].trim().is_empty()
                    {
                        self.fence = None;
                    }
                } else if let Some(marker) = marker {
                    self.fence = Some((marker, count));
                }
                self.line_start = line.ends_with('\n');
                self.pending.drain(..end);
                continue;
            }
            let mut end = None;
            for (count, (index, ch)) in self.pending.char_indices().enumerate() {
                if ch == '\n'
                    || matches!(ch, '.' | '!' | '?')
                        && self.pending[index + ch.len_utf8()..].starts_with(char::is_whitespace)
                    || count >= PHRASE_CHARS && ch.is_whitespace()
                    || count >= PHRASE_CHARS * 2
                {
                    end = Some(index + ch.len_utf8());
                    break;
                }
            }
            let Some(end) = end
                .or_else(|| (complete && !self.pending.is_empty()).then_some(self.pending.len()))
            else {
                break;
            };
            let raw: String = self.pending.drain(..end).collect();
            self.line_start = raw.ends_with('\n');
            let phrase = sanitize_prose(&raw);
            if !phrase.is_empty() {
                phrases.push(phrase);
            }
        }
        phrases
    }
}

pub fn sanitize_prose(text: &str) -> String {
    text.chars()
        .filter(|c| {
            !matches!(c, '`' | '*' | '#' | '_' | '~') && (!c.is_control() || c.is_whitespace())
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::realtime::{CanonicalMessage, MessageRole, RunFailure, RunIdentity};

    fn identity(n: u128) -> RunIdentity {
        RunIdentity {
            run_id: Uuid::from_u128(n),
            request_id: Uuid::from_u128(n + 10),
            conversation_id: Uuid::from_u128(n + 20),
        }
    }
    fn completed(run: &RunIdentity, content: &str) -> Event {
        Event::AssistantCompleted {
            run: run.clone(),
            message: CanonicalMessage {
                id: Uuid::from_u128(100),
                conversation_id: run.conversation_id,
                role: MessageRole::Assistant,
                content: content.into(),
                model: None,
                created_at: time::OffsetDateTime::UNIX_EPOCH,
            },
        }
    }
    fn enabled_gate(run: &RunIdentity) -> VoiceGate {
        let mut gate = VoiceGate::new(Uuid::nil());
        gate.set_enabled(true);
        gate.event(&Event::VoiceOwnerChanged {
            device_id: Some(Uuid::nil()),
            run_id: Some(run.run_id),
        });
        gate.event(&Event::AssistantStarted(run.clone()));
        gate
    }

    #[test]
    fn unrelated_run_failure_cannot_interrupt_current_speech() {
        let run = identity(1);
        let mut gate = enabled_gate(&run);
        gate.event(&Event::AssistantDelta {
            run: run.clone(),
            text: "Current".into(),
        });
        assert!(gate
            .event(&Event::AssistantFailed {
                run: identity(2),
                reason: RunFailure::ProviderUnavailable
            })
            .is_empty());
        assert_eq!(
            gate.event(&completed(&run, "Current answer.")),
            vec![SpeechAction::Speak("Current answer.".into())]
        );
    }

    #[test]
    fn canonical_mismatch_stops_queued_provisional_speech() {
        let run = identity(1);
        let mut gate = enabled_gate(&run);
        gate.event(&Event::AssistantDelta {
            run: run.clone(),
            text: "Provisional. ".into(),
        });
        assert_eq!(
            gate.event(&completed(&run, "Different canonical answer.")),
            vec![SpeechAction::Stop]
        );
        assert!(gate
            .event(&completed(&run, "Different canonical answer."))
            .is_empty());
    }

    #[test]
    fn fragmented_indented_fences_and_embedded_markers_never_speak_code() {
        for text in [
            "Before.\n   ```rust\nlet s = \"```\";\nnot speech\n   ```\nAfter.",
            "Before.\n  ~~~~text\ncode\n~~~\nstill code\n  ~~~~\nAfter.",
            "Before.\n```\nunterminated code",
        ] {
            let mut streamed = SpeechBuffer::default();
            let mut final_only = SpeechBuffer::default();
            streamed.start(Uuid::nil());
            final_only.start(Uuid::nil());
            let mut phrases = Vec::new();
            for ch in text.chars() {
                phrases.extend(streamed.delta(Uuid::nil(), &ch.to_string()));
            }
            phrases.extend(streamed.complete(Uuid::nil(), text));
            assert_eq!(phrases, final_only.complete(Uuid::nil(), text));
            assert_eq!(phrases.first().map(String::as_str), Some("Before."));
            assert!(phrases.iter().all(|p| p == "Before." || p == "After."));
        }
    }
    #[test]
    fn streams_phrases_and_never_repeats_completed_response() {
        let run = Uuid::nil();
        let mut buffer = SpeechBuffer::default();
        buffer.start(run);
        assert!(buffer.delta(run, "Hello").is_empty());
        assert_eq!(buffer.delta(run, " world. Next"), vec!["Hello world."]);
        assert_eq!(
            buffer.complete(run, "Hello world. Next sentence."),
            vec!["Next sentence."]
        );
        assert!(buffer
            .complete(run, "Hello world. Next sentence.")
            .is_empty());
    }

    #[test]
    fn final_only_and_code_blocks_are_deterministic() {
        let run = Uuid::nil();
        let mut buffer = SpeechBuffer::default();
        buffer.start(run);
        assert_eq!(
            buffer.complete(
                run,
                "**Hello.**\n```rust\nsecret_code();\n```\nRead the code on screen."
            ),
            vec!["Hello.", "Read the code on screen."]
        );
    }

    #[test]
    fn stop_and_other_run_do_not_speak() {
        let mut buffer = SpeechBuffer::default();
        buffer.start(Uuid::nil());
        buffer.stop();
        assert!(buffer.delta(Uuid::nil(), "Hello. ").is_empty());
        assert!(buffer.complete(Uuid::nil(), "Hello.").is_empty());
    }
}
