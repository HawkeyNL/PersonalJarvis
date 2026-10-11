//! Crash-safe budget reservations.  The in-memory `BudgetBook` is the atomic
//! admission point; every admitted hold is mirrored in a durable row with a
//! TTL so a restart can re-adopt it and a lost caller can never pin budget.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use jarvis_usage::{BudgetBook, BudgetError, BudgetSnapshot};
use tokio::time::timeout;

/// Bound on reservation database calls on a request path (local database).
const REQUEST_DB_TIMEOUT: Duration = Duration::from_secs(1);
/// Bound on startup reconciliation and sweeper database calls.
const MAINTENANCE_DB_TIMEOUT: Duration = Duration::from_secs(5);
/// How often lapsed holds are expired in memory and in the database.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(60);
/// Startup refuses to run on a partial view of active reservations.
const MAX_ACTIVE_ON_START: usize = 10_000;

/// A durable hold that was still active and unexpired.
#[derive(Debug, Clone)]
pub struct DurableHold {
    pub id: String,
    pub cents: u64,
    pub remaining: Duration,
}

/// Durable side of the gate; production uses SurrealDB.
#[async_trait::async_trait]
pub trait ReservationStore: Send + Sync {
    async fn create(&self, id: &str, cents: u64, ttl: Duration) -> anyhow::Result<()>;
    /// Idempotent; settled and cancelled rows both become `released`.
    async fn release(&self, id: &str) -> anyhow::Result<()>;
    async fn expire_lapsed(&self) -> anyhow::Result<()>;
    async fn active(&self, limit: usize) -> anyhow::Result<Vec<DurableHold>>;
}

#[async_trait::async_trait]
impl ReservationStore for jarvis_store::Database {
    async fn create(&self, id: &str, cents: u64, ttl: Duration) -> anyhow::Result<()> {
        // Round up so the durable row never expires before the in-memory hold.
        let secs = ttl.as_secs() + u64::from(ttl.subsec_nanos() > 0);
        Ok(jarvis_usage::reserve_task(self, id, None, cents, secs).await?)
    }

    async fn release(&self, id: &str) -> anyhow::Result<()> {
        Ok(jarvis_usage::release_task(self, id).await?)
    }

    async fn expire_lapsed(&self) -> anyhow::Result<()> {
        Ok(jarvis_usage::expire_tasks(self).await?)
    }

    async fn active(&self, limit: usize) -> anyhow::Result<Vec<DurableHold>> {
        Ok(jarvis_usage::active_tasks(self, limit)
            .await?
            .into_iter()
            .map(|row| DurableHold {
                id: row.task_id,
                cents: row.projected_cents.max(0) as u64,
                // Whole seconds truncate; one extra second keeps it conservative.
                remaining: Duration::from_secs(row.remaining_secs.max(0) as u64 + 1),
            })
            .collect())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateError {
    Budget(BudgetError),
    /// The durable row could not be written; nothing was reserved.
    Store,
}

/// A row left `active` only over-counts until its TTL, so a failed release
/// is logged rather than surfaced to the caller.
async fn release_durable(store: &dyn ReservationStore, id: &str) {
    match timeout(REQUEST_DB_TIMEOUT, store.release(id)).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::warn!(%error, "durable budget release failed; row expires by TTL")
        }
        Err(_) => tracing::warn!("durable budget release timed out; row expires by TTL"),
    }
}

pub struct BudgetGate {
    book: BudgetBook,
    store: Arc<dyn ReservationStore>,
}

impl BudgetGate {
    pub fn new(book: BudgetBook, store: Arc<dyn ReservationStore>) -> Self {
        Self { book, store }
    }

    /// Admit under the book's mutex, then persist.  A failed or timed-out
    /// durable write rolls the hold back: no durable row, no reservation.
    pub async fn reserve(&self, id: &str, cents: u64, ttl: Duration) -> Result<(), GateError> {
        self.book
            .reserve(id, cents, Instant::now() + ttl)
            .map_err(GateError::Budget)?;
        match timeout(REQUEST_DB_TIMEOUT, self.store.create(id, cents, ttl)).await {
            Ok(Ok(())) => Ok(()),
            outcome => {
                self.book.cancel(id);
                let error = match outcome {
                    Ok(Err(error)) => error.to_string(),
                    _ => "timed out".to_owned(),
                };
                // A write that landed after the timeout stays `active` until
                // its TTL: conservative, never an overspend.
                tracing::warn!(%error, "durable budget reservation failed; refused");
                Err(GateError::Store)
            }
        }
    }

