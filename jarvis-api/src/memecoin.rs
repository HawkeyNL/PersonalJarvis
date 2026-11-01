//! The memecoin PAPER trader: a simulation on read-only market data.
//!
//! It has no wallet, no signer and no execution path, and no LLM output
//! reaches it. Each tick it reloads the paper ledger from the database,
//! values open positions and simulates exits (also while paused), then, if
//! not paused, observes a few watchlist mints, runs the hard filters and the
//! strategy, and simulates entries. A [`TradeIntent`] is used on the spot by
//! the simulator and never stored or sent anywhere.
//!
//! [`TradeIntent`]: jarvis_onchain::strategy::TradeIntent

pub mod profile;
pub mod store;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use jarvis_market_data::dexscreener::{DexScreenerClient, Pair};
use jarvis_market_data::jupiter::{JupiterClient, QuoteRequest};
use jarvis_market_data::rpc::{CrossCheckedRpc, RpcEndpoint};
use jarvis_market_data::snapshot::{to_snapshot, Observation};
use jarvis_market_data::{ApiKey, MarketDataError};
use jarvis_onchain::filter::{self, RejectReason, Verdict};
use jarvis_onchain::gate::{self, GateReport, GateRules};
use jarvis_onchain::goals::{self, Progress};
use jarvis_onchain::paper::{self, Fill, Ledger, LedgerError, Mark, Pnl};
use jarvis_onchain::research::{apply_research, ResearchAdjustment};
use jarvis_onchain::strategy::{self, ExitReason};
use jarvis_onchain::types::{Position, TokenSnapshot};
use rand::Rng;
use rust_decimal::Decimal;
use time::{Duration, OffsetDateTime};

use jarvis_store::Database;
pub use profile::PaperProfile;
use store::{Candidate, Cost, CostKind, GateSnapshot, Trip, TripClose};

pub const TICK_INTERVAL: StdDuration = StdDuration::from_secs(60);
/// A whole tick, provider pacing included. Cancelling it is safe: the next
/// tick reloads the ledger from the database.
const TICK_TIMEOUT: StdDuration = StdDuration::from_secs(55);
const DB_TIMEOUT: StdDuration = StdDuration::from_secs(10);
/// Watchlist mints observed for an entry per tick.
pub const MAX_ENTRY_CANDIDATES_PER_TICK: usize = 5;
/// A mint is observed again for an entry at most this often.
const OBSERVE_COOLDOWN: Duration = Duration::minutes(15);
const GATE_SNAPSHOT_EVERY: Duration = Duration::hours(1);
/// Failed or unpriceable exits before a position is marked stuck.
pub const EXIT_FAILURE_CAP: u32 = 10;
/// A ledger dated further ahead than this means the clock jumped back or
/// was ahead before: stop instead of stamping events into the future, which
/// would stretch the paper gate's duration.
const CLOCK_TOLERANCE: Duration = Duration::minutes(5);
/// Sell quotes are taken into USDC.
const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

/// Shared between the task and the API.
#[derive(Default)]
pub struct PaperHandle {
    enabled: bool,
    running: AtomicBool,
    /// Set by the pause route right away, so entries stop within the tick.
    /// Only a restart with the resume setting can clear a pause.
    paused: AtomicBool,
    view: RwLock<Option<PaperView>>,
    /// Why the last tick stopped, if it did. Cleared by a good tick.
    last_error: RwLock<Option<&'static str>>,
}

impl PaperHandle {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            ..Self::default()
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    pub fn pause_now(&self) {
        self.paused.store(true, Ordering::SeqCst);
    }

    pub fn paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    pub fn view(&self) -> Option<PaperView> {
        self.view.read().ok().and_then(|view| view.clone())
    }

    /// A fixed message, never error detail (that goes to the log).
    pub fn last_error(&self) -> Option<&'static str> {
        self.last_error.read().ok().and_then(|error| *error)
    }

    /// Shown in the overview when the trader is enabled but did not start.
    pub fn note_start_failure(&self) {
        self.note_outcome(Some("paper trader did not start; see the Core log"));
    }

    /// Record a tick outcome; `true` when it differs from the previous one,
    /// so a persistent failure is logged once, not every minute.
    fn note_outcome(&self, error: Option<&'static str>) -> bool {
        let Ok(mut slot) = self.last_error.write() else {
            return true;
        };
        let changed = *slot != error;
        *slot = error;
        changed
    }
}

/// Clears `running` when the task ends, panics included.
struct RunningGuard(Arc<PaperHandle>);

impl Drop for RunningGuard {
    fn drop(&mut self) {
        self.0.running.store(false, Ordering::SeqCst);
    }
}

/// What the overview shows, for the current strategy version.
#[derive(Debug, Clone)]
pub struct PaperView {
    pub strategy_version: u32,
    pub at: OffsetDateTime,
    pub stake_usd: Decimal,
    pub pnl: Pnl,
    pub equity_usd: Decimal,
    pub open: Vec<OpenView>,
    pub gate: GateReport,
    pub goal_multiple: Decimal,
    pub goal: Option<Progress>,
}

