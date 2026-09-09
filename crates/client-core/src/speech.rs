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
                self.buffer
                    .complete(run.run_id, &message.content)
                    .into_iter()
                    .map(SpeechAction::Speak)
                    .collect()
            }
            Event::AssistantFailed { .. } | Event::ConnectionReady { .. } => {
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
    fenced: bool,
}

impl SpeechBuffer {
    pub fn start(&mut self, run: Uuid) {
        *self = Self {
            run: Some(run),
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
            // Preserve fences even when a network chunk splits their markers.
            if self.pending.starts_with('`') && self.pending.len() < 3 && !complete {
                break;
            }
            if self.pending.starts_with("```") {
                // Consume the language label / fence line without speaking it.
                let end = self
                    .pending
                    .find('\n')
                    .map(|i| i + 1)
                    .or_else(|| complete.then_some(self.pending.len()));
                let Some(end) = end else { break };
                self.pending.drain(..end);
                self.fenced = !self.fenced;
                continue;
            }
            if self.fenced {
                if let Some(fence) = self.pending.find("```") {
                    self.pending.drain(..fence);
                    continue;
                }
                if complete {
                    self.pending.clear();
                }
                break;
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
