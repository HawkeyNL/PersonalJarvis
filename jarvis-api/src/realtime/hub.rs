use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use jarvis_client_core::realtime::{Event, EventEnvelope, MAX_EVENT_BYTES, REALTIME_PROTOCOL};
use std::time::{Duration, Instant};
use time::OffsetDateTime;
use tokio::sync::mpsc;
use uuid::Uuid;

const QUEUE_CAPACITY: usize = 64;
const MAX_SUBSCRIBERS: usize = 256;
const MAX_PER_USER: usize = 16;
const MAX_PER_DEVICE: usize = 2;

struct Sink {
    user: Uuid,
    device: Uuid,
    sender: mpsc::Sender<Arc<EventEnvelope>>,
}

struct Inner {
    epoch: Uuid,
    sequence: u64,
    sinks: HashMap<Uuid, Sink>,
    runs: HashSet<(Uuid, Uuid)>,
    voices: HashMap<Uuid, VoiceLease>,
}

struct VoiceLease {
    device: Uuid,
    run: Option<Uuid>,
    expires: Instant,
}

#[derive(Clone)]
pub struct Hub(Arc<Mutex<Inner>>);

impl Default for Hub {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(Inner {
            epoch: Uuid::now_v7(),
            sequence: 0,
            sinks: HashMap::new(),
            runs: HashSet::new(),
            voices: HashMap::new(),
        })))
    }
}

pub struct Subscription {
    hub: Hub,
    id: Uuid,
    pub receiver: mpsc::Receiver<Arc<EventEnvelope>>,
}

pub struct RunGuard {
    hub: Hub,
    key: (Uuid, Uuid),
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.hub.0.lock() {
            inner.runs.remove(&self.key);
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.hub.0.lock() {
            inner.sinks.remove(&self.id);
            tracing::debug!(subscribers = inner.sinks.len(), "realtime disconnected");
        }
    }
}

impl Inner {
    fn publish(&mut self, user: Uuid, event: Event) {
        let kind = match &event {
            Event::ConnectionReady { .. } => "connection.ready",
            Event::ConversationCreated(_) => "conversation.created",
            Event::ConversationUpdated(_) => "conversation.updated",
            Event::ConversationDeleted { .. } => "conversation.deleted",
            Event::MessageCreated { .. } => "message.created",
            Event::AssistantStarted(_) => "assistant.started",
            Event::AssistantDelta { .. } => "assistant.delta",
            Event::AssistantCompleted { .. } => "assistant.completed",
            Event::AssistantFailed { .. } => "assistant.failed",
            Event::VoiceOwnerChanged { .. } => "voice.owner_changed",
            Event::VoiceStarted { .. } => "voice.started",
            Event::VoiceStopped { .. } => "voice.stopped",
            Event::VoiceFailed { .. } => "voice.failed",
        };
        let Some(envelope) = self.envelope(event) else {
            return;
        };
        if serde_json::to_vec(envelope.as_ref()).map_or(true, |v| v.len() > MAX_EVENT_BYTES) {
            self.sinks.retain(|_, sink| sink.user != user);
            tracing::warn!("realtime event too large; reconciliation required");
            return;
        }
        self.sinks
            .retain(|_, sink| sink.user != user || sink.sender.try_send(envelope.clone()).is_ok());
        tracing::debug!(
            kind,
            subscribers = self.sinks.len(),
            "realtime event published"
        );
    }
    fn envelope(&mut self, event: Event) -> Option<Arc<EventEnvelope>> {
        // Never wrap a cursor; at exhaustion refuse events rather than causing
        // clients to accept old/duplicate sequence numbers.
        self.sequence = self.sequence.checked_add(1)?;
        Some(Arc::new(EventEnvelope {
            protocol: REALTIME_PROTOCOL,
            epoch: self.epoch,
            event_id: Uuid::now_v7(),
            sequence: self.sequence,
            at: OffsetDateTime::now_utc(),
            event,
        }))
    }
}