    pub async fn settle(&self, id: &str, actual_cents: u64) -> Result<(), GateError> {
        self.book
            .settle(id, actual_cents)
            .map_err(GateError::Budget)?;
        // Detached so the caller records usage without waiting on this write.
        let store = self.store.clone();
        let id = id.to_owned();
        tokio::spawn(async move { release_durable(store.as_ref(), &id).await });
        Ok(())
    }

    pub async fn cancel(&self, id: &str) -> bool {
        let held = self.book.cancel(id);
        release_durable(self.store.as_ref(), id).await;
        held
    }

    /// Startup: expire lapsed rows, then re-adopt every still-active hold.
    /// An error aborts startup rather than run on an undercounted book.
    pub async fn reconcile(&self) -> anyhow::Result<()> {
        timeout(MAINTENANCE_DB_TIMEOUT, self.store.expire_lapsed()).await??;
        let holds = timeout(
            MAINTENANCE_DB_TIMEOUT,
            self.store.active(MAX_ACTIVE_ON_START + 1),
        )
        .await??;
        if holds.len() > MAX_ACTIVE_ON_START {
            anyhow::bail!("more than {MAX_ACTIVE_ON_START} active budget reservations");
        }
        let now = Instant::now();
        for hold in &holds {
            self.book
                .restore(&hold.id, hold.cents, now + hold.remaining);
        }
        tracing::info!(restored = holds.len(), "budget reservations reconciled");
        Ok(())
    }