#[derive(Debug, Clone)]
pub struct OpenView {
    pub mint: String,
    pub strategy_version: u32,
    pub tokens: Decimal,
    pub cost_usd: Decimal,
    /// Executable value along the pool curve; `None` without a fresh mark.
    pub value_usd: Option<Decimal>,
    pub opened_at: OffsetDateTime,
    pub exit_failures: u32,
    /// Exits stopped after [`EXIT_FAILURE_CAP`] failed attempts.
    pub stuck: bool,
}

/// Read-only market data. The live source wraps the market-data clients;
/// tests use fixtures.
#[async_trait]
pub trait MarketSource: Send + Sync {
    /// DexScreener pools for up to 30 mints, in one call.
    async fn pairs(&self, mints: &[&str]) -> Result<Vec<Pair>, MarketDataError>;
    /// Everything the filter needs about `pair`'s base token, with a sell
    /// quote for `sell_usd` worth of it.
    async fn entry_snapshot(
        &self,
        pair: &Pair,
        sell_usd: Decimal,
        now: OffsetDateTime,
    ) -> Result<TokenSnapshot, MarketDataError>;
}

pub struct LiveMarket {
    dex: DexScreenerClient,
    rpc: CrossCheckedRpc,
    jupiter: JupiterClient,
}

impl LiveMarket {
    /// Both RPC URLs are required and must name different hosts; the
    /// Jupiter key is optional. Errors never contain a URL or key.
    pub fn new(rpc_a: &str, rpc_b: &str, jupiter_key: &str) -> Result<Self, MarketDataError> {
        let key = jupiter_key.trim();
        let key = (!key.is_empty())
            .then(|| ApiKey::new(key.to_owned()))
            .transpose()?;
        Ok(Self {
            dex: DexScreenerClient::new()?,
            rpc: CrossCheckedRpc::new(
                RpcEndpoint::new(rpc_a.trim().to_owned())?,
                RpcEndpoint::new(rpc_b.trim().to_owned())?,
            )?,
            jupiter: JupiterClient::new(key)?,
        })
    }
}

#[async_trait]
impl MarketSource for LiveMarket {
    async fn pairs(&self, mints: &[&str]) -> Result<Vec<Pair>, MarketDataError> {
        self.dex.tokens(mints).await
    }

    async fn entry_snapshot(
        &self,
        pair: &Pair,
        sell_usd: Decimal,
        now: OffsetDateTime,
    ) -> Result<TokenSnapshot, MarketDataError> {
        let mint = pair.base_token.address.as_str();
        let (mint_account, largest) =
            tokio::try_join!(self.rpc.mint(mint), self.rpc.largest_accounts(mint))?;
        // Quote selling the largest position the simulation may open: a
        // smaller entry can only have less price impact.
        let sell_amount = pair
            .price_usd
            .filter(|price| *price > Decimal::ZERO)
            .and_then(|price| {
                let tokens = sell_usd.checked_div(price)?;
                let scale =
                    Decimal::from(10u64.checked_pow(u32::from(mint_account.get().decimals))?);
                u64::try_from(tokens.checked_mul(scale)?.floor()).ok()
            })
            .unwrap_or(0);
        let quote = if sell_amount == 0 {
            None
        } else {
            match self
                .jupiter
                .quote(&QuoteRequest {
                    input_mint: mint.to_owned(),
                    output_mint: USDC_MINT.to_owned(),
                    amount: sell_amount,
                    slippage_bps: None,
                })
                .await
            {
                Ok(quote) => Some(quote),
                // No route to sell: the filter treats it as a honeypot.
                Err(MarketDataError::NoRoute) => None,
                Err(error) => return Err(error),
            }
        };
        to_snapshot(&Observation {
            mint,
            observed_at: now,
            pair: Some(pair),
            mint_account: Some(&mint_account),
            largest_accounts: Some(&largest),
            excluded_accounts: &[],
            sell_amount,
            sell_quote: quote.as_ref(),
        })
    }
}

#[derive(Debug, PartialEq)]
pub enum ReplayError {
    Ledger(LedgerError),
    /// A stored trip's net result differs from the recomputed one.
    Mismatch,
}

impl From<LedgerError> for ReplayError {
    fn from(error: LedgerError) -> Self {
        Self::Ledger(error)
    }
}

/// Rebuild the ledger from stored rows in time order (closes before opens
/// at the same instant) and check every stored trip result.
pub fn replay(trips: &[&Trip], costs: &[&Cost]) -> Result<Ledger, ReplayError> {
    let mut events: Vec<(OffsetDateTime, u8, &str, &Trip)> = Vec::with_capacity(trips.len() * 2);
    for trip in trips {
        events.push((trip.opened_at, 1, &trip.id, trip));
        if let Some(close) = &trip.close {
            events.push((close.closed_at, 0, &trip.id, trip));
        }
    }
    events.sort_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)));
    let mut ledger = Ledger::default();
    for (_, kind, _, trip) in events {
        match (kind, &trip.close) {
            (0, Some(close)) => {
                let net = ledger.close(
                    &trip.mint,
                    trip.tokens,
                    close.usd_out,
                    close.exit_fees_usd,
                    close.closed_at,
                )?;
                if net != close.net_pnl_usd {
                    return Err(ReplayError::Mismatch);
                }
            }
            _ => ledger.open(position(trip))?,
        }
    }
    for cost in costs {
        match cost.kind {
            CostKind::FailedTx => ledger.record_failed_tx(cost.usd)?,
            CostKind::AiCost => ledger.record_ai_cost(cost.usd)?,
        }
    }
    Ok(ledger)
}