impl Hub {
    pub fn claim_voice(&self, user: Uuid, device: Uuid, run: Option<Uuid>) -> bool {
        let Ok(mut inner) = self.0.lock() else {
            return false;
        };
        let expired: Vec<_> = inner
            .voices
            .iter()
            .filter(|(_, lease)| lease.expires <= Instant::now())
            .map(|(owner, _)| *owner)
            .collect();
        for owner in expired {
            inner.voices.remove(&owner);
            inner.publish(
                owner,
                Event::VoiceOwnerChanged {
                    device_id: None,
                    run_id: None,
                },
            );
        }
        if inner.voices.len() >= MAX_SUBSCRIBERS && !inner.voices.contains_key(&user) {
            return false;
        }
        inner.voices.insert(
            user,
            VoiceLease {
                device,
                run,
                expires: Instant::now() + Duration::from_secs(90),
            },
        );
        inner.publish(
            user,
            Event::VoiceOwnerChanged {
                device_id: Some(device),
                run_id: run,
            },
        );
        true
    }

    pub fn voice_owner(&self, user: Uuid) -> Option<(Uuid, Option<Uuid>)> {
        let mut inner = self.0.lock().ok()?;
        let lease = inner.voices.get(&user)?;
        if lease.expires > Instant::now() {
            return Some((lease.device, lease.run));
        }
        inner.voices.remove(&user);
        inner.publish(
            user,
            Event::VoiceOwnerChanged {
                device_id: None,
                run_id: None,
            },
        );
        None
    }

    pub fn renew_voice(&self, user: Uuid, device: Uuid) {
        if let Ok(mut inner) = self.0.lock() {
            if let Some(lease) = inner.voices.get_mut(&user) {
                if lease.device == device && lease.expires > Instant::now() {
                    lease.expires = Instant::now() + Duration::from_secs(90);
                }
            }
        }
    }

    pub fn release_voice(&self, user: Uuid, device: Uuid) -> bool {
        let Ok(mut inner) = self.0.lock() else {
            return false;
        };
        if !inner.voices.get(&user).is_some_and(|v| v.device == device) {
            return false;
        }
        inner.voices.remove(&user);
        inner.publish(
            user,
            Event::VoiceOwnerChanged {
                device_id: None,
                run_id: None,
            },
        );
        true
    }

    pub fn epoch(&self) -> Option<Uuid> {
        self.0.lock().ok().map(|i| i.epoch)
    }

    pub fn run_active(&self, user: Uuid, conversation: Uuid) -> bool {
        self.0
            .lock()
            .is_ok_and(|inner| inner.runs.contains(&(user, conversation)))
    }

    pub fn reserve_run(&self, user: Uuid, conversation: Uuid) -> Option<RunGuard> {
        let mut inner = self.0.lock().ok()?;
        let key = (user, conversation);
        if inner.runs.len() >= 16
            || inner.runs.iter().filter(|(u, _)| *u == user).count() >= 4
            || !inner.runs.insert(key)
        {
            return None;
        }
        Some(RunGuard {
            hub: self.clone(),
            key,
        })
    }
    /// Identity must come from Authed, never a request payload. The initial
    /// ready event is queued under the same lock as subscription registration,
    /// so events cannot race ahead of it.
    pub fn subscribe(&self, user: Uuid, device: Uuid) -> Option<Subscription> {
        let mut inner = self.0.lock().ok()?;
        if inner.sinks.len() >= MAX_SUBSCRIBERS
            || inner.sinks.values().filter(|s| s.user == user).count() >= MAX_PER_USER
            || inner.sinks.values().filter(|s| s.device == device).count() >= MAX_PER_DEVICE
        {
            return None;
        }
        let (sender, receiver) = mpsc::channel(QUEUE_CAPACITY);
        let ready = inner.envelope(Event::ConnectionReady {
            device_id: device,
            reconcile: true,
        })?;
        sender.try_send(ready).ok()?;
        let id = Uuid::now_v7();
        inner.sinks.insert(
            id,
            Sink {
                user,
                device,
                sender,
            },
        );
        tracing::debug!(subscribers = inner.sinks.len(), "realtime connected");
        Some(Subscription {
            hub: self.clone(),
            id,
            receiver,
        })
    }

