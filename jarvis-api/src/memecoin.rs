//! The memecoin PAPER trader: a simulation on read-only market data.
//!
//! It has no wallet, no signer and no execution path. Claude may decide
//! paper entries and exits and propose strategy versions ([`agent`]), only
//! past the same checks. Each tick it reloads the paper ledger from the
//! database, values open positions and simulates exits by the rules of the
//! strategy version that opened each one (also while paused or when that
//! strategy is disabled), records the price and liquidity it saw, then, if
//! not paused, observes a few watchlist mints and mints found by
//! [`discovery`], runs the hard filters and every enabled strategy's active
//! [`StrategyConfig`] on signals from those recorded observations, and
//! simulates entries (or offers them to Claude). Nothing it decides is stored
//! as an order or sent anywhere.
//!
//! [`discovery`]: jarvis_market_data::discovery

pub mod agent;
pub mod profile;
pub mod store;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use jarvis_market_data::dexscreener::{DexScreenerClient, Pair};
use jarvis_market_data::discovery::{self, Discovery, Migration, PollStats};
use jarvis_market_data::jupiter::{JupiterClient, QuoteRequest};
use jarvis_market_data::pools;
use jarvis_market_data::rpc::{CrossCheckedRpc, RpcEndpoint};
use jarvis_market_data::snapshot::{to_snapshot, Observation};
use jarvis_market_data::{ApiKey, MarketDataError};
use jarvis_onchain::config::{seeds, StrategyConfig};
use jarvis_onchain::filter::{self, FilterRules, RejectReason, Verdict};
use jarvis_onchain::gate::{self, GateReport, GateRules};
use jarvis_onchain::goals::{self, Progress};
use jarvis_onchain::paper::{self, Fill, Ledger, LedgerError, Mark, Pnl};
use jarvis_onchain::research::{apply_research, ResearchAdjustment};
use jarvis_onchain::signals::{self, Observation as SeriesPoint, Series, SignalId};
use jarvis_onchain::strategy::{self, ExitReason, ExitRules};
use jarvis_onchain::types::{Position, TokenSnapshot};
use rand::Rng;
use rust_decimal::{Decimal, RoundingStrategy};
use serde::Deserialize;
use time::{Date, Duration, OffsetDateTime, UtcOffset};

use jarvis_client_core::realtime::Event;
use jarvis_llm::claude_worker_protocol::WorkerTask;
use jarvis_llm::{ChatReply, LlmError};
use jarvis_store::Database;
use profile::is_mint;
pub use profile::PaperProfile;
use store::{
    Candidate, Cost, CostKind, DecisionKind, GateSnapshot, StrategyAuthor, StrategyVersion, Trip,
    TripClose,
};

use crate::realtime::Hub;

pub const TICK_INTERVAL: StdDuration = StdDuration::from_secs(60);
/// A whole tick, provider pacing included. Cancelling it is safe: the next
/// tick reloads the ledger from the database.
const TICK_TIMEOUT: StdDuration = StdDuration::from_secs(55);
const DB_TIMEOUT: StdDuration = StdDuration::from_secs(10);
/// Watchlist and discovered mints observed for an entry per tick.
pub const MAX_ENTRY_CANDIDATES_PER_TICK: usize = 5;
/// pump.fun migrations are polled at most this often ...
const DISCOVERY_EVERY: Duration = Duration::minutes(2);
/// ... and for at most this long, so a slow provider leaves the tick time
/// for entries. A cut-off poll keeps what it handled and resumes next time.
const DISCOVERY_TIMEOUT: StdDuration = StdDuration::from_secs(15);
/// A mint is observed again for an entry at most this often.
const OBSERVE_COOLDOWN: Duration = Duration::minutes(15);
const GATE_SNAPSHOT_EVERY: Duration = Duration::hours(1);
/// Failed or unpriceable exits before a position is marked stuck.
pub const EXIT_FAILURE_CAP: u32 = 10;
/// A ledger dated further ahead than this means the clock jumped back or
/// was ahead before: stop instead of stamping events into the future, which
/// would stretch the paper gate's duration.
const CLOCK_TOLERANCE: Duration = Duration::minutes(5);
/// Typed strategy versions start above this; legacy profile versions stay
/// below it. Versions are unique across strategies, so the two never collide.
pub const STRATEGY_VERSION_FLOOR: u32 = 1_000_000;
/// Exits of a position whose version has no usable rules: only the time
/// stop, at the longest hold any typed version may have. The price-based
/// fields are never read, as such a position is evaluated without a mark.
const TIME_STOP_ONLY: ExitRules = ExitRules {
    take_profit_bps: strategy::bounds::TAKE_PROFIT_BPS.1,
    stop_loss_bps: strategy::bounds::STOP_LOSS_BPS.1,
    trailing_stop_bps: None,
    liquidity_drop_exit_bps: strategy::bounds::LIQUIDITY_DROP_BPS.1,
    max_hold: strategy::bounds::CONFIG_MAX_HOLD.1,
};
/// One fresh observation for an entry Claude chose ...
const AGENT_ENTRY_OBSERVE_TIMEOUT: StdDuration = StdDuration::from_secs(5);
/// ... and all of a decision's entries together: those left when this has
/// passed are skipped (`apply_deadline`), so applying a decision leaves the
/// tick most of [`TICK_TIMEOUT`].
const AGENT_APPLY_DEADLINE: StdDuration = StdDuration::from_secs(20);
/// Sell quotes are taken into USDC.
const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

/// Core sends the memecoin realtime events. Apps from 0.1.27 on (app PR
/// #118) ignore event types they do not know; older apps fail on them.
pub const EMIT_MEME_EVENTS: bool = true;

/// Publish a memecoin event, while [`EMIT_MEME_EVENTS`] allows it.
pub(crate) fn announce(events: Option<&Hub>, event: Event) {
    if EMIT_MEME_EVENTS {
        if let Some(events) = events {
            events.publish_all(event);
        }
    }
}

/// How long an enable challenge can be signed.
pub const ENABLE_CHALLENGE_TTL: Duration = Duration::minutes(5);
const MAX_ENABLE_CHALLENGES: usize = 64;

/// What the owner's device signs to enable a strategy again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnableChallenge {
    pub strategy_id: String,
    pub version: u32,
    pub config_sha256: [u8; 32],
    pub nonce: [u8; 32],
    pub expires_at: OffsetDateTime,
}

/// Open enable challenges, one per device, in memory only: a restart
/// forgets them. Each is used at most once.
#[derive(Default)]
pub struct EnableChallenges(HashMap<uuid::Uuid, EnableChallenge>);

impl EnableChallenges {
    /// Replace `device`'s challenge. `false` when too many devices hold one.
    pub fn issue(
        &mut self,
        device: uuid::Uuid,
        challenge: EnableChallenge,
        now: OffsetDateTime,
    ) -> bool {
        self.0.retain(|_, c| c.expires_at > now);
        if self.0.len() >= MAX_ENABLE_CHALLENGES && !self.0.contains_key(&device) {
            return false;
        }
        self.0.insert(device, challenge);
        true
    }

    /// Take `device`'s challenge, whatever happens next: it is returned only
    /// for `strategy_id` and before it expires.
    pub fn take(
        &mut self,
        device: uuid::Uuid,
        strategy_id: &str,
        now: OffsetDateTime,
    ) -> Option<EnableChallenge> {
        self.0
            .remove(&device)
            .filter(|c| c.strategy_id == strategy_id && c.expires_at > now)
    }
}

/// What the overview shows of Claude's part.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentStatus {
    pub settings: agent::AgentSettings,
    pub budget: agent::Budget,
    /// A decision or review call is running.
    pub calling: bool,
}

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
    /// Strategies the owner disabled since the tick read the strategies, so
    /// they stop entering within the tick. The stored state is authoritative.
    disabled_now: Mutex<HashSet<String>>,
    agent: RwLock<Option<AgentStatus>>,
    pub enable_challenges: Mutex<EnableChallenges>,
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

    /// Stop `strategy_id`'s entries at once (the owner disabled it).
    pub fn disable_now(&self, strategy_id: &str) {
        if let Ok(mut set) = self.disabled_now.lock() {
            set.insert(strategy_id.to_owned());
        }
    }

    /// The owner enabled `strategy_id` again.
    pub fn clear_disabled(&self, strategy_id: &str) {
        if let Ok(mut set) = self.disabled_now.lock() {
            set.remove(strategy_id);
        }
    }

    /// A poisoned lock counts as disabled.
    fn disabled(&self, strategy_id: &str) -> bool {
        self.disabled_now
            .lock()
            .map_or(true, |set| set.contains(strategy_id))
    }

    /// `None` while Claude takes no part.
    pub fn agent(&self) -> Option<AgentStatus> {
        self.agent.read().ok().and_then(|status| *status)
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

/// What the overview shows, for the whole paper book (every strategy).
#[derive(Debug, Clone)]
pub struct PaperView {
    /// Deprecated, kept for clients written for a single strategy: the
    /// active version of the first strategy by id (normally
    /// `a-survivor-momentum`), or seed A's version before any is listed. It
    /// does not scope anything else in the view.
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
    /// Poll new pump.fun migrations into `discovery`. A source without
    /// discovery finds nothing.
    async fn discover(
        &self,
        _discovery: &mut Discovery,
        _now: OffsetDateTime,
    ) -> Result<PollStats, MarketDataError> {
        Ok(PollStats::default())
    }
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
        let (mint_account, largest, pool) = tokio::join!(
            self.rpc.mint(mint),
            self.rpc.largest_accounts(mint),
            pools::read(&self.rpc, pair)
        );
        let (mint_account, largest) = (mint_account?, largest?);
        // A pool that cannot be read leaves the LP `Unknown` and its vaults in
        // the holder share: both reject.
        let pool = pool
            .map_err(|error| tracing::warn!(%error, %mint, "memecoin paper: pool not read"))
            .ok();
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
            // Also leaves the pool's vaults out of the holder share.
            lp: pool.as_ref(),
            sell_amount,
            sell_quote: quote.as_ref(),
        })
    }

    async fn discover(
        &self,
        discovery: &mut Discovery,
        now: OffsetDateTime,
    ) -> Result<PollStats, MarketDataError> {
        discovery.poll(&self.rpc, now).await
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
        ExitReason::TrailingStop => "trailing_stop",
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
        lp: None,
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
    pub discovered: usize,
    pub observed: usize,
    pub accepted: usize,
    pub entries: usize,
    pub exits: usize,
    pub failed_attempts: usize,
    pub unpriced_exits: usize,
    pub stuck: usize,
    pub provider_errors: usize,
}

/// What a recorded strategy version lets the paper loop do.
#[derive(Debug, Clone)]
enum Rules {
    /// A typed version that validates against the profile's filter rules:
    /// it enters while its strategy is enabled, with `filter` (the profile's
    /// rules tightened by the config).
    Config {
        config: Box<StrategyConfig>,
        filter: FilterRules,
    },
    /// Exits only: a legacy profile version, or a typed one that no longer
    /// validates against the profile's rules.
    ExitOnly(ExitRules),
    /// Nothing usable is recorded; its positions only get the time stop
    /// ([`TIME_STOP_ONLY`]).
    Unusable,
}

impl Rules {
    fn exit_rules(&self) -> Option<ExitRules> {
        match self {
            Rules::Config { config, .. } => Some(config.exit_rules()),
            Rules::ExitOnly(rules) => Some(*rules),
            Rules::Unusable => None,
        }
    }
}

/// A recorded strategy version. Versions never change, so the task keeps
/// them once read.
#[derive(Debug, Clone)]
struct Version {
    /// `None` for a legacy version.
    strategy_id: Option<String>,
    rules: Rules,
}

/// The exit side of a legacy version's recorded profile parameters.
#[derive(Deserialize)]
struct LegacyParams {
    strategy: LegacyExit,
}

#[derive(Deserialize)]
struct LegacyExit {
    take_profit_bps: u32,
    stop_loss_bps: u32,
    liquidity_drop_exit_bps: u32,
    max_hold_secs: u64,
}