fn position(trip: &Trip) -> Position {
    Position {
        mint: trip.mint.clone(),
        tokens: trip.tokens,
        cost_usd: trip.cost_usd,
        opened_at: trip.opened_at,
        entry_liquidity_usd: trip.entry_liquidity_usd,
    }
}

pub fn reject_reason(reason: RejectReason) -> &'static str {
    match reason {
        RejectReason::StaleSnapshot => "stale_snapshot",
        RejectReason::InvalidSnapshot => "invalid_snapshot",
        RejectReason::Denied => "denied",
        RejectReason::MintAuthorityActive => "mint_authority_active",
        RejectReason::FreezeAuthorityActive => "freeze_authority_active",
        RejectReason::UnknownExtension(_) => "unknown_extension",
        RejectReason::ExtensionNotAllowed(_) => "extension_not_allowed",
        RejectReason::ExtensionAuthorityActive(_) => "extension_authority_active",
        RejectReason::LpNotSecured => "lp_not_secured",
        RejectReason::LpUnlocksTooSoon => "lp_unlocks_too_soon",
        RejectReason::LiquidityTooLow => "liquidity_too_low",
        RejectReason::PoolTooYoung => "pool_too_young",
        RejectReason::HolderConcentrationTooHigh => "holder_concentration_too_high",
        RejectReason::NoSellRoute => "no_sell_route",
        RejectReason::SellImpactTooHigh => "sell_impact_too_high",
    }
}

fn exit_reason(reason: ExitReason) -> &'static str {
    match reason {
        ExitReason::LiquidityDrop => "liquidity_drop",
        ExitReason::StopLoss => "stop_loss",
        ExitReason::TakeProfit => "take_profit",
        ExitReason::TimeStop => "time_stop",
    }
}

/// The deepest Solana pool whose base token is `mint`.
fn main_pair<'a>(pairs: &'a [Pair], mint: &str) -> Option<&'a Pair> {
    pairs
        .iter()
        .filter(|pair| pair.chain_id == "solana" && pair.base_token.address == mint)
        .max_by_key(|pair| pair.liquidity.as_ref().and_then(|l| l.usd))
}

/// Price and liquidity from DexScreener alone: enough to value and exit a
/// position, never enough to pass the entry filter.
fn pair_snapshot(pair: &Pair, now: OffsetDateTime) -> Option<TokenSnapshot> {
    to_snapshot(&Observation {
        mint: &pair.base_token.address,
        observed_at: now,
        pair: Some(pair),
        mint_account: None,
        largest_accounts: None,
        excluded_accounts: &[],
        sell_amount: 0,
        sell_quote: None,
    })
    .ok()
}

/// A uniform roll in `[0, 1)` for the simulated failure rate.
fn roll() -> Decimal {
    Decimal::new(rand::thread_rng().gen_range(0..1_000_000), 6)
}

/// One call with a timeout. The store error is logged here; callers and the
/// overview only ever see a fixed message.
async fn bounded<T>(
    what: &'static str,
    future: impl std::future::Future<Output = Result<T, store::MemecoinStoreError>>,
) -> Result<T, &'static str> {
    match tokio::time::timeout(DB_TIMEOUT, future).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => {
            tracing::warn!(%error, what, "memecoin paper database call failed");
            Err("database call failed")
        }
        Err(_) => {
            tracing::warn!(what, "memecoin paper database call timed out");
            Err("database call timed out")
        }
    }
}

/// Per-tick counters for the log line.
#[derive(Debug, Default, PartialEq)]
pub struct TickStats {
    pub observed: usize,
    pub accepted: usize,
    pub entries: usize,
    pub exits: usize,
    pub failed_attempts: usize,
    pub unpriced_exits: usize,
    pub stuck: usize,
    pub provider_errors: usize,
}

/// State the task keeps between ticks. Everything else is reloaded.
pub struct PaperTask {
    pub profile: PaperProfile,
    pub handle: Arc<PaperHandle>,
    /// Last observation per mint, to pace providers and to write a
    /// candidate row only when its verdict changes.
    seen: HashMap<String, (Option<&'static str>, OffsetDateTime)>,
    last_gate_at: Option<OffsetDateTime>,
}

/// The whole stored ledger for the current strategy version.
async fn load_history(db: &Database, version: u32) -> Result<Ledger, &'static str> {
    let trips = bounded("trips", store::load_trips(db)).await?;
    let costs = bounded("costs", store::load_costs(db)).await?;
    // Every stored result must recompute, whatever its version.
    replay(
        &trips.iter().collect::<Vec<_>>(),
        &costs.iter().collect::<Vec<_>>(),
    )
    .map_err(|error| {
        tracing::error!(?error, "memecoin paper ledger does not replay");
        "paper ledger does not replay"
    })?;
    let ours: Vec<&Trip> = trips
        .iter()
        .filter(|t| t.strategy_version == version)
        .collect();
    let our_costs: Vec<&Cost> = costs
        .iter()
        .filter(|c| c.strategy_version == version)
        .collect();
    replay(&ours, &our_costs).map_err(|_| "paper ledger does not replay")
}

