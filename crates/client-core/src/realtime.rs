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

/// The owner's own latest question, as the app would send it to Research.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearchOffer {
    pub question: String,
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
    AssistantStarted {
        #[serde(flatten)]
        run: RunIdentity,
        /// True only for a run Core started by itself to search the web after
        /// an answer (automatic web search); it has no user message of its
        /// own. Absent otherwise and from older Cores.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        web_search: bool,
    },
    #[serde(rename = "assistant.delta")]
    AssistantDelta { run: RunIdentity, text: String },
    #[serde(rename = "assistant.completed")]
    AssistantCompleted {
        run: RunIdentity,
        message: CanonicalMessage,
        /// Advisory only: the app may offer a web search, and only the owner's
        /// click sends a new explicit Research run. Absent for older Cores.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        research_offer: Option<ResearchOffer>,
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
    /// A memecoin paper strategy changed its active version or enabled
    /// state. A hint to reload `GET /v1/trading/meme/strategies/{id}`.
    /// Core does not send it yet: apps released before they ignore unknown
    /// event types would fail on it.
    #[serde(rename = "trading.meme.strategy_changed")]
    MemeStrategyChanged {
        strategy_id: String,
        version: u32,
        enabled: bool,
    },
    /// The strategy agent recorded a paper decision; reload
    /// `GET /v1/trading/meme/decisions`. Not sent yet, like
    /// [`Event::MemeStrategyChanged`].
    #[serde(rename = "trading.meme.decision")]
    MemeDecision { id: String },
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

    fn envelope(epoch: u128, sequence: u64, ready: bool) -> EventEnvelope {
        EventEnvelope {
            protocol: REALTIME_PROTOCOL,
            epoch: Uuid::from_u128(epoch),
            event_id: Uuid::from_u128(sequence.into()),
            sequence,
            at: OffsetDateTime::UNIX_EPOCH,
            event: if ready {
                Event::ConnectionReady {
                    device_id: Uuid::from_u128(10),
                    reconcile: true,
                }
            } else {
                Event::ConversationDeleted {
                    conversation_id: Uuid::from_u128(20),
                }
            },
        }
    }

    #[test]
    fn restarted_core_requires_ready_before_accepting_reset_sequence() {
        let mut cursor = EventCursor::default();
        assert!(!cursor.accept(&envelope(1, 1, false)));
        assert!(cursor.accept(&envelope(1, 100, true)));
        assert!(cursor.accept(&envelope(1, 105, false)));
        assert!(!cursor.accept(&envelope(2, 1, false)));
        // A rejected epoch must not discard the previous valid cursor.
        assert!(!cursor.accept(&envelope(1, 104, false)));
        assert!(cursor.accept(&envelope(1, 106, false)));
        assert!(cursor.accept(&envelope(2, 1, true)));
        assert!(cursor.accept(&envelope(2, 2, false)));
        assert!(!cursor.accept(&envelope(2, 2, false)));
        assert!(!cursor.accept(&envelope(1, 200, false)));
        assert!(cursor.accept(&envelope(2, 3, false)));
    }

    #[test]
    fn incompatible_protocol_cannot_advance_or_reset_the_cursor() {
        let mut cursor = EventCursor::default();
        assert!(cursor.accept(&envelope(1, 5, true)));
        for (epoch, ready) in [(1, false), (2, true)] {
            let mut incompatible = envelope(epoch, 500, ready);
            incompatible.protocol = REALTIME_PROTOCOL + 1;
            assert!(!cursor.accept(&incompatible));
        }
        assert!(cursor.accept(&envelope(1, 6, false)));
        assert!(!cursor.accept(&envelope(1, 5, true)));
        assert!(cursor.accept(&envelope(1, 7, true)));
    }

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

    #[test]
    fn started_web_search_marker_is_optional_on_the_wire() {
        let id = Uuid::nil();
        let run = RunIdentity {
            run_id: id,
            request_id: id,
            conversation_id: id,
        };
        let ordinary = Event::AssistantStarted {
            run: run.clone(),
            web_search: false,
        };
        // An ordinary start keeps the old flat payload, so old payloads decode.
        let old = serde_json::to_value(&ordinary).unwrap();
        assert_eq!(old["type"], "assistant.started");
        assert_eq!(old["payload"], serde_json::to_value(&run).unwrap());
        assert_eq!(serde_json::from_value::<Event>(old).unwrap(), ordinary);
        let automatic = Event::AssistantStarted {
            run: run.clone(),
            web_search: true,
        };
        let new = serde_json::to_value(&automatic).unwrap();
        assert_eq!(new["payload"]["web_search"], true);
        assert_eq!(new["payload"]["run_id"], id.to_string());
        assert_eq!(
            serde_json::from_value::<Event>(new.clone()).unwrap(),
            automatic
        );
        // A client that only knows the run identity ignores the marker.
        assert_eq!(
            serde_json::from_value::<RunIdentity>(new["payload"].clone()).unwrap(),
            run
        );
    }

    #[test]
    fn memecoin_events_carry_only_ids_and_state() {
        let changed = Event::MemeStrategyChanged {
            strategy_id: "b-base-reclaim".into(),
            version: 1_000_002,
            enabled: false,
        };
        let wire = serde_json::to_value(&changed).unwrap();
        assert_eq!(wire["type"], "trading.meme.strategy_changed");
        assert_eq!(
            wire["payload"],
            serde_json::json!({"strategy_id": "b-base-reclaim", "version": 1_000_002, "enabled": false})
        );
        assert_eq!(serde_json::from_value::<Event>(wire).unwrap(), changed);
        let decision = Event::MemeDecision { id: "d1".into() };
        let wire = serde_json::to_value(&decision).unwrap();
        assert_eq!(wire["type"], "trading.meme.decision");
        assert_eq!(wire["payload"], serde_json::json!({"id": "d1"}));
    }

    #[test]
    fn research_offer_is_optional_on_the_wire() {
        let id = Uuid::nil();
        let mut completed = Event::AssistantCompleted {
            run: RunIdentity {
                run_id: id,
                request_id: id,
                conversation_id: id,
            },
            message: CanonicalMessage {
                id,
                conversation_id: id,
                role: MessageRole::Assistant,
                content: "hi".into(),
                model: None,
                created_at: OffsetDateTime::UNIX_EPOCH,
            },
            research_offer: None,
        };
        // Without an offer the payload keeps the old shape, so old payloads decode.
        let old = serde_json::to_value(&completed).unwrap();
        assert!(old["payload"].get("research_offer").is_none());
        assert_eq!(serde_json::from_value::<Event>(old).unwrap(), completed);

        if let Event::AssistantCompleted { research_offer, .. } = &mut completed {
            *research_offer = Some(ResearchOffer {
                question: "weather in Utrecht?".into(),
            });
        }
        let new = serde_json::to_value(&completed).unwrap();
        assert_eq!(
            new["payload"]["research_offer"]["question"],
            "weather in Utrecht?"
        );
        assert_eq!(serde_json::from_value::<Event>(new).unwrap(), completed);
    }
}