fn parse_version(
    version: u32,
    strategy_id: Option<String>,
    params: &str,
    base: &FilterRules,
) -> Version {
    let rules = match serde_json::from_str::<StrategyConfig>(params) {
        Ok(config) if config.version == version && strategy_id.as_ref() == Some(&config.id) => {
            match config.validate(base) {
                Ok(filter) => Rules::Config {
                    config: Box::new(config),
                    filter,
                },
                Err(error) => {
                    tracing::warn!(version, %error, "memecoin paper: strategy version does not fit the profile; exits only");
                    Rules::ExitOnly(config.exit_rules())
                }
            }
        }
        Ok(_) => Rules::Unusable,
        Err(_) => match serde_json::from_str::<LegacyParams>(params) {
            Ok(LegacyParams { strategy: s }) => i64::try_from(s.max_hold_secs)
                .map(|secs| {
                    Rules::ExitOnly(ExitRules {
                        take_profit_bps: s.take_profit_bps,
                        stop_loss_bps: s.stop_loss_bps,
                        trailing_stop_bps: None,
                        liquidity_drop_exit_bps: s.liquidity_drop_exit_bps,
                        max_hold: Duration::seconds(secs),
                    })
                })
                .unwrap_or(Rules::Unusable),
            Err(_) => Rules::Unusable,
        },
    };
    Version { strategy_id, rules }
}

/// Per-strategy entry limits on top of the profile's global ones, counted
/// from the stored trips at the start of a tick.
struct Caps<'a> {
    /// Positions one strategy may hold: an equal share of the profile's
    /// `max_open_positions` among the enabled strategies, at least one.
    max_open: usize,
    today: Date,
    open: HashMap<&'a str, usize>,
    entries_today: HashMap<&'a str, u32>,
    last_exit: HashMap<(&'a str, &'a str), OffsetDateTime>,
}

impl<'a> Caps<'a> {
    fn new(
        trips: impl Iterator<Item = &'a Trip>,
        owner: impl Fn(u32) -> Option<&'a str>,
        max_open: usize,
        now: OffsetDateTime,
    ) -> Self {
        let mut caps = Caps {
            max_open,
            today: now.to_offset(UtcOffset::UTC).date(),
            open: HashMap::new(),
            entries_today: HashMap::new(),
            last_exit: HashMap::new(),
        };
        for trip in trips {
            let Some(id) = owner(trip.strategy_version) else {
                continue;
            };
            if trip.opened_at.to_offset(UtcOffset::UTC).date() == caps.today {
                *caps.entries_today.entry(id).or_default() += 1;
            }
            match &trip.close {
                None => *caps.open.entry(id).or_default() += 1,
                Some(close) => {
                    let last = caps
                        .last_exit
                        .entry((id, trip.mint.as_str()))
                        .or_insert(close.closed_at);
                    *last = (*last).max(close.closed_at);
                }
            }
        }
        caps
    }

    /// Whether strategy `id` may enter `mint` at `at`: below its share of
    /// open positions and its entries for the UTC day, and past its
    /// cooldown since it last left `mint`.
    fn allow(&self, id: &str, config: &StrategyConfig, mint: &str, at: OffsetDateTime) -> bool {
        let open = self.open.get(id).copied().unwrap_or(0);
        let entries = self.entries_today.get(id).copied().unwrap_or(0);
        let cooling = self
            .last_exit
            .get(&(id, mint))
            .is_some_and(|left| at - *left < config.token_cooldown());
        open < self.max_open && entries < config.max_entries_per_day && !cooling
    }

    fn entered(&mut self, id: &'a str) {
        *self.open.entry(id).or_default() += 1;
        *self.entries_today.entry(id).or_default() += 1;
    }
}

/// State the task keeps between ticks. Everything else is reloaded.
pub struct PaperTask {
    pub profile: PaperProfile,
    pub handle: Arc<PaperHandle>,
    /// Last observation per mint with the filter verdict per strategy
    /// version, to pace providers and to write a candidate row only when a
    /// verdict changes.
    seen: HashMap<String, (BTreeMap<u32, Option<&'static str>>, OffsetDateTime)>,
    last_gate_at: Option<OffsetDateTime>,
    /// `None` when the profile turns discovery off.
    discovery: Option<Discovery>,
    last_discovery_at: Option<OffsetDateTime>,
    /// Discovered mints already stored in the universe.
    universe_saved: HashSet<String>,
    versions: HashMap<u32, Version>,
    /// `None` while Claude takes no part.
    agent: Option<AgentState>,
}

/// Claude's part in the task.
struct AgentState {
    brain: Arc<dyn agent::Brain>,
    settings: agent::AgentSettings,
    events: Option<Hub>,
    /// Mints a strategy would enter, by mint, for the next decision.
    eligible: BTreeMap<String, agent::Eligible>,
    /// One call at a time: the worker has one background slot.
    in_flight: Option<InFlight>,
    /// Spacing kept in memory too, in case decision rows cannot be written.
    last_decision: Option<OffsetDateTime>,
    last_review: Option<OffsetDateTime>,
}

enum InFlight {
    Decision(
        agent::DecisionCall,
        tokio::task::JoinHandle<Result<ChatReply, LlmError>>,
    ),
    Review(tokio::task::JoinHandle<()>),
}

/// Up to [`MAX_ENTRY_CANDIDATES_PER_TICK`] mints to observe: not held, not
/// observed within [`OBSERVE_COOLDOWN`], watchlist before discovered mints,
/// then least recently observed first.
fn entry_candidates<'a, T>(
    watchlist: &'a [String],
    discovered: impl Iterator<Item = &'a String>,
    held: &BTreeSet<String>,
    seen: &HashMap<String, (T, OffsetDateTime)>,
    now: OffsetDateTime,
) -> Vec<&'a String> {
    let mut due: Vec<(bool, &String)> = watchlist
        .iter()
        .map(|m| (false, m))
        .chain(
            discovered
                .filter(|m| !watchlist.contains(m))
                .map(|m| (true, m)),
        )
        .filter(|(_, m)| !held.contains(*m))
        .filter(|(_, m)| {
            seen.get(*m)
                .is_none_or(|(_, at)| now - *at >= OBSERVE_COOLDOWN)
        })
        .collect();
    due.sort_by_key(|(discovered, m)| (*discovered, seen.get(*m).map(|(_, at)| *at)));
    due.truncate(MAX_ENTRY_CANDIDATES_PER_TICK);
    due.into_iter().map(|(_, m)| m).collect()
}

/// The whole stored paper book, checked to replay exactly.
struct Book {
    trips: Vec<Trip>,
    costs: Vec<Cost>,
    ledger: Ledger,
}

impl Book {
    /// The ledger of one strategy version alone.
    fn version_ledger(&self, version: u32) -> Result<Ledger, &'static str> {
        let trips: Vec<&Trip> = self
            .trips
            .iter()
            .filter(|t| t.strategy_version == version)
            .collect();
        let costs: Vec<&Cost> = self
            .costs
            .iter()
            .filter(|c| c.strategy_version == version)
            .collect();
        replay(&trips, &costs).map_err(|_| "paper ledger does not replay")
    }
}

async fn load_book(db: &Database) -> Result<Book, &'static str> {
    let trips = bounded("trips", store::load_trips(db)).await?;
    let costs = bounded("costs", store::load_costs(db)).await?;
    // Every stored result must recompute, whatever its version.
    let ledger = replay(
        &trips.iter().collect::<Vec<_>>(),
        &costs.iter().collect::<Vec<_>>(),
    )
    .map_err(|error| {
        tracing::error!(?error, "memecoin paper ledger does not replay");
        "paper ledger does not replay"
    })?;
    Ok(Book {
        trips,
        costs,
        ledger,
    })
}

/// Price and liquidity of each of `mints` with a main pool in `pairs`.
fn marks_of(pairs: &[Pair], mints: &[&str], now: OffsetDateTime) -> BTreeMap<String, Mark> {
    mints
        .iter()
        .filter_map(|mint| {
            let snapshot = main_pair(pairs, mint).and_then(|pair| pair_snapshot(pair, now))?;
            Some((
                (*mint).to_owned(),
                Mark {
                    price_usd: snapshot.price_usd,
                    liquidity_usd: snapshot.liquidity_usd,
                },
            ))
        })
        .collect()
}

/// Store what was seen of each mint this tick, for the signals. Best
/// effort: a lost observation only thins a series.
async fn record_observations(db: &Database, marks: &BTreeMap<String, Mark>, now: OffsetDateTime) {
    let _ = bounded("observations", async {
        for (mint, mark) in marks {
            let observation = store::Observation {
                mint: mint.clone(),
                at: now,
                price_usd: mark.price_usd,
                liquidity_usd: mark.liquidity_usd,
            };
            store::record_observation(db, &observation).await?;
        }
        Ok(())
    })
    .await;
}

/// The recent series of `mint`, ending with `snapshot`, which is what an
/// entry would fill at.
async fn load_series(
    db: &Database,
    mint: &str,
    snapshot: &TokenSnapshot,
) -> Result<Series, &'static str> {
    let at = snapshot.observed_at;
    let stored = store::load_series(db, mint, at, store::MAX_SERIES_POINTS);
    let mut points: Vec<SeriesPoint> = bounded("series", stored)
        .await?
        .into_iter()
        .map(|o| SeriesPoint {
            at: o.at,
            price_usd: o.price_usd,
            liquidity_usd: o.liquidity_usd,
        })
        .collect();
    if points.last().is_none_or(|last| last.at < at) {
        points.push(SeriesPoint {
            at,
            price_usd: snapshot.price_usd,
            liquidity_usd: snapshot.liquidity_usd,
        });
    }
    Ok(Series {
        pool_created_at: snapshot.pool_created_at,
        points,
    })
}

/// Store the discovered mints not stored yet, so a restart keeps the
/// discovery window.
async fn save_universe(db: &Database, discovery: &Discovery, saved: &mut HashSet<String>) {
    let new: Vec<store::UniverseEntry> = discovery
        .migrations()
        .filter(|m| !saved.contains(&m.mint) && is_mint(&m.mint) && is_mint(&m.pool))
        .map(|m| store::UniverseEntry {
            mint: m.mint.clone(),
            pool: m.pool.clone(),
            program: pools::PUMPSWAP_PROGRAM.to_owned(),
            migrated_at: m.migrated_at,
        })
        .collect();
    if new.is_empty() {
        return;
    }
    let stored = bounded("universe", async {
        for entry in &new {
            store::upsert_universe(db, entry).await?;
        }
        Ok(())
    })
    .await;
    if stored.is_ok() {
        saved.extend(new.into_iter().map(|entry| entry.mint));
    }
    saved.retain(|mint| discovery.knows(mint));
}

/// Load the stored universe of the discovery window into `discovery`.
async fn restore_universe(
    db: &Database,
    discovery: &mut Discovery,
    saved: &mut HashSet<String>,
    now: OffsetDateTime,
) {
    let stored = store::load_universe(db, now - discovery::MAX_AGE, discovery::MAX_MINTS);
    let Ok(entries) = bounded("universe", stored).await else {
        return;
    };
    for entry in entries {
        let migration = Migration {
            mint: entry.mint,
            pool: entry.pool,
            migrated_at: entry.migrated_at,
        };
        let mint = migration.mint.clone();
        if discovery.remember(migration, now) {
            saved.insert(mint);
        }
    }
}

impl PaperTask {
    pub fn new(profile: PaperProfile, handle: Arc<PaperHandle>) -> Self {
        Self {
            discovery: profile.discovery.then(Discovery::new),
            profile,
            handle,
            seen: HashMap::new(),
            last_gate_at: None,
            last_discovery_at: None,
            universe_saved: HashSet::new(),
            versions: HashMap::new(),
            agent: None,
        }
    }

    /// Let Claude decide entries and exits and review the strategies, when
    /// the profile allows it.
    pub fn with_agent(mut self, brain: Option<Arc<dyn agent::Brain>>, events: Option<Hub>) -> Self {
        self.agent = self
            .profile
            .agent
            .zip(brain)
            .map(|(settings, brain)| AgentState {
                brain,
                settings,
                events,
                eligible: BTreeMap::new(),
                in_flight: None,
                last_decision: None,
                last_review: None,
            });
        self
    }