impl PaperTask {
    pub fn new(profile: PaperProfile, handle: Arc<PaperHandle>) -> Self {
        Self {
            profile,
            handle,
            seen: HashMap::new(),
            last_gate_at: None,
        }
    }

    /// One tick. `Err` stops the tick; nothing is simulated without its
    /// write having succeeded first. Exits need only the held positions, so
    /// a problem with the stored history stops entries and the report, never
    /// exits.
    pub async fn tick(
        &mut self,
        db: &Database,
        source: &dyn MarketSource,
        roll: &mut (dyn FnMut() -> Decimal + Send),
    ) -> Result<TickStats, &'static str> {
        let mut stats = TickStats::default();
        let mut held = bounded("held trips", store::load_held_trips(db)).await?;
        let latest_close = bounded("latest trip", store::recent_closed_trips(db, 1)).await?;
        let costs = bounded("costs", store::load_costs(db)).await?;
        // Events are strictly ordered by our own clock, even if it steps back.
        let mut last = held
            .iter()
            .map(|t| t.opened_at)
            .chain(
                latest_close
                    .iter()
                    .filter_map(|t| t.close.as_ref().map(|c| c.closed_at)),
            )
            .chain(costs.iter().map(|c| c.at))
            .max();
        if last.is_some_and(|last| last > OffsetDateTime::now_utc() + CLOCK_TOLERANCE) {
            return Err("paper ledger is dated ahead of the clock");
        }
        let mut stamp = move || {
            let at = OffsetDateTime::now_utc()
                .max(last.map_or(OffsetDateTime::UNIX_EPOCH, |l| l + Duration::nanoseconds(1)));
            last = Some(at);
            at
        };
        let p = &self.profile;
        let version = p.strategy.version;
        let history = load_history(db, version).await;
        // A stored pause latches; a failed read pauses this tick only.
        let read_failed = match bounded("pause state", store::control(db)).await {
            Ok(control) => {
                if control.paused {
                    self.handle.pause_now();
                }
                false
            }
            Err(_) => true,
        };
        let paused = || read_failed || self.handle.paused();

        let held_mints: BTreeSet<String> = held.iter().map(|t| t.mint.clone()).collect();
        let mut mints: Vec<&str> = held_mints.iter().map(String::as_str).collect();
        mints.extend(
            p.watchlist
                .iter()
                .map(String::as_str)
                .filter(|m| !held_mints.contains(*m)),
        );
        let pairs = match source.pairs(&mints).await {
            Ok(pairs) => pairs,
            Err(error) => {
                stats.provider_errors += 1;
                tracing::warn!(%error, "memecoin paper: no market data this tick");
                Vec::new()
            }
        };
        let now = OffsetDateTime::now_utc();
        let mut marks = BTreeMap::new();
        for mint in &mints {
            if let Some(snapshot) =
                main_pair(&pairs, mint).and_then(|pair| pair_snapshot(pair, now))
            {
                marks.insert(
                    (*mint).to_owned(),
                    Mark {
                        price_usd: snapshot.price_usd,
                        liquidity_usd: snapshot.liquidity_usd,
                    },
                );
            }
        }

        // Exits first, for every held position and also while paused. A
        // position whose exit failed EXIT_FAILURE_CAP times is stuck.
        for trip in held.iter_mut().filter(|t| !t.stuck) {
            let Some(snapshot) =
                main_pair(&pairs, &trip.mint).and_then(|pair| pair_snapshot(pair, now))
            else {
                continue;
            };
            let Some(intent) = strategy::evaluate_exit(
                &position(trip),
                &snapshot,
                &p.strategy,
                &p.costs,
                p.rules.max_snapshot_age,
                now,
            ) else {
                continue;
            };
            let strategy::IntentKind::Exit(reason) = intent.kind else {
                continue;
            };
            match paper::sell(
                trip.tokens,
                snapshot.price_usd,
                snapshot.liquidity_usd,
                &p.costs,
                roll(),
            ) {
                Some(Fill::Filled {
                    amount_out,
                    fees_usd,
                }) => {
                    let net_pnl_usd = amount_out
                        .checked_sub(fees_usd)
                        .and_then(|v| v.checked_sub(trip.cost_usd))
                        .ok_or("paper exit overflow")?;
                    let close = TripClose {
                        closed_at: stamp(),
                        usd_out: amount_out,
                        exit_fees_usd: fees_usd,
                        net_pnl_usd,
                        exit_reason: exit_reason(reason),
                    };
                    if !bounded("close trip", store::close_trip(db, &trip.id, &close)).await? {
                        return Err("paper trip was not open");
                    }
                    trip.close = Some(close);
                    stats.exits += 1;
                }
                failed => {
                    // The cost first: a lost counter only means one more try.
                    if let Some(Fill::Failed { fees_usd }) = failed {
                        let cost = store::add_cost(
                            db,
                            CostKind::FailedTx,
                            trip.strategy_version,
                            fees_usd,
                            stamp(),
                        );
                        bounded("add cost", cost).await?;
                        stats.failed_attempts += 1;
                    } else {
                        stats.unpriced_exits += 1;
                    }
                    let failure = store::record_exit_failure(db, &trip.id, EXIT_FAILURE_CAP);
                    let stuck = bounded("exit failure", failure)
                        .await?
                        .ok_or("paper trip was not open")?;
                    trip.exit_failures += 1;
                    if stuck {
                        trip.stuck = true;
                        stats.stuck += 1;
                        tracing::warn!(mint = %trip.mint, "memecoin paper position stuck; exits stopped");
                    }
                }
            }
        }

