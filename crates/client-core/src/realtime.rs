//! Public presentation events. Never put provider responses, prompts, tool
//! arguments, credentials or internal reasoning into this contract.
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

pub const REALTIME_PROTOCOL: u16 = 1;
pub const MAX_EVENT_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealtimeCapability {
    pub protocol: u16,
    pub transport: String,
    pub reconciliation: String,
    pub asynchronous_chat: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub protocol: u16,
    /// Changes on Core restart. Sequence numbers are comparable only within
    /// this epoch. Reconnect always requires authoritative REST reconciliation.
    pub epoch: Uuid,
    pub event_id: Uuid,
    pub sequence: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    #[serde(flatten)]
    pub event: Event,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalMessage {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub role: MessageRole,
    pub content: String,
    pub model: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationMetadata {
    pub id: Uuid,
    pub title: String,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunIdentity {
    pub run_id: Uuid,
    pub request_id: Uuid,
    pub conversation_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum Event {
    #[serde(rename = "connection.ready")]
    ConnectionReady { device_id: Uuid, reconcile: bool },
    #[serde(rename = "conversation.created")]
    ConversationCreated(ConversationMetadata),
    #[serde(rename = "conversation.updated")]
    ConversationUpdated(ConversationMetadata),
    #[serde(rename = "conversation.deleted")]
    ConversationDeleted { conversation_id: Uuid },
    #[serde(rename = "message.created")]
    MessageCreated {
        request_id: Uuid,
        message: CanonicalMessage,
    },
    #[serde(rename = "assistant.started")]
    AssistantStarted(RunIdentity),
    #[serde(rename = "assistant.delta")]
    AssistantDelta { run: RunIdentity, text: String },
    #[serde(rename = "assistant.completed")]
    AssistantCompleted {
        run: RunIdentity,
        message: CanonicalMessage,
    },
    #[serde(rename = "assistant.failed")]
    AssistantFailed {
        run: RunIdentity,
        reason: RunFailure,
    },
    #[serde(rename = "voice.owner_changed")]
    VoiceOwnerChanged {
        device_id: Option<Uuid>,
        run_id: Option<Uuid>,
    },
    #[serde(rename = "voice.started")]
    VoiceStarted { run_id: Uuid, device_id: Uuid },
    #[serde(rename = "voice.stopped")]
    VoiceStopped { run_id: Uuid, device_id: Uuid },
    #[serde(rename = "voice.failed")]
    VoiceFailed { run_id: Uuid, device_id: Uuid },
}

/// Fixed safe failure codes, not raw provider errors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunFailure {
    ProviderUnavailable,
    PersistenceUnavailable,
    Interrupted,
}

/// Native clients keep a cursor; JS does not need session credentials. A new
/// socket starts with connection.ready and an authoritative reload, not replay.
#[derive(Default)]
pub struct EventCursor {
    epoch: Option<Uuid>,
    sequence: u64,
}

impl EventCursor {
    pub fn accept(&mut self, event: &EventEnvelope) -> bool {
        if event.protocol != REALTIME_PROTOCOL {
            return false;
        }
        if self.epoch != Some(event.epoch) {
            if !matches!(event.event, Event::ConnectionReady { .. }) {
                return false;
            }
            self.epoch = Some(event.epoch);
            self.sequence = event.sequence;
            return true;
        }
        if event.sequence <= self.sequence {
            return false;
        }
        self.sequence = event.sequence;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_contract_and_duplicate_suppression() {
        let mut event = EventEnvelope {
            protocol: REALTIME_PROTOCOL,
            epoch: Uuid::nil(),
            event_id: Uuid::nil(),
            sequence: 1,
            at: OffsetDateTime::UNIX_EPOCH,
            event: Event::ConnectionReady {
                device_id: Uuid::nil(),
                reconcile: true,
            },
        };
        let encoded = serde_json::to_value(&event).unwrap();
        assert_eq!(encoded["type"], "connection.ready");
        assert_eq!(
            serde_json::from_value::<EventEnvelope>(encoded).unwrap(),
            event
        );
        let mut cursor = EventCursor::default();
        assert!(cursor.accept(&event));
        assert!(!cursor.accept(&event));
        event.sequence = 2;
        assert!(cursor.accept(&event));
        event.sequence = 1;
        assert!(!cursor.accept(&event));
        event.protocol = 99;
        assert!(!cursor.accept(&event));
    }
}