    /// Read the strategy versions in `needed` that are not known yet.
    async fn load_versions(
        &mut self,
        db: &Database,
        needed: &BTreeSet<u32>,
    ) -> Result<(), &'static str> {
        for &version in needed {
            if self.versions.contains_key(&version) {
                continue;
            }
            let stored = bounded("strategy version", store::version_params(db, version)).await?;
            let parsed = match stored {
                Some((strategy_id, params)) => {
                    parse_version(version, strategy_id, &params, &self.profile.rules)
                }
                None => Version {
                    strategy_id: None,
                    rules: Rules::Unusable,
                },
            };
            self.versions.insert(version, parsed);
        }
        Ok(())
    }

    /// One tick. `Err` stops the tick; nothing is simulated without its
    /// write having succeeded first. Exits need only the held positions and
    /// their versions, so a problem with the stored history or the strategy
    /// list stops entries and the report, never exits.
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
        let book = load_book(db).await;
        let strategies = bounded("strategies", store::list_strategies(db)).await;
        let needed: BTreeSet<u32> = held
            .iter()
            .map(|t| t.strategy_version)
            .chain(
                book.iter()
                    .flat_map(|b| b.trips.iter().map(|t| t.strategy_version)),
            )
            .chain(strategies.iter().flatten().map(|s| s.active_version))
            .collect();
        self.load_versions(db, &needed).await?;
        let p = &self.profile;
        let versions = &self.versions;
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
        let mut pairs = match source.pairs(&mints).await {
            Ok(pairs) => pairs,
            Err(error) => {
                stats.provider_errors += 1;
                tracing::warn!(%error, "memecoin paper: no market data this tick");
                Vec::new()
            }
        };
        let now = OffsetDateTime::now_utc();
        let mut marks = marks_of(&pairs, &mints, now);

        let mut unusable = 0;
        // Exits first, for every held position, by the rules of the version
        // that opened it, also while paused or its strategy is disabled. A
        // position whose exit failed EXIT_FAILURE_CAP times is stuck.
        for trip in held.iter_mut().filter(|t| !t.stuck) {
            let Some(mark) = marks.get(&trip.mint).copied() else {
                continue;
            };
            let known = versions
                .get(&trip.strategy_version)
                .and_then(|v| v.rules.exit_rules());
            if known.is_none() {
                unusable += 1;
            }
            let rules = known.unwrap_or(TIME_STOP_ONLY);
            let held_position = position(trip);
            // Without rules of its own only the time stop applies.
            let reason = strategy::exit_reason(
                &held_position,
                trip.peak_value_usd,
                known.and(Some(mark)),
                &rules,
                &p.costs,
                now,
            );
            let Some(reason) = reason else {
                let peak =
                    strategy::track_peak(trip.peak_value_usd, &held_position, mark, &p.costs);
                if let Some(peak) = peak.filter(|_| peak > trip.peak_value_usd) {
                    // Best effort: a lost peak only lets the trail start lower.
                    match bounded("peak value", store::set_peak_value(db, &trip.id, peak)).await {
                        Ok(true) => trip.peak_value_usd = Some(peak),
                        Ok(false) => {
                            tracing::warn!(mint = %trip.mint, "memecoin paper: peak of a trip that is not open")
                        }
                        Err(_) => {}
                    }
                }
                continue;
            };
            match paper::sell(
                trip.tokens,
                mark.price_usd,
                mark.liquidity_usd,
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

        if unusable > 0 {
            tracing::error!(
                positions = unusable,
                "memecoin paper: positions without usable strategy rules; only the time stop applies"
            );
        }

        // Strategies that may enter: enabled, with an active version that
        // fits the profile. Ordered by id, which is also their priority.
        let enabled: Vec<(&str, u32, &StrategyConfig, &FilterRules)> = strategies
            .iter()
            .flatten()
            .filter(|s| s.enabled)
            .filter_map(|s| match versions.get(&s.active_version) {
                Some(Version {
                    strategy_id: Some(owner),
                    rules: Rules::Config { config, filter },
                }) if *owner == s.id => Some((s.id.as_str(), s.active_version, &**config, filter)),
                _ => None,
            })
            .collect();
        let entries_possible = book.is_ok() && !enabled.is_empty();
        let mut open = held.iter().filter(|t| t.close.is_none()).count();
        let per_strategy = (p.max_open_positions / enabled.len().max(1)).max(1);
        let owner = |version: u32| {
            versions
                .get(&version)
                .and_then(|v| v.strategy_id.as_deref())
        };
        let mut caps = Caps::new(
            book.iter().flat_map(|b| b.trips.iter()),
            owner,
            per_strategy,
            now,
        );

        // A decision Claude finished is carried out first, while the tick
        // has most of its time left.
        if let Some(agent) = self.agent.as_mut() {
            let mut env = AgentEnv {
                db,
                source,
                profile: p,
                handle: &self.handle,
                versions,
                enabled: &enabled,
                caps: &mut caps,
                held: &mut held,
                open: &mut open,
                marks: &marks,
                roll: &mut *roll,
                stamp: &mut stamp,
                stats: &mut stats,
                read_failed,
                book_ok: book.is_ok(),
            };
            agent_apply(agent, &mut env).await?;
        }

        // New candidates from pump.fun migrations, only when entries can
        // follow.
        let poll_due = self
            .last_discovery_at
            .is_none_or(|at| now - at >= DISCOVERY_EVERY);
        if let Some(discovery) = self.discovery.as_mut() {
            if poll_due && entries_possible && !paused() {
                self.last_discovery_at = Some(now);
                let poll = source.discover(discovery, now);
                match tokio::time::timeout(DISCOVERY_TIMEOUT, poll).await {
                    Ok(Ok(poll)) => {
                        stats.discovered = poll.found;
                        if poll.gap {
                            tracing::warn!("memecoin paper: discovery skipped older migrations");
                        }
                    }
                    Ok(Err(error)) => {
                        stats.provider_errors += 1;
                        tracing::warn!(%error, "memecoin paper: discovery poll failed");
                    }
                    Err(_) => tracing::warn!("memecoin paper: discovery poll timed out"),
                }
                save_universe(db, discovery, &mut self.universe_saved).await;
            }
        }

        // Entries need an intact history and an enabled strategy: a few
        // watchlist or discovered mints not held at the start of the tick.
        let discovery = self.discovery.as_ref();
        self.seen
            .retain(|m, _| p.watchlist.contains(m) || discovery.is_some_and(|d| d.knows(m)));
        let discovered = discovery
            .into_iter()
            .flat_map(|d| d.due(now))
            .map(|migration| &migration.mint);
        let mut due = entry_candidates(&p.watchlist, discovered, &held_mints, &self.seen, now);
        if !entries_possible {
            due.clear();
        }
        // Discovered mints were not in the first DexScreener call.
        let extra: Vec<&str> = due
            .iter()
            .map(|m| m.as_str())
            .filter(|m| !mints.contains(m))
            .collect();
        if !extra.is_empty() {
            match source.pairs(&extra).await {
                Ok(more) => {
                    marks.extend(marks_of(&more, &extra, now));
                    pairs.extend(more);
                }
                Err(error) => {
                    stats.provider_errors += 1;
                    tracing::warn!(%error, "memecoin paper: no market data for discovered mints");
                }
            }
        }
        record_observations(db, &marks, now).await;

        let deny_list = BTreeSet::new();
        'mints: for mint in due {
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
            // The hard filter, with each strategy's tightened rules.
            let verdicts: BTreeMap<u32, Option<&'static str>> = enabled
                .iter()
                .map(|(_, version, _, rules)| {
                    let reason = match &observed {
                        None => Some("data_unavailable"),
                        Some(snapshot) => match filter::check(snapshot, rules, &deny_list, at) {
                            Verdict::Accept => None,
                            Verdict::Reject(reason) => Some(reject_reason(reason)),
                        },
                    };
                    (*version, reason)
                })
                .collect();
            let before = self.seen.get(mint).map(|(verdicts, _)| verdicts);
            for (&version, &reason) in &verdicts {
                if before.is_some_and(|b| b.get(&version) == Some(&reason)) {
                    continue;
                }
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
            let accepting: Vec<_> = enabled
                .iter()
                .filter(|(_, version, ..)| verdicts.get(version) == Some(&None))
                .collect();
            self.seen.insert(mint.clone(), (verdicts, at));
            if let Some(agent) = self.agent.as_mut() {
                agent.eligible.remove(mint.as_str());
            }
            let Some(snapshot) = observed.filter(|_| !accepting.is_empty()) else {
                continue;
            };
            stats.accepted += 1;
            let Ok(series) = load_series(db, mint, &snapshot).await else {
                continue;
            };
            // The first strategy whose limits and signal allow it enters.
            for &&(id, version, config, _) in &accepting {
                if !caps.allow(id, config, mint, at)
                    || !config.entry_signal(&series, at)
                    || self.handle.disabled(id)
                {
                    continue;
                }
                let Some(size) = config.entry_size(snapshot.liquidity_usd, p.max_position_usd)
                else {
                    continue;
                };
                if let Some(agent) = self.agent.as_mut() {
                    // Claude decides whether and how much to enter.
                    let signals = SignalId::ALL
                        .into_iter()
                        .filter_map(|signal| Some((signal, signals::compute(signal, &series, at)?)))
                        .collect();
                    let eligible = agent::Eligible {
                        mint: mint.clone(),
                        strategy_id: id.to_owned(),
                        version,
                        liquidity_usd: snapshot.liquidity_usd,
                        sell_impact_bps: snapshot.sell_impact_bps,
                        signals,
                        seen_at: at,
                    };
                    agent.eligible.insert(mint.clone(), eligible);
                    break;
                }
                // No research step runs yet; it could only veto or shrink.
                let size = apply_research(size, ResearchAdjustment::Keep);
                if paused() {
                    break 'mints;
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
                            peak_value_usd: None,
                            close: None,
                        };
                        bounded("open trip", store::open_trip(db, &trip)).await?;
                        held.push(trip);
                        open += 1;
                        caps.entered(id);
                        stats.entries += 1;
                    }
                    Some(Fill::Failed { fees_usd }) => {
                        let cost =
                            store::add_cost(db, CostKind::FailedTx, version, fees_usd, stamp());
                        bounded("add cost", cost).await?;
                        stats.failed_attempts += 1;
                    }
                    None => {}
                }
                break;
            }
        }

        if let Some(agent) = self.agent.as_mut() {
            let mut env = AgentEnv {
                db,
                source,
                profile: p,
                handle: &self.handle,
                versions,
                enabled: &enabled,
                caps: &mut caps,
                held: &mut held,
                open: &mut open,
                marks: &marks,
                roll: &mut *roll,
                stamp: &mut stamp,
                stats: &mut stats,
                read_failed,
                book_ok: book.is_ok(),
            };
            agent_schedule(agent, &mut env, book.as_ref().ok()).await?;
        }

        // Report on the whole paper book, from the stored ledger.
        book?;
        // ponytail: reloads the whole ledger every tick; keep it in memory if it grows large.
        let book = load_book(db).await?;
        let rules = GateRules {
            min_closed_trades: gate::MIN_CLOSED_TRADES,
            min_duration: gate::MIN_DURATION,
            min_trading_days: gate::MIN_TRADING_DAYS,
        };
        let report = gate::evaluate(&book.ledger, &marks, &p.costs, &rules)
            .map_err(|_| "paper gate cannot be computed")?;
        let pnl = book
            .ledger
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
            strategy_version: strategies
                .iter()
                .flatten()
                .next()
                .map_or(STRATEGY_VERSION_FLOOR + 1, |s| s.active_version),
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
        // An hourly gate snapshot per strategy, of its active version.
        let snapshot_due = self
            .last_gate_at
            .is_none_or(|at| now - at >= GATE_SNAPSHOT_EVERY);
        if let (true, Ok(strategies)) = (snapshot_due, &strategies) {
            // Hourly even if one fails: a failed write is logged and skipped.
            self.last_gate_at = Some(now);
            for version in strategies.iter().map(|s| s.active_version) {
                let Some(report) = book
                    .version_ledger(version)
                    .ok()
                    .and_then(|ledger| gate::evaluate(&ledger, &marks, &p.costs, &rules).ok())
                else {
                    tracing::warn!(version, "memecoin paper: gate snapshot cannot be computed");
                    continue;
                };
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
                let _ = bounded("gate snapshot", store::insert_gate_snapshot(db, &snapshot)).await;
            }
        }
        Ok(stats)
    }
}

/// What Claude's step of a tick works on: the tick's own state.
struct AgentEnv<'t, 'a> {
    db: &'t Database,
    source: &'t dyn MarketSource,
    profile: &'t PaperProfile,
    handle: &'t PaperHandle,
    versions: &'t HashMap<u32, Version>,
    enabled: &'t [(&'a str, u32, &'a StrategyConfig, &'a FilterRules)],
    caps: &'t mut Caps<'a>,
    held: &'t mut Vec<Trip>,
    open: &'t mut usize,
    marks: &'t BTreeMap<String, Mark>,
    roll: &'t mut (dyn FnMut() -> Decimal + Send),
    stamp: &'t mut (dyn FnMut() -> OffsetDateTime + Send),
    stats: &'t mut TickStats,
    read_failed: bool,
    /// The stored ledger replays; without it the limits cannot be counted.
    book_ok: bool,
}