        // Entries need an intact history: a few watchlist mints not held at
        // the start of the tick, least recently observed first.
        let mut open = held.iter().filter(|t| t.close.is_none()).count();
        let mut due: Vec<&String> = p
            .watchlist
            .iter()
            .filter(|m| !held_mints.contains(*m))
            .filter(|m| {
                self.seen
                    .get(*m)
                    .is_none_or(|(_, at)| now - *at >= OBSERVE_COOLDOWN)
            })
            .collect();
        due.sort_by_key(|m| self.seen.get(*m).map(|(_, at)| *at));
        due.truncate(MAX_ENTRY_CANDIDATES_PER_TICK);
        if history.is_err() {
            due.clear();
        }
        let deny_list = BTreeSet::new();
        for mint in due {
            if paused() || open >= p.max_open_positions {
                break;
            }
            stats.observed += 1;
            let observed = match main_pair(&pairs, mint) {
                Some(pair) => source
                    .entry_snapshot(pair, p.max_position_usd, now)
                    .await
                    .map_err(|error| {
                        stats.provider_errors += 1;
                        tracing::warn!(%error, %mint, "memecoin paper: candidate not observed");
                    })
                    .ok(),
                None => None,
            };
            let at = OffsetDateTime::now_utc();
            let reason = match &observed {
                None => Some("data_unavailable"),
                Some(snapshot) => match filter::check(snapshot, &p.rules, &deny_list, at) {
                    Verdict::Accept => None,
                    Verdict::Reject(reason) => Some(reject_reason(reason)),
                },
            };
            let changed = self.seen.get(mint).is_none_or(|(last, _)| *last != reason);
            self.seen.insert(mint.clone(), (reason, at));
            if changed {
                let candidate = Candidate {
                    id: uuid::Uuid::now_v7().to_string(),
                    mint: mint.clone(),
                    observed_at: observed.as_ref().map_or(at, |s| s.observed_at),
                    strategy_version: version,
                    reason,
                    price_usd: observed.as_ref().map(|s| s.price_usd),
                    liquidity_usd: observed.as_ref().map(|s| s.liquidity_usd),
                };
                // Best effort: a lost decision row must not stop trading.
                let _ = bounded("candidate", store::insert_candidate(db, &candidate)).await;
            }
            let Some(snapshot) = observed.filter(|_| reason.is_none()) else {
                continue;
            };
            stats.accepted += 1;
            let Some(intent) = strategy::evaluate_entry(
                &snapshot,
                &p.strategy,
                &p.rules,
                &deny_list,
                p.max_position_usd,
                at,
            ) else {
                continue;
            };
            // No research step runs yet; it could only veto or shrink.
            let size = apply_research(intent.amount_in, ResearchAdjustment::Keep);
            if paused() {
                break;
            }
            match paper::buy(
                size,
                snapshot.price_usd,
                snapshot.liquidity_usd,
                &p.costs,
                roll(),
            ) {
                Some(Fill::Filled {
                    amount_out,
                    fees_usd,
                }) => {
                    let trip = Trip {
                        id: uuid::Uuid::now_v7().to_string(),
                        mint: mint.clone(),
                        strategy_version: version,
                        tokens: amount_out,
                        cost_usd: size.checked_add(fees_usd).ok_or("paper entry overflow")?,
                        entry_liquidity_usd: snapshot.liquidity_usd,
                        opened_at: stamp(),
                        exit_failures: 0,
                        stuck: false,
                        close: None,
                    };
                    bounded("open trip", store::open_trip(db, &trip)).await?;
                    held.push(trip);
                    open += 1;
                    stats.entries += 1;
                }
                Some(Fill::Failed { fees_usd }) => {
                    let cost = store::add_cost(db, CostKind::FailedTx, version, fees_usd, stamp());
                    bounded("add cost", cost).await?;
                    stats.failed_attempts += 1;
                }
                None => {}
            }
        }