    /// Periodic TTL sweep of both the in-memory book and the durable rows.
    pub async fn sweep(&self) {
        let expired = self.book.expire(Instant::now());
        if expired > 0 {
            tracing::info!(expired, "expired stale budget holds");
        }
        match timeout(MAINTENANCE_DB_TIMEOUT, self.store.expire_lapsed()).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::warn!(%error, "durable budget expiry failed"),
            Err(_) => tracing::warn!("durable budget expiry timed out"),
        }
    }

    pub fn snapshot(&self) -> BudgetSnapshot {
        self.book.snapshot()
    }

    pub fn reconcile_actual(&self, actual_cents: u64) {
        self.book.reconcile_actual(actual_cents);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::{
            atomic::{AtomicBool, Ordering},
            Mutex,
        },
    };

    use jarvis_usage::BudgetLimits;

    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Status {
        Active,
        Released,
        Expired,
    }

    #[derive(Default)]
    struct FakeStore {
        rows: Mutex<BTreeMap<String, (u64, Status, Instant)>>,
        fail_create: AtomicBool,
    }

    impl FakeStore {
        fn status(&self, id: &str) -> Option<Status> {
            self.rows.lock().unwrap().get(id).map(|row| row.1)
        }
        fn insert(&self, id: &str, cents: u64, expires_at: Instant) {
            self.rows
                .lock()
                .unwrap()
                .insert(id.into(), (cents, Status::Active, expires_at));
        }
    }

    #[async_trait::async_trait]
    impl ReservationStore for FakeStore {
        async fn create(&self, id: &str, cents: u64, ttl: Duration) -> anyhow::Result<()> {
            // Interleave with other reservers between admission and persist.
            tokio::task::yield_now().await;
            if self.fail_create.load(Ordering::SeqCst) {
                anyhow::bail!("injected failure");
            }
            self.insert(id, cents, Instant::now() + ttl);
            Ok(())
        }
        async fn release(&self, id: &str) -> anyhow::Result<()> {
            if let Some(row) = self.rows.lock().unwrap().get_mut(id) {
                if row.1 == Status::Active {
                    row.1 = Status::Released;
                }
            }
            Ok(())
        }
        async fn expire_lapsed(&self) -> anyhow::Result<()> {
            let now = Instant::now();
            for row in self.rows.lock().unwrap().values_mut() {
                if row.1 == Status::Active && row.2 <= now {
                    row.1 = Status::Expired;
                }
            }
            Ok(())
        }
        async fn active(&self, limit: usize) -> anyhow::Result<Vec<DurableHold>> {
            let now = Instant::now();
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, row)| row.1 == Status::Active && row.2 > now)
                .take(limit)
                .map(|(id, row)| DurableHold {
                    id: id.clone(),
                    cents: row.0,
                    remaining: row.2 - now,
                })
                .collect())
        }
    }

    const TTL: Duration = Duration::from_secs(60);

    fn gate(hard: u64, store: Arc<FakeStore>) -> BudgetGate {
        BudgetGate::new(
            BudgetBook::new(
                BudgetLimits {
                    monthly_soft_cents: hard,
                    monthly_hard_cents: hard,
                    per_request_hard_cents: 100,
                },
                0,
            ),
            store,
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn parallel_reservations_never_exceed_the_hard_cap() {
        let store = Arc::new(FakeStore::default());
        let gate = Arc::new(gate(1_000, store.clone()));
        let tasks: Vec<_> = (0..64)
            .map(|n| {
                let gate = gate.clone();
                tokio::spawn(async move { gate.reserve(&format!("t{n}"), 100, TTL).await })
            })
            .collect();
        let mut admitted = 0;
        for task in tasks {
            match task.await.unwrap() {
                Ok(()) => admitted += 1,
                Err(error) => assert_eq!(error, GateError::Budget(BudgetError::MonthlyHardCap)),
            }
        }
        assert_eq!(admitted, 10);
        assert_eq!(gate.snapshot().reserved_cents, 1_000);
        assert_eq!(store.active(usize::MAX).await.unwrap().len(), 10);
    }

    #[tokio::test]
    async fn settle_and_cancel_release_both_sides() {
        let store = Arc::new(FakeStore::default());
        let gate = gate(1_000, store.clone());
        gate.reserve("a", 100, TTL).await.unwrap();
        gate.reserve("b", 100, TTL).await.unwrap();
        gate.settle("a", 30).await.unwrap();
        assert!(gate.cancel("b").await);
        assert!(!gate.cancel("b").await);
        let snapshot = gate.snapshot();
        assert_eq!((snapshot.actual_cents, snapshot.reserved_cents), (30, 0));
        assert_eq!(store.status("b"), Some(Status::Released));
        // The settle release is detached; let it run.
        for _ in 0..100 {
            if store.status("a") == Some(Status::Released) {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(store.status("a"), Some(Status::Released));
    }

    #[tokio::test]
    async fn a_failed_durable_write_reserves_nothing() {
        let store = Arc::new(FakeStore::default());
        store.fail_create.store(true, Ordering::SeqCst);
        let gate = gate(1_000, store.clone());
        assert_eq!(gate.reserve("a", 100, TTL).await, Err(GateError::Store));
        assert_eq!(gate.snapshot().reserved_cents, 0);
        assert_eq!(store.status("a"), None);
    }

    #[tokio::test]
    async fn sweep_expires_stale_holds_in_memory_and_durably() {
        let store = Arc::new(FakeStore::default());
        let gate = gate(100, store.clone());
        gate.reserve("stale", 100, Duration::ZERO).await.unwrap();
        gate.reserve("live", 0, TTL).await.unwrap();
        gate.sweep().await;
        assert_eq!(gate.snapshot().reserved_cents, 0);
        assert_eq!(store.status("stale"), Some(Status::Expired));
        assert_eq!(store.status("live"), Some(Status::Active));
        gate.reserve("next", 100, TTL).await.unwrap();
    }

    #[tokio::test]
    async fn reconcile_restores_active_rows_and_expires_lapsed_ones() {
        let store = Arc::new(FakeStore::default());
        let now = Instant::now();
        store.insert("survivor", 90, now + TTL);
        store.insert("crashed", 80, now);
        // A hold admitted under an older, larger cap still counts.
        store.insert("over-cap", 60, now + TTL);
        let gate = gate(100, store.clone());
        gate.reconcile().await.unwrap();
        let snapshot = gate.snapshot();
        assert_eq!(snapshot.reserved_cents, 150);
        assert_eq!(snapshot.remaining_hard_cents, 0);
        assert_eq!(store.status("crashed"), Some(Status::Expired));
        assert_eq!(
            gate.reserve("new", 1, TTL).await,
            Err(GateError::Budget(BudgetError::MonthlyHardCap))
        );
        assert!(gate.cancel("survivor").await);
        assert_eq!(store.status("survivor"), Some(Status::Released));
    }
}