impl AgentEnv<'_, '_> {
    fn paused(&self) -> bool {
        self.read_failed || self.handle.paused()
    }
}

/// Carry out a decision call that finished, and forget a finished review.
/// Code exits have already run this tick, whatever Claude does.
async fn agent_apply(
    agent: &mut AgentState,
    env: &mut AgentEnv<'_, '_>,
) -> Result<(), &'static str> {
    match agent.in_flight.take() {
        Some(InFlight::Decision(call, reply)) if reply.is_finished() => {
            let reply = reply.await.unwrap_or_else(|_| {
                Err(LlmError::Api {
                    status: 503,
                    body: String::new(),
                })
            });
            apply_decision(agent, env, call, reply).await?;
        }
        Some(InFlight::Review(review)) if review.is_finished() => {}
        running => agent.in_flight = running,
    }
    Ok(())
}

/// Start the next call when it is due.
async fn agent_schedule(
    agent: &mut AgentState,
    env: &mut AgentEnv<'_, '_>,
    book: Option<&Book>,
) -> Result<(), &'static str> {
    let now = OffsetDateTime::now_utc();
    let held = &*env.held;
    agent.eligible.retain(|mint, e| {
        now - e.seen_at < agent::ELIGIBLE_TTL
            && !held.iter().any(|t| t.close.is_none() && t.mint == *mint)
    });
    let rows = bounded(
        "agent calls",
        store::recent_calls(env.db, now - Duration::days(1)),
    )
    .await;
    let budget = rows
        .ok()
        .map(|rows| agent::budget(&rows, &agent.settings, now));
    if let Ok(mut slot) = env.handle.agent.write() {
        *slot = Some(AgentStatus {
            settings: agent.settings,
            budget: budget.unwrap_or_default(),
            calling: agent.in_flight.is_some(),
        });
    }
    let (Some(budget), Some(book)) = (budget, book) else {
        return Ok(());
    };
    if agent.in_flight.is_some() || env.paused() {
        return Ok(());
    }
    let spaced = |last: Option<OffsetDateTime>, gap| last.is_none_or(|at| now - at >= gap);
    if budget.review_open && spaced(agent.last_review, agent::REVIEW_RETRY) {
        agent.last_review = Some(now);
        let p = env.profile;
        let review = agent::ReviewEnv {
            db: env.db.clone(),
            brain: agent.brain.clone(),
            rules: p.rules.clone(),
            costs: p.costs.clone(),
            stake_usd: p.stake_usd,
            max_position_usd: p.max_position_usd,
            settings: agent.settings,
            events: agent.events.clone(),
        };
        agent.in_flight = Some(InFlight::Review(tokio::spawn(agent::run_review(review))));
        return Ok(());
    }
    if !budget.decision_open || !spaced(agent.last_decision, agent::DECISION_INTERVAL) {
        return Ok(());
    }
    let Some(call) = decision_input(agent, env, book, now).await else {
        return Ok(());
    };
    agent.last_decision = Some(now);
    // Recorded before the call, so a call cut off by a restart still counts.
    if !agent::CallRecord::new(call.call_id.clone(), None)
        .start(env.db)
        .await
    {
        return Ok(());
    }
    let brain = agent.brain.clone();
    let input = call.input.clone();
    let reply = tokio::spawn(async move { brain.run(WorkerTask::MemeDecision, &input).await });
    agent.in_flight = Some(InFlight::Decision(call, reply));
    Ok(())
}

/// The next decision's input: eligible candidates (newest first), open
/// positions with a price, and the strategies they belong to.
async fn decision_input(
    agent: &AgentState,
    env: &mut AgentEnv<'_, '_>,
    book: &Book,
    now: OffsetDateTime,
) -> Option<agent::DecisionCall> {
    let p = env.profile;
    let mut candidates: Vec<agent::Eligible> = agent.eligible.values().cloned().collect();
    candidates.sort_by_key(|c| std::cmp::Reverse(c.seen_at));
    let owner = |version: u32| {
        env.versions
            .get(&version)
            .and_then(|v| v.strategy_id.clone())
    };
    let positions: Vec<agent::PositionFacts> = env
        .held
        .iter()
        .filter(|t| t.close.is_none() && !t.stuck)
        .filter_map(|t| {
            let mark = env.marks.get(&t.mint)?;
            let value = paper::sell_value(t.tokens, mark.price_usd, mark.liquidity_usd, &p.costs)?;
            Some(agent::PositionFacts::new(
                t.id.clone(),
                owner(t.strategy_version),
                t.cost_usd,
                value,
                t.peak_value_usd,
                now - t.opened_at,
            ))
        })
        .collect();
    if candidates.is_empty() && positions.is_empty() {
        return None;
    }
    let ids: BTreeSet<String> = env
        .enabled
        .iter()
        .map(|e| e.0.to_owned())
        .chain(positions.iter().filter_map(|p| p.strategy_id.clone()))
        .collect();
    let mut strategies = Vec::with_capacity(ids.len());
    for id in ids {
        let performance = store::strategy_performance(env.db, &id);
        let performance = bounded("performance", performance).await.ok()?;
        let config = env.enabled.iter().find(|e| e.0 == id).map(|e| e.2);
        strategies.push(agent::StrategyFacts {
            open: env.caps.open.get(id.as_str()).copied().unwrap_or(0),
            max_open: env.caps.max_open,
            entries_today: env
                .caps
                .entries_today
                .get(id.as_str())
                .copied()
                .unwrap_or(0),
            max_entries_per_day: config.map_or(0, |c| c.max_entries_per_day),
            results: agent::Results::of(&performance, p.stake_usd),
            id,
        });
    }
    let max_open = p.max_open_positions.max(1);
    let free_slots = max_open.saturating_sub(*env.open);
    let free_slots_bps = i64::try_from(free_slots * 10_000 / max_open).unwrap_or(0);
    let net = book.ledger.pnl(&BTreeMap::new(), &p.costs).ok()?.net_usd;
    let free_stake = p.stake_usd.checked_add(net)?.max(Decimal::ZERO);
    let free_stake_bps = agent::bps_of(free_stake, p.stake_usd);
    agent::decision_call(
        candidates,
        positions,
        &strategies,
        free_slots_bps,
        free_stake_bps,
    )
}

/// Record and carry out a finished decision call. Every action is checked
/// again here; a refused one is recorded as `skip` with the reason.
async fn apply_decision(
    agent: &AgentState,
    env: &mut AgentEnv<'_, '_>,
    call: agent::DecisionCall,
    reply: Result<ChatReply, LlmError>,
) -> Result<(), &'static str> {
    let events = agent.events.as_ref();
    let reply = match reply {
        Ok(reply) => reply,
        Err(error) => {
            // A busy or absent worker only skips this cycle.
            tracing::info!(%error, "memecoin decision call failed");
            agent::CallRecord::new(call.call_id, None)
                .failed(env.db, &error, events)
                .await;
            return Ok(());
        }
    };
    let mut record = agent::CallRecord::new(call.call_id.clone(), Some(&reply));
    let parsed = agent::parse_decision(&reply.text, call.candidates.len(), call.positions.len());
    let decision = match parsed {
        Ok(decision) => decision,
        Err(reason) => {
            tracing::warn!(reason, "memecoin decision reply dropped");
            let row = record.row(DecisionKind::InvalidOutput, None, None, None, Some(reason));
            record.write(env.db, row, events).await;
            return Ok(());
        }
    };
    record.rationale = decision.rationale;
    let mut choices = decision.choices;
    // Exits first: they free slots and stake.
    choices.sort_by_key(|c| !matches!(c, agent::Choice::Exit { .. }));
    let mut free_stake = None;
    let deadline = tokio::time::Instant::now() + AGENT_APPLY_DEADLINE;
    for choice in choices {
        let row = match choice {
            agent::Choice::Hold => record.row(DecisionKind::Hold, None, None, None, None),
            agent::Choice::Exit { position } => {
                agent_exit(env, &call.positions[position], &mut record).await?
            }
            agent::Choice::Enter {
                candidate,
                size_bps,
                reason_code,
            } => {
                let candidate = &call.candidates[candidate];
                let entry = if tokio::time::Instant::now() >= deadline {
                    Err("apply_deadline")
                } else {
                    agent_enter(env, candidate, size_bps, &mut free_stake).await?
                };
                match entry {
                    Ok(()) => record.row(
                        DecisionKind::Enter,
                        Some(candidate.version),
                        Some(&candidate.mint),
                        Some(size_bps),
                        Some(&reason_code),
                    ),
                    Err(reason) => record.row(
                        DecisionKind::Skip,
                        Some(candidate.version),
                        Some(&candidate.mint),
                        None,
                        Some(reason),
                    ),
                }
            }
        };
        record.write(env.db, row, events).await;
    }
    Ok(())
}

/// Sell a position Claude chose to exit, at the tick's mark.
async fn agent_exit(
    env: &mut AgentEnv<'_, '_>,
    trip_id: &str,
    record: &mut agent::CallRecord,
) -> Result<store::Decision, &'static str> {
    let costs = &env.profile.costs;
    let Some(trip) = env
        .held
        .iter_mut()
        .find(|t| t.id == trip_id && t.close.is_none() && !t.stuck)
    else {
        return Ok(record.row(
            DecisionKind::Skip,
            None,
            None,
            None,
            Some("position_closed"),
        ));
    };
    let (version, mint) = (trip.strategy_version, trip.mint.clone());
    let mut skip = |reason| {
        Ok(record.row(
            DecisionKind::Skip,
            Some(version),
            Some(&mint),
            None,
            Some(reason),
        ))
    };
    let Some(mark) = env.marks.get(&trip.mint).copied() else {
        return skip("no_price");
    };
    match paper::sell(
        trip.tokens,
        mark.price_usd,
        mark.liquidity_usd,
        costs,
        (env.roll)(),
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
                closed_at: (env.stamp)(),
                usd_out: amount_out,
                exit_fees_usd: fees_usd,
                net_pnl_usd,
                exit_reason: "agent_exit",
            };
            if !bounded("close trip", store::close_trip(env.db, &trip.id, &close)).await? {
                return Err("paper trip was not open");
            }
            trip.close = Some(close);
            *env.open = env.open.saturating_sub(1);
            env.stats.exits += 1;
            Ok(record.row(DecisionKind::Exit, Some(version), Some(&mint), None, None))
        }
        Some(Fill::Failed { fees_usd }) => {
            let cost =
                store::add_cost(env.db, CostKind::FailedTx, version, fees_usd, (env.stamp)());
            bounded("add cost", cost).await?;
            env.stats.failed_attempts += 1;
            skip("exit_failed")
        }
        None => skip("no_price"),
    }
}