        // Report on the current strategy version, from the stored ledger.
        history?;
        // ponytail: reloads the whole ledger every tick; keep it in memory if it grows large.
        let ledger = load_history(db, version).await?;
        let rules = GateRules {
            min_closed_trades: gate::MIN_CLOSED_TRADES,
            min_duration: gate::MIN_DURATION,
            min_trading_days: gate::MIN_TRADING_DAYS,
        };
        let report = gate::evaluate(&ledger, &marks, &p.costs, &rules)
            .map_err(|_| "paper gate cannot be computed")?;
        let pnl = ledger
            .pnl(&marks, &p.costs)
            .map_err(|_| "paper result cannot be computed")?;
        let equity_usd = p
            .stake_usd
            .checked_add(pnl.net_usd)
            .ok_or("paper equity overflow")?;
        let open_view = held
            .iter()
            .filter(|t| t.close.is_none())
            .map(|t| OpenView {
                mint: t.mint.clone(),
                strategy_version: t.strategy_version,
                tokens: t.tokens,
                cost_usd: t.cost_usd,
                value_usd: marks.get(&t.mint).and_then(|m| {
                    paper::sell_value(t.tokens, m.price_usd, m.liquidity_usd, &p.costs)
                }),
                opened_at: t.opened_at,
                exit_failures: t.exit_failures,
                stuck: t.stuck,
            })
            .collect();
        let view = PaperView {
            strategy_version: version,
            at: now,
            stake_usd: p.stake_usd,
            pnl,
            equity_usd,
            open: open_view,
            gate: report,
            goal_multiple: p.goal_multiple,
            goal: goals::progress(
                p.stake_usd,
                equity_usd.max(Decimal::ZERO),
                Decimal::ZERO,
                p.goal_multiple,
            ),
        };
        if let Ok(mut slot) = self.handle.view.write() {
            *slot = Some(view);
        }
        if self
            .last_gate_at
            .is_none_or(|at| now - at >= GATE_SNAPSHOT_EVERY)
        {
            let snapshot = GateSnapshot {
                id: uuid::Uuid::now_v7().to_string(),
                at: now,
                strategy_version: version,
                closed_trades: report.closed_trades as u64,
                duration_secs: report.duration.whole_seconds().max(0) as u64,
                trading_days: report.trading_days as u64,
                net_result_usd: report.net_result_usd,
                net_expectancy_usd: report.net_expectancy_usd,
            };
            if bounded("gate snapshot", store::insert_gate_snapshot(db, &snapshot))
                .await
                .is_ok()
            {
                self.last_gate_at = Some(now);
            }
        }
        Ok(stats)
    }
}

/// Settings for [`start`]; secrets stay inside `market`.
pub struct StartConfig {
    pub profile: PaperProfile,
    pub market: Box<dyn MarketSource>,
    /// Clear the pause made at exactly this time, if it is the current one.
    pub resume_paused_at: Option<OffsetDateTime>,
}