    /// Nonblocking and independent of persistence. Full queues lose their
    /// sender and disconnect after draining, forcing authoritative recovery.
    /// A single Arc payload is shared by all of this owner's devices.
    pub fn publish(&self, user: Uuid, event: Event) {
        let Ok(mut inner) = self.0.lock() else { return };
        inner.publish(user, event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event() -> Event {
        Event::ConversationDeleted {
            conversation_id: Uuid::nil(),
        }
    }

    #[tokio::test]
    async fn fanout_is_identical_and_owner_scoped() {
        let hub = Hub::default();
        let owner = Uuid::now_v7();
        let mut a = hub.subscribe(owner, Uuid::now_v7()).unwrap();
        let mut b = hub.subscribe(owner, Uuid::now_v7()).unwrap();
        let mut other = hub.subscribe(Uuid::now_v7(), Uuid::now_v7()).unwrap();
        a.receiver.recv().await.unwrap();
        b.receiver.recv().await.unwrap();
        other.receiver.recv().await.unwrap();
        hub.publish(owner, event());
        let first = a.receiver.recv().await.unwrap();
        assert_eq!(first, b.receiver.recv().await.unwrap());
        assert!(other.receiver.try_recv().is_err());
        hub.publish(owner, event());
        assert!(a.receiver.recv().await.unwrap().sequence > first.sequence);
    }

    #[test]
    fn slow_clients_are_bounded_and_do_not_block() {
        let hub = Hub::default();
        let owner = Uuid::now_v7();
        let mut slow = hub.subscribe(owner, Uuid::now_v7()).unwrap();
        for _ in 0..1000 {
            hub.publish(owner, event());
        }
        assert!(hub.0.lock().unwrap().sinks.is_empty());
        let mut count = 0;
        while slow.receiver.try_recv().is_ok() {
            count += 1;
        }
        assert_eq!(count, QUEUE_CAPACITY);
        assert!(slow.receiver.is_closed());
    }

    #[test]
    fn reconnects_cleanup_and_device_limit() {
        let hub = Hub::default();
        let owner = Uuid::now_v7();
        let device = Uuid::now_v7();
        for _ in 0..1000 {
            drop(hub.subscribe(owner, device).unwrap());
        }
        assert!(hub.0.lock().unwrap().sinks.is_empty());
        let _a = hub.subscribe(owner, device).unwrap();
        let _b = hub.subscribe(owner, device).unwrap();
        assert!(hub.subscribe(owner, device).is_none());
    }

    #[test]
    fn voice_lease_and_run_reservations_are_bounded_and_owner_scoped() {
        let hub = Hub::default();
        let owner = Uuid::now_v7();
        let device = Uuid::now_v7();
        let run = Uuid::now_v7();
        let guard = hub.reserve_run(owner, run).unwrap();
        assert!(hub.reserve_run(owner, run).is_none());
        drop(guard);
        assert!(hub.reserve_run(owner, run).is_some());
        assert!(hub.claim_voice(owner, device, Some(run)));
        assert!(!hub.release_voice(owner, Uuid::now_v7()));
        assert!(hub.voice_owner(Uuid::now_v7()).is_none());
        hub.0
            .lock()
            .unwrap()
            .voices
            .get_mut(&owner)
            .unwrap()
            .expires = Instant::now() - Duration::from_secs(1);
        assert!(hub.voice_owner(owner).is_none());
        assert!(hub.claim_voice(owner, Uuid::now_v7(), None));
    }
}