/// Enter a candidate Claude chose, if it still passes every check on fresh
/// data: its strategy enabled at the same version, the global and the
/// strategy's limits, and the hard filter. The size is `size_bps` of what
/// the strategy would enter, within the free paper stake. `Ok(Err(reason))`
/// when refused.
async fn agent_enter(
    env: &mut AgentEnv<'_, '_>,
    candidate: &agent::Eligible,
    size_bps: u32,
    free_stake: &mut Option<Decimal>,
) -> Result<Result<(), &'static str>, &'static str> {
    let p = env.profile;
    let mint = candidate.mint.as_str();
    let strategy =
        env.enabled.iter().copied().find(|(id, version, ..)| {
            *id == candidate.strategy_id && *version == candidate.version
        });
    let Some((id, version, config, rules)) = strategy else {
        return Ok(Err("strategy_changed"));
    };
    let now = OffsetDateTime::now_utc();
    let refusal = if !env.book_ok {
        // The limits are counted from the stored ledger.
        Some("book_unavailable")
    } else if env.paused() {
        Some("paused")
    } else if env.handle.disabled(id) {
        Some("strategy_disabled")
    } else if env.held.iter().any(|t| t.close.is_none() && t.mint == mint) {
        Some("already_held")
    } else if *env.open >= p.max_open_positions {
        Some("no_slot")
    } else if !env.caps.allow(id, config, mint, now) {
        Some("strategy_limit")
    } else {
        None
    };
    if let Some(reason) = refusal {
        return Ok(Err(reason));
    }
    let Some(snapshot) = fresh_snapshot(env, mint, now).await else {
        return Ok(Err("data_unavailable"));
    };
    let at = OffsetDateTime::now_utc();
    if let Verdict::Reject(reason) = filter::check(&snapshot, rules, &BTreeSet::new(), at) {
        return Ok(Err(reject_reason(reason)));
    }
    let free = match *free_stake {
        Some(free) => free,
        None => {
            let net = match load_book(env.db).await {
                Ok(book) => book.ledger.pnl(&BTreeMap::new(), &p.costs).ok(),
                Err(_) => None,
            };
            let free = net
                .and_then(|pnl| p.stake_usd.checked_add(pnl.net_usd))
                .unwrap_or(Decimal::ZERO);
            *free_stake = Some(free);
            free
        }
    };
    let size = config
        .entry_size(snapshot.liquidity_usd, p.max_position_usd)
        .and_then(|max| max.checked_mul(Decimal::from(size_bps)))
        .map(|size| (size / Decimal::from(10_000)).min(free))
        .map(|size| size.round_dp_with_strategy(2, RoundingStrategy::ToZero))
        .filter(|size| *size > Decimal::ZERO);
    let Some(size) = size else {
        return Ok(Err("no_stake"));
    };
    if env.paused() || env.handle.disabled(id) {
        return Ok(Err("paused"));
    }
    match paper::buy(
        size,
        snapshot.price_usd,
        snapshot.liquidity_usd,
        &p.costs,
        (env.roll)(),
    ) {
        Some(Fill::Filled {
            amount_out,
            fees_usd,
        }) => {
            let cost_usd = size.checked_add(fees_usd).ok_or("paper entry overflow")?;
            let trip = Trip {
                id: uuid::Uuid::now_v7().to_string(),
                mint: mint.to_owned(),
                strategy_version: version,
                tokens: amount_out,
                cost_usd,
                entry_liquidity_usd: snapshot.liquidity_usd,
                opened_at: (env.stamp)(),
                exit_failures: 0,
                stuck: false,
                peak_value_usd: None,
                close: None,
            };
            bounded("open trip", store::open_trip(env.db, &trip)).await?;
            env.held.push(trip);
            *env.open += 1;
            env.caps.entered(id);
            env.stats.entries += 1;
            *free_stake = Some(free - cost_usd);
            Ok(Ok(()))
        }
        Some(Fill::Failed { fees_usd }) => {
            let cost =
                store::add_cost(env.db, CostKind::FailedTx, version, fees_usd, (env.stamp)());
            bounded("add cost", cost).await?;
            env.stats.failed_attempts += 1;
            Ok(Err("entry_failed"))
        }
        None => Ok(Err("unpriced")),
    }
}

/// A full entry observation of `mint`, taken now.
async fn fresh_snapshot(
    env: &mut AgentEnv<'_, '_>,
    mint: &str,
    now: OffsetDateTime,
) -> Option<TokenSnapshot> {
    let observe = async {
        let pairs = env.source.pairs(&[mint]).await?;
        match main_pair(&pairs, mint) {
            Some(pair) => env
                .source
                .entry_snapshot(pair, env.profile.max_position_usd, now)
                .await
                .map(Some),
            None => Ok(None),
        }
    };
    match tokio::time::timeout(AGENT_ENTRY_OBSERVE_TIMEOUT, observe).await {
        Ok(Ok(snapshot)) => snapshot,
        Ok(Err(error)) => {
            env.stats.provider_errors += 1;
            tracing::warn!(%error, %mint, "memecoin paper: Claude's entry not observed");
            None
        }
        Err(_) => {
            tracing::warn!(%mint, "memecoin paper: Claude's entry observation timed out");
            None
        }
    }
}

/// Record the seed strategies A, B and C with versions above
/// [`STRATEGY_VERSION_FLOOR`] and enable each new one. Idempotent: an
/// existing strategy keeps its active version and enabled state. A seed that
/// would loosen the profile's filter rules, or whose version is recorded
/// with other content, is left out and logged; the others still run.
pub async fn seed_strategies(db: &Database, base: &FilterRules) -> Result<(), &'static str> {
    for (index, mut config) in seeds().into_iter().enumerate() {
        let index = u32::try_from(index).map_err(|_| "too many seeds")?;
        config.version = STRATEGY_VERSION_FLOOR + 1 + index;
        if let Err(error) = config.validate(base) {
            tracing::warn!(strategy = %config.id, %error, "memecoin paper: seed strategy left out; it would loosen the profile");
            continue;
        }
        let params =
            serde_json::to_string(&config).map_err(|_| "seed strategy cannot be recorded")?;
        let version = StrategyVersion {
            version: config.version,
            strategy_id: config.id.clone(),
            family: config.family.into(),
            parent_version: None,
            author: StrategyAuthor::Seed,
            params,
            rationale: None,
            backtest: None,
            created_at: OffsetDateTime::now_utc(),
        };
        match tokio::time::timeout(DB_TIMEOUT, store::insert_strategy_version(db, &version)).await {
            Ok(Ok(())) => {}
            Ok(Err(store::MemecoinStoreError::VersionConflict)) => {
                tracing::error!(strategy = %config.id, version = config.version, "memecoin paper: seed strategy left out; its version is recorded with other content");
                continue;
            }
            Ok(Err(error)) => {
                tracing::warn!(%error, "memecoin paper database call failed");
                return Err("database call failed");
            }
            Err(_) => return Err("database call timed out"),
        }
        let insert = store::insert_strategy(db, &config.id, config.version, true, "seed");
        bounded("seed strategy", insert).await?;
    }
    Ok(())
}

/// Settings for [`start`]; secrets stay inside `market`.
pub struct StartConfig {
    pub profile: PaperProfile,
    pub market: Box<dyn MarketSource>,
    /// Clear the pause made at exactly this time, if it is the current one.
    pub resume_paused_at: Option<OffsetDateTime>,
    /// Claude, used only when the profile has an `agent` section.
    pub brain: Option<Arc<dyn agent::Brain>>,
    pub events: Option<Hub>,
}