/// Record the strategy version, apply a configured resume and spawn the
/// interval task. An error leaves the trader off; Core keeps running.
pub async fn start(
    db: Database,
    handle: Arc<PaperHandle>,
    config: StartConfig,
) -> Result<(), String> {
    let StartConfig {
        profile,
        market,
        resume_paused_at,
    } = config;
    let register =
        store::register_strategy_version(&db, profile.strategy.version, &profile.versioned_params);
    bounded("strategy version", register).await.map_err(|error| {
        format!(
            "strategy version {} cannot be recorded ({error}); a changed profile needs a new version",
            profile.strategy.version
        )
    })?;
    if let Some(paused_at) = resume_paused_at {
        match bounded("resume", store::resume(&db, paused_at)).await {
            Ok(true) => tracing::warn!(%paused_at, "memecoin paper pause cleared by configuration"),
            Ok(false) => tracing::info!("memecoin paper resume setting matches no current pause"),
            Err(_) => tracing::warn!("memecoin paper resume failed"),
        }
    }
    let version = profile.strategy.version;
    let mut task = PaperTask::new(profile, handle.clone());
    handle.running.store(true, Ordering::SeqCst);
    let guard = RunningGuard(handle.clone());
    tokio::spawn(async move {
        let _guard = guard;
        let mut interval =
            tokio::time::interval_at(tokio::time::Instant::now() + TICK_INTERVAL, TICK_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            let mut dice = roll;
            let outcome =
                tokio::time::timeout(TICK_TIMEOUT, task.tick(&db, market.as_ref(), &mut dice))
                    .await;
            let error = match &outcome {
                Ok(Ok(_)) => None,
                Ok(Err(error)) => Some(*error),
                Err(_) => Some("tick timed out"),
            };
            let changed = handle.note_outcome(error);
            match (outcome, error) {
                (Ok(Ok(stats)), _) => tracing::info!(
                    observed = stats.observed,
                    accepted = stats.accepted,
                    entries = stats.entries,
                    exits = stats.exits,
                    failed_attempts = stats.failed_attempts,
                    unpriced_exits = stats.unpriced_exits,
                    stuck = stats.stuck,
                    provider_errors = stats.provider_errors,
                    "memecoin paper tick"
                ),
                (_, Some(error)) if changed => {
                    tracing::error!(%error, "memecoin paper tick stopped")
                }
                _ => {}
            }
        }
    });
    tracing::info!(
        strategy_version = version,
        "memecoin PAPER trader running (no execution path)"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;
    use std::sync::Mutex;

    use jarvis_onchain::types::LpStatus;

    use super::profile::tests::{fixture, MINT_A, MINT_B};
    use super::*;

    fn d(value: &str) -> Decimal {
        Decimal::from_str(value).unwrap()
    }

    fn pair(mint: &str, price: &str, liquidity: &str) -> Pair {
        serde_json::from_value(serde_json::json!({
            "chainId": "solana", "dexId": "raydium", "pairAddress": format!("pool-{mint}"),
            "baseToken": {"address": mint}, "quoteToken": {"address": USDC_MINT},
            "priceUsd": price, "liquidity": {"usd": liquidity},
            "txns": {"m5": {"buys": 40, "sells": 10}}, "priceChange": {"m5": 12.5},
            "pairCreatedAt": (OffsetDateTime::now_utc() - Duration::days(3)).unix_timestamp() * 1000,
        }))
        .unwrap()
    }

    fn trip(id: &str, mint: &str, opened: i64, close: Option<(i64, &str, &str)>) -> Trip {
        let t0 = OffsetDateTime::UNIX_EPOCH + Duration::days(20_000);
        Trip {
            id: id.into(),
            mint: mint.into(),
            strategy_version: 3,
            tokens: d("100"),
            cost_usd: d("25"),
            entry_liquidity_usd: d("100000"),
            opened_at: t0 + Duration::seconds(opened),
            exit_failures: 0,
            stuck: false,
            close: close.map(|(at, usd_out, net)| TripClose {
                closed_at: t0 + Duration::seconds(at),
                usd_out: d(usd_out),
                exit_fees_usd: d("0"),
                net_pnl_usd: d(net),
                exit_reason: "take_profit",
            }),
        }
    }

    #[test]
    fn replay_rebuilds_round_trips_and_checks_stored_results() {
        // A close and the next open of the same mint at one instant: the
        // close is replayed first, so the two trips stay apart.
        let first = trip("a", MINT_A, 0, Some((60, "30", "5")));
        let second = trip("b", MINT_A, 60, None);
        let ledger = replay(&[&second, &first], &[]).unwrap();
        assert_eq!(ledger.closed_trades().len(), 1);
        assert_eq!(ledger.closed_trades()[0].net_pnl_usd, d("5"));
        assert_eq!(ledger.lots().len(), 1);
        let cost = Cost {
            kind: CostKind::FailedTx,
            usd: d("0.5"),
            at: first.opened_at,
            strategy_version: 3,
        };
        let ledger = replay(&[&first], &[&cost]).unwrap();
        assert_eq!(ledger.failed_tx_fees_usd(), d("0.5"));
        // A tampered result fails closed.
        let tampered = trip("a", MINT_A, 0, Some((60, "30", "6")));
        assert_eq!(
            replay(&[&tampered], &[]).unwrap_err(),
            ReplayError::Mismatch
        );
    }

    #[test]
    fn main_pair_is_the_deepest_pool_of_the_mint() {
        let pairs = [
            pair(MINT_A, "1", "500"),
            pair(MINT_A, "1", "900"),
            pair(MINT_B, "1", "5000"),
        ];
        let best = main_pair(&pairs, MINT_A).unwrap();
        assert_eq!(best.liquidity.as_ref().and_then(|l| l.usd), Some(d("900")));
        assert!(main_pair(&pairs, "11111111111111111111111111111111").is_none());
        // DexScreener data alone never passes the entry filter.
        let now = OffsetDateTime::now_utc();
        let snapshot = pair_snapshot(best, now).unwrap();
        assert_eq!(
            filter::check(&snapshot, &fixture().rules, &BTreeSet::new(), now),
            Verdict::Reject(RejectReason::MintAuthorityActive)
        );
    }

    #[test]
    fn rolls_stay_in_the_unit_interval() {
        for _ in 0..1_000 {
            let value = roll();
            assert!(value >= Decimal::ZERO && value < Decimal::ONE);
        }
    }

    /// Fixture market data: `MINT_A` passes every filter, `MINT_B` has an
    /// unsecured pool (as every live snapshot has today).
    struct FixtureMarket {
        price_a: Mutex<Decimal>,
        /// Paused by the owner while the first candidate is observed.
        pause_on_observe: Option<Arc<PaperHandle>>,
    }

    #[async_trait]
    impl MarketSource for FixtureMarket {
        async fn pairs(&self, mints: &[&str]) -> Result<Vec<Pair>, MarketDataError> {
            let price = self.price_a.lock().unwrap().to_string();
            Ok(mints
                .iter()
                .map(|m| pair(m, if *m == MINT_A { &price } else { "0.002" }, "100000"))
                .collect())
        }

        async fn entry_snapshot(
            &self,
            pair: &Pair,
            _sell_usd: Decimal,
            now: OffsetDateTime,
        ) -> Result<TokenSnapshot, MarketDataError> {
            if let Some(handle) = &self.pause_on_observe {
                handle.pause_now();
            }
            let mut s = pair_snapshot(pair, now).ok_or(MarketDataError::NotFound("pair"))?;
            s.mint_authority_revoked = true;
            s.freeze_authority_revoked = true;
            s.top_holders_share = d("0.1");
            s.sell_impact_bps = Some(50);
            s.lp = if pair.base_token.address == MINT_A {
                LpStatus::Burned
            } else {
                LpStatus::Unknown
            };
            Ok(s)
        }
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn tick_simulates_entries_and_exits_on_fixture_data(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use surrealdb::{engine::remote::ws::Ws, opt::auth::Root, Surreal};
        let db = Surreal::new::<Ws>(std::env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
        db.signin(Root {
            username: &std::env::var("JARVIS_SURREAL_TEST_USER")?,
            password: &std::env::var("JARVIS_SURREAL_TEST_PASS")?,
        })
        .await?;
        db.use_ns(format!("memecoin_tick_{}", uuid::Uuid::now_v7().simple()))
            .use_db("core")
            .await?;
        jarvis_store::apply_baseline_schema(&db).await?;
        let handle = Arc::new(PaperHandle::new(true));
        let mut task = PaperTask::new(fixture(), handle.clone());
        let market = FixtureMarket {
            price_a: Mutex::new(d("0.001")),
            pause_on_observe: None,
        };
        // Rolls at or above the 10% failure rate fill; a roll of 0 fails.
        let mut no_failure = || Decimal::new(999_999, 6);

        // Tick 1: A is accepted and bought, B is rejected for its pool.
        let stats = task.tick(&db, &market, &mut no_failure).await?;
        assert_eq!((stats.observed, stats.accepted, stats.entries), (2, 1, 1));
        let candidates = store::recent_candidates(&db, 10).await?;
        let reasons: BTreeSet<_> = candidates.iter().map(|c| c.reason.as_deref()).collect();
        assert_eq!(reasons, BTreeSet::from([None, Some("lp_not_secured")]));
        let trips = store::load_trips(&db).await?;
        assert_eq!(trips.len(), 1);
        assert_eq!(trips[0].mint, MINT_A);
        assert_eq!(trips[0].cost_usd, d("25.011"));
        let view = handle.view().ok_or("no view")?;
        assert_eq!(view.open.len(), 1);
        assert!(
            view.equity_usd < view.stake_usd,
            "fees and price impact cost money"
        );

        // Tick 2: A doubles; the take profit closes it. B is in its cooldown,
        // so no provider is asked about it again.
        *market.price_a.lock().unwrap() = d("0.002");
        let stats = task.tick(&db, &market, &mut no_failure).await?;
        assert_eq!((stats.observed, stats.exits), (0, 1));
        let closed = store::recent_closed_trips(&db, 10).await?;
        let close = closed[0].close.as_ref().ok_or("not closed")?;
        assert_eq!(close.exit_reason, "take_profit");
        assert!(close.net_pnl_usd > Decimal::ZERO);
        let view = handle.view().ok_or("no view")?;
        assert_eq!(view.gate.closed_trades, 1);
        assert!(!view.gate.passed);
        assert!(view.equity_usd > view.stake_usd);
        assert_eq!(store::recent_candidates(&db, 10).await?.len(), 2);

        // Tick 3 (cooldown over): the buy attempt fails and its fees are a cost.
        task.seen.clear();
        let mut always_fail = || Decimal::ZERO;
        let stats = task.tick(&db, &market, &mut always_fail).await?;
        assert_eq!((stats.entries, stats.failed_attempts), (0, 1));
        assert_eq!(store::load_costs(&db).await?[0].usd, d("0.011"));
        assert!(handle.view().ok_or("no view")?.pnl.failed_tx_fees_usd > Decimal::ZERO);

        // Tick 4: a stored pause is picked up and stops entries.
        store::pause(&db, "device").await?;
        task.seen.clear();
        let stats = task.tick(&db, &market, &mut no_failure).await?;
        assert!(handle.paused());
        assert_eq!((stats.observed, stats.entries), (0, 0));
        assert_eq!(store::load_trips(&db).await?.len(), 1);
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn a_pause_during_a_tick_stops_the_next_buy() -> Result<(), Box<dyn std::error::Error>> {
        use surrealdb::{engine::remote::ws::Ws, opt::auth::Root, Surreal};
        let db = Surreal::new::<Ws>(std::env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
        db.signin(Root {
            username: &std::env::var("JARVIS_SURREAL_TEST_USER")?,
            password: &std::env::var("JARVIS_SURREAL_TEST_PASS")?,
        })
        .await?;
        db.use_ns(format!("memecoin_pause_{}", uuid::Uuid::now_v7().simple()))
            .use_db("core")
            .await?;
        jarvis_store::apply_baseline_schema(&db).await?;
        let handle = Arc::new(PaperHandle::new(true));
        let mut task = PaperTask::new(fixture(), handle.clone());
        // The owner pauses while A, which would be bought, is observed.
        let market = FixtureMarket {
            price_a: Mutex::new(d("0.001")),
            pause_on_observe: Some(handle.clone()),
        };
        let stats = task
            .tick(&db, &market, &mut || Decimal::new(999_999, 6))
            .await?;
        assert_eq!((stats.observed, stats.accepted, stats.entries), (1, 1, 0));
        assert!(store::load_trips(&db).await?.is_empty());
        Ok(())
    }
}