/// Record the legacy strategy version, seed the strategies, apply a
/// configured resume and spawn the interval task. An error leaves the
/// trader off; Core keeps running.
pub async fn start(
    db: Database,
    handle: Arc<PaperHandle>,
    config: StartConfig,
) -> Result<(), String> {
    let StartConfig {
        profile,
        market,
        resume_paused_at,
        brain,
        events,
    } = config;
    if let (Some(strategy), Some(params)) = (&profile.strategy, &profile.versioned_params) {
        let register = store::register_strategy_version(&db, strategy.version, params);
        bounded("strategy version", register).await.map_err(|error| {
            format!(
                "strategy version {} cannot be recorded ({error}); a changed profile needs a new version",
                strategy.version
            )
        })?;
    }
    seed_strategies(&db, &profile.rules)
        .await
        .map_err(|error| format!("strategies cannot be seeded ({error})"))?;
    if let Some(paused_at) = resume_paused_at {
        match bounded("resume", store::resume(&db, paused_at)).await {
            Ok(true) => tracing::warn!(%paused_at, "memecoin paper pause cleared by configuration"),
            Ok(false) => tracing::info!("memecoin paper resume setting matches no current pause"),
            Err(_) => tracing::warn!("memecoin paper resume failed"),
        }
    }
    let mut task = PaperTask::new(profile, handle.clone()).with_agent(brain, events);
    if task.agent.is_some() {
        tracing::info!("memecoin paper: Claude decides entries and exits (paper only)");
    }
    if let Some(discovery) = task.discovery.as_mut() {
        let now = OffsetDateTime::now_utc();
        restore_universe(&db, discovery, &mut task.universe_saved, now).await;
    }
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
                    discovered = stats.discovered,
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
    tracing::info!("memecoin PAPER trader running (no execution path)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;
    use std::sync::Mutex;

    use jarvis_onchain::config::{Condition, FilterOverrides, Op};
    use jarvis_onchain::signals::SignalId;
    use jarvis_onchain::types::LpStatus;

    use std::collections::VecDeque;

    use jarvis_llm::Usage;

    use super::profile::tests::{fixture, fixture_json, MINT_A, MINT_B};
    use super::store::StrategyFamily;
    use super::*;

    fn d(value: &str) -> Decimal {
        Decimal::from_str(value).unwrap()
    }

    fn pair(mint: &str, price: &str, liquidity: &str) -> Pair {
        aged_pair(mint, price, liquidity, Duration::days(3))
    }

    fn aged_pair(mint: &str, price: &str, liquidity: &str, age: Duration) -> Pair {
        serde_json::from_value(serde_json::json!({
            "chainId": "solana", "dexId": "raydium", "pairAddress": format!("pool-{mint}"),
            "baseToken": {"address": mint}, "quoteToken": {"address": USDC_MINT},
            "priceUsd": price, "liquidity": {"usd": liquidity},
            "txns": {"m5": {"buys": 40, "sells": 10}}, "priceChange": {"m5": 12.5},
            "pairCreatedAt": (OffsetDateTime::now_utc() - age).unix_timestamp() * 1000,
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
            peak_value_usd: None,
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
    fn entry_candidates_put_the_watchlist_first_and_respect_the_cooldown() {
        let now = OffsetDateTime::now_utc();
        let names = |prefix: &str, n: usize| -> Vec<String> {
            (0..n).map(|i| format!("{prefix}{i}")).collect()
        };
        let watchlist = names("w", 3);
        let discovered = names("d", 6);
        let held = BTreeSet::from(["w0".to_owned()]);
        let mut seen = HashMap::new();
        seen.insert("w1".to_owned(), ((), now - Duration::minutes(1)));
        seen.insert("d0".to_owned(), ((), now - Duration::hours(1)));
        // A discovered mint already on the watchlist counts once.
        let extra = "w2".to_owned();
        let picked = entry_candidates(
            &watchlist,
            discovered.iter().chain([&extra]),
            &held,
            &seen,
            now,
        );
        // w0 is held, w1 cooling down; never-observed discovered mints come
        // before d0, which was observed an hour ago.
        assert_eq!(picked, ["w2", "d1", "d2", "d3", "d4"]);
    }

    #[test]
    fn rolls_stay_in_the_unit_interval() {
        for _ in 0..1_000 {
            let value = roll();
            assert!(value >= Decimal::ZERO && value < Decimal::ONE);
        }
    }

    #[test]
    fn versions_parse_into_what_they_allow() {
        let base = fixture().rules;
        let mut config = seeds()[0].clone();
        config.version = STRATEGY_VERSION_FLOOR + 1;
        let id = Some(config.id.clone());
        let json = serde_json::to_string(&config).unwrap();
        let parsed = parse_version(config.version, id.clone(), &json, &base);
        assert!(matches!(parsed.rules, Rules::Config { .. }));
        assert_eq!(parsed.strategy_id, id);
        // Under stricter base rules the seed would loosen them: exits only.
        let mut strict = fixture_json();
        strict["filter"]["min_liquidity_usd"] = "100000".into();
        let strict = PaperProfile::parse(&strict.to_string()).unwrap().rules;
        let parsed = parse_version(config.version, id.clone(), &json, &strict);
        assert!(matches!(parsed.rules, Rules::ExitOnly(rules) if rules == config.exit_rules()));
        // Recorded under another version or strategy: nothing usable.
        let other = parse_version(config.version + 1, id.clone(), &json, &base);
        assert!(matches!(other.rules, Rules::Unusable));
        let other = parse_version(config.version, Some("b".into()), &json, &base);
        assert!(matches!(other.rules, Rules::Unusable));
        // A legacy profile version exits by its recorded parameters.
        let legacy = fixture().versioned_params.unwrap();
        let parsed = parse_version(3, None, &legacy, &base);
        let rules = fixture().strategy.unwrap().exit_rules();
        assert!(matches!(parsed.rules, Rules::ExitOnly(r) if r == rules));
        assert!(matches!(
            parse_version(3, None, "{}", &base).rules,
            Rules::Unusable
        ));
    }

    #[test]
    fn caps_limit_positions_entries_per_day_and_reentry() {
        let now = OffsetDateTime::now_utc();
        let config = seeds()[0].clone();
        let at = |ago: Duration| now - ago;
        let mut open = trip("open", MINT_A, 0, None);
        open.opened_at = at(Duration::hours(1));
        let mut closed = trip("closed", MINT_B, 0, None);
        closed.opened_at = at(Duration::hours(3));
        closed.close = Some(TripClose {
            closed_at: at(Duration::hours(2)),
            usd_out: d("30"),
            exit_fees_usd: d("0"),
            net_pnl_usd: d("5"),
            exit_reason: "take_profit",
        });
        // Version 9 belongs to no strategy and is not counted.
        let mut other = trip("other", USDC_MINT, 0, None);
        other.strategy_version = 9;
        other.opened_at = at(Duration::minutes(1));
        let trips = [open, closed, other];
        let owner = |version: u32| (version == 3).then_some("a");
        let start_of_day = now.replace_time(time::Time::MIDNIGHT);
        let opened_today = |t: &Trip| t.opened_at >= start_of_day;
        let today = trips[..2].iter().filter(|t| opened_today(t)).count() as u32;

        let caps = Caps::new(trips.iter(), owner, 2, now);
        assert_eq!(caps.open.get("a"), Some(&1));
        assert_eq!(caps.entries_today.get("a").copied().unwrap_or(0), today);
        // `a` left MINT_B two hours ago; its cooldown is 6 hours.
        assert!(!caps.allow("a", &config, MINT_B, now));
        assert!(caps.allow("a", &config, MINT_B, at(-Duration::hours(4))));
        assert!(caps.allow("b", &config, MINT_B, now));
        // One more open position fills `a`'s share of two.
        let mut full = Caps::new(trips.iter(), owner, 2, now);
        assert!(full.allow("a", &config, USDC_MINT, now));
        full.entered("a");
        assert!(!full.allow("a", &config, USDC_MINT, now));
        // Entries per UTC day count the closed ones too.
        let mut daily = Caps::new(trips.iter(), owner, 10, now);
        while daily.entries_today.get("a").copied().unwrap_or(0) < config.max_entries_per_day {
            assert!(daily.allow("a", &config, USDC_MINT, now));
            daily.entered("a");
        }
        assert!(!daily.allow("a", &config, USDC_MINT, now));
    }

    #[test]
    fn enable_challenges_are_single_use_bound_and_short_lived() {
        let now = OffsetDateTime::now_utc();
        let device = uuid::Uuid::now_v7();
        let challenge = |id: &str| EnableChallenge {
            strategy_id: id.into(),
            version: 1_000_002,
            config_sha256: [1; 32],
            nonce: [2; 32],
            expires_at: now + ENABLE_CHALLENGE_TTL,
        };
        let mut open = EnableChallenges::default();
        assert!(open.issue(device, challenge("b"), now));
        assert_eq!(open.take(device, "b", now), Some(challenge("b")));
        // Used up, whatever the outcome.
        assert_eq!(open.take(device, "b", now), None);
        // Another strategy's request uses it up without returning it.
        assert!(open.issue(device, challenge("b"), now));
        assert_eq!(open.take(device, "a", now), None);
        assert_eq!(open.take(device, "b", now), None);
        // Expired after five minutes.
        assert!(open.issue(device, challenge("b"), now));
        assert_eq!(open.take(device, "b", now + ENABLE_CHALLENGE_TTL), None);
        // A new challenge replaces the device's open one; devices are capped.
        assert!(open.issue(device, challenge("a"), now));
        assert!(open.issue(device, challenge("b"), now));
        assert_eq!(open.take(device, "b", now), Some(challenge("b")));
        for _ in 0..MAX_ENABLE_CHALLENGES {
            assert!(open.issue(uuid::Uuid::now_v7(), challenge("b"), now));
        }
        assert!(!open.issue(device, challenge("b"), now));
        // Expired ones make room again.
        assert!(open.issue(device, challenge("b"), now + ENABLE_CHALLENGE_TTL));
    }

    /// Replies in order, recording each input. Never runs a model.
    struct ScriptedBrain {
        replies: Mutex<VecDeque<Result<String, u16>>>,
        inputs: Mutex<Vec<(WorkerTask, serde_json::Value)>>,
    }

    impl ScriptedBrain {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                replies: Mutex::new(VecDeque::new()),
                inputs: Mutex::new(Vec::new()),
            })
        }

        fn reply(&self, reply: Result<String, u16>) {
            self.replies.lock().unwrap().push_back(reply);
        }

        fn inputs(&self) -> Vec<(WorkerTask, serde_json::Value)> {
            self.inputs.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl agent::Brain for ScriptedBrain {
        async fn run(
            &self,
            task: WorkerTask,
            input: &serde_json::Value,
        ) -> Result<ChatReply, LlmError> {
            self.inputs.lock().unwrap().push((task, input.clone()));
            let reply = self.replies.lock().unwrap().pop_front();
            match reply {
                Some(Ok(text)) => Ok(ChatReply {
                    text,
                    model: "claude-test".into(),
                    backend: Some("claude-cli".into()),
                    requested_route: None,
                    actual_provider: None,
                    stop_reason: None,
                    usage: Some(Usage {
                        input_tokens: 120,
                        output_tokens: 30,
                        cache_read_tokens: 0,
                        cache_write_tokens: 0,
                    }),
                    fallback_count: 0,
                }),
                Some(Err(status)) => Err(LlmError::Api {
                    status,
                    body: String::new(),
                }),
                None => panic!("unscripted call"),
            }
        }
    }

    /// Wait for the running Claude call, if any, to finish.
    async fn settle(task: &PaperTask) {
        for _ in 0..500 {
            let busy = match task.agent.as_ref().and_then(|a| a.in_flight.as_ref()) {
                Some(InFlight::Decision(_, reply)) => !reply.is_finished(),
                Some(InFlight::Review(review)) => !review.is_finished(),
                None => false,
            };
            if !busy {
                return;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
        panic!("agent call did not finish");
    }

    /// Fixture market data: every pool's LP is burned; each mint has its own
    /// pool age, so test strategies can tell them apart.
    struct FixtureMarket {
        prices: Mutex<HashMap<String, Decimal>>,
        ages: HashMap<String, Duration>,
        /// Paused by the owner while the first candidate is observed.
        pause_on_observe: Option<Arc<PaperHandle>>,
    }

    impl FixtureMarket {
        fn new(mints: &[(&str, &str, Duration)]) -> Self {
            Self {
                prices: Mutex::new(mints.iter().map(|(m, p, _)| ((*m).into(), d(p))).collect()),
                ages: mints
                    .iter()
                    .map(|(m, _, age)| ((*m).into(), *age))
                    .collect(),
                pause_on_observe: None,
            }
        }

        fn set_price(&self, mint: &str, price: &str) {
            self.prices.lock().unwrap().insert(mint.into(), d(price));
        }
    }

    #[async_trait]
    impl MarketSource for FixtureMarket {
        async fn pairs(&self, mints: &[&str]) -> Result<Vec<Pair>, MarketDataError> {
            let prices = self.prices.lock().unwrap();
            Ok(mints
                .iter()
                .filter_map(|m| {
                    let price = prices.get(*m)?.to_string();
                    Some(aged_pair(m, &price, "100000", self.ages[*m]))
                })
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
            s.lp = LpStatus::Burned;
            Ok(s)
        }
    }

    const MINT_C: &str = "JUPyiwrYJFskUPiHa7hkeR8VUtAeFoSYbKedZNsDvCN";
    const MINT_D: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";

    async fn connect(prefix: &str) -> Result<Database, Box<dyn std::error::Error>> {
        use surrealdb::{engine::remote::ws::Ws, opt::auth::Root, Surreal};
        let db = Surreal::new::<Ws>(std::env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
        db.signin(Root {
            username: &std::env::var("JARVIS_SURREAL_TEST_USER")?,
            password: &std::env::var("JARVIS_SURREAL_TEST_PASS")?,
        })
        .await?;
        db.use_ns(format!("{prefix}_{}", uuid::Uuid::now_v7().simple()))
            .use_db("core")
            .await?;
        jarvis_store::apply_baseline_schema(&db).await?;
        Ok(db)
    }

    /// A profile observing `watchlist`, with room for four positions.
    fn watching(watchlist: &[&str]) -> PaperProfile {
        let mut json = fixture_json();
        json["watchlist"] = serde_json::json!(watchlist);
        json["max_open_positions"] = 4.into();
        PaperProfile::parse(&json.to_string()).unwrap()
    }

    /// Seed A, B and C, then switch each to a child version that enters on
    /// pool age alone: A from 30 minutes to 5 hours (trailing stop, one
    /// entry a day), B from 6 to 72 hours, C from 14 days. Returns the
    /// versions in that order.
    async fn pool_age_strategies(
        db: &Database,
        profile: &PaperProfile,
    ) -> Result<[u32; 3], Box<dyn std::error::Error>> {
        seed_strategies(db, &profile.rules).await?;
        let mut versions = [0; 3];
        let families = [StrategyFamily::A, StrategyFamily::B, StrategyFamily::C];
        let ages = [(30, Some(300)), (360, Some(4_320)), (14 * 24 * 60, None)];
        for (i, mut config) in seeds().into_iter().enumerate() {
            let seed = STRATEGY_VERSION_FLOOR + 1 + i as u32;
            config.version = seed + 10;
            config.parent = Some(seed);
            config.filters = FilterOverrides::default();
            let (a, b) = ages[i];
            let op = if b.is_some() { Op::Between } else { Op::Gt };
            config.entry = vec![Condition {
                signal: SignalId::PoolAgeMinutes,
                op,
                a,
                b,
            }];
            if i == 0 {
                config.take_profit_bps = 100_000;
                config.trailing_stop_bps = Some(2_000);
                config.max_entries_per_day = 1;
            } else if i == 1 {
                config.take_profit_bps = 5_000;
            }
            config.validate(&profile.rules)?;
            let version = StrategyVersion {
                version: config.version,
                strategy_id: config.id.clone(),
                family: families[i],
                parent_version: Some(seed),
                author: StrategyAuthor::Claude,
                params: serde_json::to_string(&config)?,
                rationale: None,
                backtest: None,
                created_at: OffsetDateTime::now_utc(),
            };
            store::insert_strategy_version(db, &version).await?;
            assert!(store::set_active_version(db, &config.id, config.version, "test").await?);
            versions[i] = config.version;
        }
        Ok(versions)
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn ticks_run_three_strategies_with_their_own_limits(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let db = connect("memecoin_tick").await?;
        // MINT_A suits A, MINT_B and MINT_D suit B, MINT_C suits C.
        let watchlist = [MINT_A, MINT_B, MINT_C, MINT_D];
        let profile = watching(&watchlist);
        let [a, b, c] = pool_age_strategies(&db, &profile).await?;
        let handle = Arc::new(PaperHandle::new(true));
        let mut task = PaperTask::new(profile, handle.clone());
        let market = FixtureMarket::new(&[
            (MINT_A, "0.001", Duration::hours(2)),
            (MINT_B, "0.001", Duration::hours(10)),
            (MINT_C, "0.002", Duration::days(20)),
            (MINT_D, "0.001", Duration::hours(7)),
        ]);
        // Rolls at or above the 10% failure rate fill; a roll of 0 fails.
        let mut no_failure = || Decimal::new(999_999, 6);
        let version_of = |trips: &[Trip], mint: &str| {
            trips
                .iter()
                .find(|t| t.mint == mint)
                .map(|t| t.strategy_version)
        };

        // Tick 1: each strategy enters its mint. With four positions shared
        // by three strategies each may hold one, so B skips MINT_D.
        let stats = task.tick(&db, &market, &mut no_failure).await?;
        assert_eq!((stats.observed, stats.accepted, stats.entries), (4, 4, 3));
        let trips = store::load_trips(&db).await?;
        assert_eq!(version_of(&trips, MINT_A), Some(a));
        assert_eq!(version_of(&trips, MINT_B), Some(b));
        assert_eq!(version_of(&trips, MINT_C), Some(c));
        assert_eq!(version_of(&trips, MINT_D), None);
        assert_eq!(trips[0].cost_usd, d("25.011"));
        // A filter verdict per mint and strategy version.
        let candidates = store::recent_candidates(&db, 100).await?;
        assert_eq!(candidates.len(), 12);
        assert!(candidates.iter().all(|c| c.reason.is_none()));
        // What was seen is recorded for the signals.
        let series = store::load_series(&db, MINT_A, OffsetDateTime::now_utc(), 10).await?;
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].price_usd, d("0.001"));
        let view = handle.view().ok_or("no view")?;
        assert_eq!(view.open.len(), 3);
        // Deprecated, for single-strategy clients: the first strategy's version.
        assert_eq!(view.strategy_version, a);

        // Tick 2: the owner disables B; its position still exits at the
        // take profit. A rises: its peak value is stored, it holds on.
        assert!(store::set_strategy_enabled(&db, "b-base-reclaim", false, true, "device-1").await?);
        market.set_price(MINT_A, "0.0015");
        market.set_price(MINT_B, "0.002");
        let stats = task.tick(&db, &market, &mut no_failure).await?;
        assert_eq!((stats.observed, stats.exits, stats.entries), (0, 1, 0));
        let closed = store::recent_closed_trips(&db, 10).await?;
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].mint, MINT_B);
        assert_eq!(
            closed[0].close.as_ref().ok_or("open")?.exit_reason,
            "take_profit"
        );
        let held = store::load_held_trips(&db).await?;
        let a_trip = held.iter().find(|t| t.mint == MINT_A).ok_or("A not held")?;
        assert!(a_trip.peak_value_usd.ok_or("no peak")? > d("35"));

        // Tick 3: A falls more than 20% below its peak, still above cost:
        // the trailing stop sells. MINT_B and MINT_D suit only the disabled
        // B, so nothing enters.
        market.set_price(MINT_A, "0.0011");
        task.seen.clear();
        let stats = task.tick(&db, &market, &mut no_failure).await?;
        assert_eq!((stats.observed, stats.exits, stats.entries), (2, 1, 0));
        let closed = store::recent_closed_trips(&db, 10).await?;
        let a_close = closed[0].close.as_ref().ok_or("open")?;
        assert_eq!(
            (closed[0].mint.as_str(), a_close.exit_reason),
            (MINT_A, "trailing_stop")
        );
        assert!(a_close.net_pnl_usd > Decimal::ZERO);

        // Tick 4: the owner enables B again. A has used its entry for the
        // day and B its cooldown on MINT_B, so only B on MINT_D tries; the
        // attempt fails and its fees are a cost of B's version.
        assert!(store::set_strategy_enabled(&db, "b-base-reclaim", true, true, "device-1").await?);
        task.seen.clear();
        let mut always_fail = || Decimal::ZERO;
        let stats = task.tick(&db, &market, &mut always_fail).await?;
        assert_eq!(
            (stats.observed, stats.entries, stats.failed_attempts),
            (3, 0, 1)
        );
        let costs = store::load_costs(&db).await?;
        assert_eq!(costs.len(), 1);
        assert_eq!((costs[0].strategy_version, costs[0].usd), (b, d("0.011")));

        // Results per strategy, over all of its versions.
        let b_result = store::strategy_performance(&db, "b-base-reclaim").await?;
        assert_eq!(
            (b_result.closed.trades, b_result.closed.win_rate_bps),
            (1, 10_000)
        );
        assert_eq!(b_result.costs_usd, d("0.011"));
        assert_eq!(b_result.net_usd, b_result.closed.net_usd - d("0.011"));
        let c_result = store::strategy_performance(&db, "c-established-trend").await?;
        assert_eq!(c_result.closed.trades, 0);
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn a_pause_during_a_tick_stops_the_next_buy() -> Result<(), Box<dyn std::error::Error>> {
        let db = connect("memecoin_pause").await?;
        let profile = watching(&[MINT_A]);
        pool_age_strategies(&db, &profile).await?;
        let handle = Arc::new(PaperHandle::new(true));
        let mut task = PaperTask::new(profile, handle.clone());
        // The owner pauses while A, which would be bought, is observed.
        let mut market = FixtureMarket::new(&[(MINT_A, "0.001", Duration::hours(2))]);
        market.pause_on_observe = Some(handle.clone());
        let stats = task
            .tick(&db, &market, &mut || Decimal::new(999_999, 6))
            .await?;
        assert_eq!((stats.observed, stats.accepted, stats.entries), (1, 1, 0));
        assert!(store::load_trips(&db).await?.is_empty());

        // A stored pause is picked up by the next tick.
        store::pause(&db, "device").await?;
        let mut task = PaperTask::new(watching(&[MINT_A]), Arc::new(PaperHandle::new(true)));
        let stats = task
            .tick(&db, &market, &mut || Decimal::new(999_999, 6))
            .await?;
        assert!(task.handle.paused());
        assert_eq!((stats.observed, stats.entries), (0, 0));
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn a_position_without_rules_still_gets_the_time_stop(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let db = connect("memecoin_rules").await?;
        // Opened under a version that was never recorded, 31 days ago.
        let mut orphan = trip("orphan", MINT_A, 0, None);
        orphan.strategy_version = 77;
        orphan.opened_at = OffsetDateTime::now_utc() - Duration::days(31);
        store::open_trip(&db, &orphan).await?;
        let mut task = PaperTask::new(watching(&[MINT_A]), Arc::new(PaperHandle::new(true)));
        let market = FixtureMarket::new(&[(MINT_A, "0.25", Duration::hours(2))]);
        let stats = task
            .tick(&db, &market, &mut || Decimal::new(999_999, 6))
            .await?;
        assert_eq!(stats.exits, 1);
        let closed = store::recent_closed_trips(&db, 1).await?;
        assert_eq!(
            closed[0].close.as_ref().ok_or("open")?.exit_reason,
            "time_stop"
        );
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn seeding_is_idempotent_and_never_reuses_a_version(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let db = connect("memecoin_seed").await?;
        let profile = fixture();
        // A legacy version recorded before typed strategies existed.
        let legacy = profile.versioned_params.clone().ok_or("no legacy params")?;
        store::register_strategy_version(&db, 3, &legacy).await?;

        // Base rules that seeds A and B would loosen leave them out.
        let mut strict = fixture_json();
        strict["filter"]["min_liquidity_usd"] = "100000".into();
        let strict = PaperProfile::parse(&strict.to_string())?;
        seed_strategies(&db, &strict.rules).await?;
        let ids = |s: &[store::Strategy]| s.iter().map(|s| s.id.clone()).collect::<Vec<_>>();
        assert_eq!(
            ids(&store::list_strategies(&db).await?),
            ["c-established-trend"]
        );

        seed_strategies(&db, &profile.rules).await?;
        assert!(
            store::set_strategy_enabled(&db, "a-survivor-momentum", false, true, "device-1")
                .await?
        );
        seed_strategies(&db, &profile.rules).await?;
        let strategies = store::list_strategies(&db).await?;
        assert_eq!(
            ids(&strategies),
            [
                "a-survivor-momentum",
                "b-base-reclaim",
                "c-established-trend"
            ]
        );
        // Seeding again keeps the owner's disable and the active versions.
        assert!(!strategies[0].enabled && strategies[0].disabled_by_owner);
        assert!(strategies[1].enabled && strategies[2].enabled);
        let mut seen = BTreeSet::from([3]);
        for (i, strategy) in strategies.iter().enumerate() {
            let expected = STRATEGY_VERSION_FLOOR + 1 + i as u32;
            assert_eq!(strategy.active_version, expected);
            let versions = store::list_strategy_versions(&db, &strategy.id, 10).await?;
            assert_eq!(versions.len(), 1);
            assert_eq!(versions[0].author, StrategyAuthor::Seed);
            assert!(seen.insert(versions[0].version), "versions never collide");
        }
        // The legacy version is untouched.
        let (owner, params) = store::version_params(&db, 3).await?.ok_or("legacy gone")?;
        assert_eq!((owner, params), (None, legacy));
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn the_universe_outlives_a_restart() -> Result<(), Box<dyn std::error::Error>> {
        let db = connect("memecoin_universe").await?;
        let now = OffsetDateTime::now_utc();
        let migration = |mint: &str, age| Migration {
            mint: mint.into(),
            pool: MINT_B.into(),
            migrated_at: now - age,
        };
        let mut before = Discovery::new();
        let mut saved = HashSet::new();
        assert!(before.remember(migration(MINT_A, Duration::hours(1)), now));
        save_universe(&db, &before, &mut saved).await;
        assert!(saved.contains(MINT_A));
        // A stored entry beyond the discovery window stays out.
        store::upsert_universe(
            &db,
            &store::UniverseEntry {
                mint: MINT_C.into(),
                pool: MINT_B.into(),
                program: pools::PUMPSWAP_PROGRAM.into(),
                migrated_at: now - Duration::days(4),
            },
        )
        .await?;

        let mut after = Discovery::new();
        let mut restored = HashSet::new();
        restore_universe(&db, &mut after, &mut restored, now).await;
        assert!(after.knows(MINT_A) && !after.knows(MINT_C));
        assert_eq!(restored, HashSet::from([MINT_A.to_owned()]));
        Ok(())
    }

    /// Move every recorded call an hour back, past the decision cadence.
    async fn age_calls(
        db: &Database,
        task: &mut PaperTask,
    ) -> Result<(), Box<dyn std::error::Error>> {
        db.query("UPDATE memecoin_decisions SET at = at - 1h")
            .await?
            .check()?;
        task.agent.as_mut().ok_or("no agent")?.last_decision = None;
        Ok(())
    }

    async fn agent_rows(
        db: &Database,
    ) -> Result<Vec<store::StoredDecision>, Box<dyn std::error::Error>> {
        let mut rows = store::recent_decisions(db, 100).await?;
        rows.retain(|row| {
            row.call_id.starts_with("d-") && row.reason_code.as_deref() != Some("call_started")
        });
        Ok(rows)
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn claude_decides_entries_and_exits_within_the_code_limits(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let db = connect("memecoin_agent").await?;
        let mut json = fixture_json();
        json["watchlist"] = serde_json::json!([MINT_A, MINT_B]);
        json["max_open_positions"] = 4.into();
        json["agent"] = serde_json::json!({});
        let profile = PaperProfile::parse(&json.to_string())?;
        // MINT_A suits A, MINT_B suits B. These versions are Claude's, so the
        // review has no room left today and makes no call.
        let [a, b, _] = pool_age_strategies(&db, &profile).await?;
        let brain = ScriptedBrain::new();
        let handle = Arc::new(PaperHandle::new(true));
        let mut task =
            PaperTask::new(profile, handle.clone()).with_agent(Some(brain.clone()), None);
        let market = FixtureMarket::new(&[
            (MINT_A, "0.001", Duration::hours(2)),
            (MINT_B, "0.001", Duration::hours(10)),
        ]);
        let mut no_failure = || Decimal::new(999_999, 6);

        // Tick 1: both mints pass, but nothing enters without Claude.
        let stats = task.tick(&db, &market, &mut no_failure).await?;
        assert_eq!((stats.accepted, stats.entries), (2, 0));
        settle(&task).await;
        assert!(brain.inputs().is_empty());

        // Tick 2: the decision call starts, with numbers only.
        brain.reply(Ok(r#"{"actions":[
            {"action":"enter","id":"c0","size_bps":5000,"reason_code":"momentum"},
            {"action":"enter","id":"c1","size_bps":5000,"reason_code":"base_reclaim"}],
            "rationale":"Two setups."}"#
            .into()));
        task.tick(&db, &market, &mut no_failure).await?;
        settle(&task).await;
        let inputs = brain.inputs();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].0, WorkerTask::MemeDecision);
        agent::tests::assert_numbers_only(&inputs[0].1);
        assert_eq!(
            inputs[0].1["candidates"].as_object().map(|c| c.len()),
            Some(2)
        );
        assert_eq!(handle.agent().ok_or("no status")?.budget.calls_last_hour, 0);

        // Tick 3: Claude's entries pass the checks again and enter at half of
        // what each strategy would enter.
        let stats = task.tick(&db, &market, &mut no_failure).await?;
        assert_eq!(stats.entries, 2);
        let trips = store::load_trips(&db).await?;
        assert_eq!(trips.len(), 2);
        for trip in &trips {
            let i = if trip.mint == MINT_A { 0 } else { 1 };
            assert_eq!(trip.strategy_version, [a, b][i]);
            let full = seeds()[i]
                .entry_size(d("100000"), d("25"))
                .ok_or("no size")?;
            let half = (full / Decimal::TWO).round_dp_with_strategy(2, RoundingStrategy::ToZero);
            assert_eq!(trip.cost_usd, half + d("0.011"));
        }
        let rows = agent_rows(&db).await?;
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.kind == "enter"
            && r.size_bps == Some(5_000)
            && r.model.as_deref() == Some("claude-test")
            && r.rationale.as_deref() == Some("Two setups.")));
        // The call's tokens are on one row only.
        assert_eq!(
            rows.iter().filter(|r| r.input_tokens == Some(120)).count(),
            1
        );
        assert_eq!(handle.agent().ok_or("no status")?.budget.calls_last_hour, 1);

        // Claude exits one position; its rows are dated back past the cadence.
        age_calls(&db, &mut task).await?;
        brain.reply(Ok(r#"{"actions":[{"action":"exit","id":"p0"}]}"#.into()));
        task.tick(&db, &market, &mut no_failure).await?;
        settle(&task).await;
        let positions = &brain.inputs()[1].1;
        agent::tests::assert_numbers_only(positions);
        assert_eq!(positions["positions"].as_object().map(|p| p.len()), Some(2));
        assert!(positions["candidates"]
            .as_object()
            .is_some_and(|c| c.is_empty()));
        let stats = task.tick(&db, &market, &mut no_failure).await?;
        assert_eq!(stats.exits, 1);
        let closed = store::recent_closed_trips(&db, 10).await?;
        assert_eq!(closed.len(), 1);
        assert_eq!(
            closed[0].close.as_ref().ok_or("open")?.exit_reason,
            "agent_exit"
        );

        // An invalid reply is dropped whole and recorded; nothing changes.
        age_calls(&db, &mut task).await?;
        brain.reply(Ok(
            r#"{"actions":[{"action":"exit","id":"p0"},{"action":"hold"}]}"#.into(),
        ));
        task.tick(&db, &market, &mut no_failure).await?;
        settle(&task).await;
        let stats = task.tick(&db, &market, &mut no_failure).await?;
        assert_eq!(stats.exits, 0);
        assert_eq!(store::recent_closed_trips(&db, 10).await?.len(), 1);
        let latest = agent_rows(&db).await?.into_iter().next().ok_or("no row")?;
        assert_eq!(
            (latest.kind.as_str(), latest.reason_code.as_deref()),
            ("invalid_output", Some("hold_not_alone"))
        );

        // A busy worker only skips the cycle.
        age_calls(&db, &mut task).await?;
        brain.reply(Err(503));
        task.tick(&db, &market, &mut no_failure).await?;
        settle(&task).await;
        task.tick(&db, &market, &mut no_failure).await?;
        let latest = agent_rows(&db).await?.into_iter().next().ok_or("no row")?;
        assert_eq!(
            (latest.kind.as_str(), latest.reason_code.as_deref()),
            ("worker_unavailable", Some("worker_unavailable"))
        );
        let status = handle.agent().ok_or("no status")?;
        assert!(status.budget.paused_until.is_none() && status.budget.backoff_until.is_none());
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn claude_entries_wait_for_an_intact_ledger() -> Result<(), Box<dyn std::error::Error>> {
        let db = connect("memecoin_agent_ledger").await?;
        let mut json = fixture_json();
        json["watchlist"] = serde_json::json!([MINT_A]);
        json["max_open_positions"] = 4.into();
        json["agent"] = serde_json::json!({});
        let profile = PaperProfile::parse(&json.to_string())?;
        pool_age_strategies(&db, &profile).await?;
        let brain = ScriptedBrain::new();
        let mut task = PaperTask::new(profile, Arc::new(PaperHandle::new(true)))
            .with_agent(Some(brain.clone()), None);
        let market = FixtureMarket::new(&[(MINT_A, "0.001", Duration::hours(2))]);
        let mut no_failure = || Decimal::new(999_999, 6);
        task.tick(&db, &market, &mut no_failure).await?;
        settle(&task).await;
        brain.reply(Ok(
            r#"{"actions":[{"action":"enter","id":"c0","size_bps":10000,"reason_code":"momentum"}]}"#
                .into(),
        ));
        task.tick(&db, &market, &mut no_failure).await?;
        settle(&task).await;
        // The stored ledger stops replaying before the reply is applied:
        // without it the limits cannot be counted, so nothing enters.
        let tampered = trip("tampered", MINT_C, 0, None);
        store::open_trip(&db, &tampered).await?;
        let close = trip("tampered", MINT_C, 0, Some((60, "30", "6")))
            .close
            .ok_or("no close")?;
        assert!(store::close_trip(&db, "tampered", &close).await?);
        assert!(task.tick(&db, &market, &mut no_failure).await.is_err());
        assert!(store::load_held_trips(&db).await?.is_empty());
        let rows = agent_rows(&db).await?;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (
                rows[0].kind.as_str(),
                rows[0].reason_code.as_deref(),
                rows[0].mint.as_deref()
            ),
            ("skip", Some("book_unavailable"), Some(MINT_A))
        );
        Ok(())
    }

    /// A proposal as Claude would write it for `s{i}`: the seed's config
    /// under the opaque id, changed by `change`.
    fn proposal(
        strategy_id: &str,
        seed: usize,
        change: impl FnOnce(&mut StrategyConfig),
    ) -> serde_json::Value {
        let mut config = seeds()[seed].clone();
        config.id = strategy_id.into();
        config.version = STRATEGY_VERSION_FLOOR + 1 + seed as u32;
        change(&mut config);
        serde_json::json!({"strategy_id": strategy_id, "config": config, "rationale": "Let winners run."})
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn the_review_records_checked_proposals_and_never_enables(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let db = connect("memecoin_review").await?;
        let profile = fixture();
        seed_strategies(&db, &profile.rules).await?;
        let (b_seed, c_seed) = (STRATEGY_VERSION_FLOOR + 2, STRATEGY_VERSION_FLOOR + 3);
        // The owner disabled B.
        assert!(store::set_strategy_enabled(&db, "b-base-reclaim", false, true, "device-1").await?);
        let brain = ScriptedBrain::new();
        let hub = Hub::default();
        let mut listener = hub
            .subscribe(uuid::Uuid::now_v7(), uuid::Uuid::now_v7())
            .ok_or("no subscription")?;
        listener.receiver.try_recv()?;
        // What apps heard since the last call: ids, versions and flags only.
        let mut heard = || {
            std::iter::from_fn(|| listener.receiver.try_recv().ok())
                .map(|envelope| envelope.event.clone())
                .collect::<Vec<_>>()
        };
        let decision = |event: &Event| match event {
            Event::MemeDecision { id } => uuid::Uuid::parse_str(id).is_ok(),
            _ => false,
        };
        let env = || agent::ReviewEnv {
            db: db.clone(),
            brain: brain.clone(),
            rules: profile.rules.clone(),
            costs: profile.costs.clone(),
            stake_usd: profile.stake_usd,
            max_position_usd: profile.max_position_usd,
            settings: agent::AgentSettings::new(None, None, false).unwrap(),
            events: Some(hub.clone()),
        };
        let review = |proposals: serde_json::Value| {
            brain.reply(Ok(serde_json::json!({"proposals": proposals}).to_string()))
        };
        let propose_rows =
            |rows: Vec<store::StoredDecision>| -> Vec<(Option<u32>, Option<String>)> {
                let mut rows: Vec<_> = rows.into_iter().filter(|r| r.kind == "propose").collect();
                rows.reverse();
                rows.into_iter()
                    .map(|r| (r.strategy_version, r.reason_code))
                    .collect()
            };

        // Review 1: a new strategy, a change to the owner-disabled B, and a
        // third version, which is over the two a day.
        let mut new = proposal("new", 0, |c| c.stop_loss_bps = 1_500);
        new["config"]["version"] = 1.into();
        new["config"]["parent"] = serde_json::Value::Null;
        review(serde_json::json!([
            new,
            proposal("s1", 1, |c| c.take_profit_bps = 6_000),
            proposal("s2", 2, |c| c.stop_loss_bps = 1_000),
        ]));
        agent::run_review(env()).await;
        let input = &brain.inputs()[0].1;
        agent::tests::assert_numbers_only(input);
        assert!(!input.to_string().contains("reclaim"));
        assert_eq!(input["new_versions"], 2);
        assert_eq!(input["strategies"]["s1"]["enabled"], 0);
        let (new_v, b_v) = (STRATEGY_VERSION_FLOOR + 4, STRATEGY_VERSION_FLOOR + 5);
        // Each recorded version is heard with its strategy, each decision
        // row by its id; the refused third proposal changes no strategy.
        let events = heard();
        assert_eq!(events.len(), 5, "{events:?}");
        assert_eq!(
            events[0],
            Event::MemeStrategyChanged {
                strategy_id: format!("claude-{new_v}"),
                version: new_v,
                enabled: false,
            }
        );
        assert_eq!(
            events[2],
            Event::MemeStrategyChanged {
                strategy_id: "b-base-reclaim".into(),
                version: b_v,
                enabled: false,
            }
        );
        assert!([1, 3, 4].into_iter().all(|i| decision(&events[i])));
        assert_eq!(
            propose_rows(store::recent_decisions(&db, 100).await?),
            [
                (Some(new_v), Some("accepted".into())),
                (Some(b_v), Some("accepted".into())),
                (None, Some("version_cap".into())),
            ]
        );
        let strategies = store::list_strategies(&db).await?;
        let find = |id: &str| {
            strategies
                .iter()
                .find(|s| s.id == id)
                .cloned()
                .ok_or("missing")
        };
        // Claude never enables: B trades the new version and stays the
        // owner's disable; the new strategy starts disabled by policy.
        let b = find("b-base-reclaim")?;
        assert_eq!(
            (b.active_version, b.enabled, b.disabled_by_owner),
            (b_v, false, true)
        );
        let created = find(&format!("claude-{new_v}"))?;
        assert_eq!(
            (
                created.active_version,
                created.enabled,
                created.disabled_by_owner
            ),
            (new_v, false, false)
        );
        assert_eq!(created.changed_by, "claude");
        let versions = store::list_strategy_versions(&db, "b-base-reclaim", 10).await?;
        assert_eq!(versions[0].version, b_v);
        assert_eq!(versions[0].parent_version, Some(STRATEGY_VERSION_FLOOR + 2));
        assert_eq!(versions[0].author, StrategyAuthor::Claude);
        assert_eq!(versions[0].rationale.as_deref(), Some("Let winners run."));
        assert!(versions[0]
            .backtest
            .as_deref()
            .ok_or("no backtest")?
            .contains("unbacktested"));
        assert_eq!(find("c-established-trend")?.active_version, c_seed);

        // Review 2: no room left today, so no call is made.
        agent::run_review(env()).await;
        assert_eq!(brain.inputs().len(), 1);
        let latest = store::recent_decisions(&db, 1).await?.remove(0);
        assert_eq!(
            (latest.kind.as_str(), latest.reason_code.as_deref()),
            ("hold", Some("no_room"))
        );
        let events = heard();
        assert!(events.len() == 1 && decision(&events[0]), "{events:?}");

        // Review 3, a day later: a loosening, a parent of another strategy and
        // an unchanged config are refused; an invalid reply records nothing else.
        db.query("UPDATE memecoin_strategy_versions SET created_at = created_at - 2d")
            .await?
            .check()?;
        db.query("DELETE memecoin_decisions").await?.check()?;
        review(serde_json::json!([
            proposal("s2", 2, |c| c.filters.min_liquidity_usd = Some(d("1"))),
            proposal("s0", 0, |c| c.version = b_seed),
            proposal("s3", 0, |c| {
                c.version = new_v;
                c.parent = None;
                c.stop_loss_bps = 1_500
            }),
        ]));
        agent::run_review(env()).await;
        assert_eq!(
            propose_rows(store::recent_decisions(&db, 100).await?),
            [
                (None, Some("loosening".into())),
                (None, Some("parent_mismatch".into())),
                (None, Some("unchanged".into())),
            ]
        );
        // Refused proposals change no strategy.
        let events = heard();
        assert!(
            events.len() == 3 && events.iter().all(decision),
            "{events:?}"
        );
        assert_eq!(store::max_version(&db).await?, Some(b_v));
        db.query("DELETE memecoin_decisions").await?.check()?;
        review(serde_json::json!([proposal("s9", 0, |_| {})]));
        agent::run_review(env()).await;
        let mut rows = store::recent_decisions(&db, 10).await?;
        // The call was recorded before it was made.
        assert_eq!(
            rows.pop().and_then(|r| r.reason_code),
            Some("call_started".into())
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (rows[0].kind.as_str(), rows[0].reason_code.as_deref()),
            ("invalid_output", Some("unknown_id"))
        );
        assert_eq!(store::max_version(&db).await?, Some(b_v));
        Ok(())
    }
}
