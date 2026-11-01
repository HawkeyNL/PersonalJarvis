//! Shared input types.
//!
//! A [`TokenSnapshot`] is everything the deterministic core knows about one
//! token at one moment. It is filled by a market-data ingest from untrusted
//! public sources, so every rule that reads it must treat odd values (zero
//! price, future timestamps, shares above 100%) as a reason to reject, never
//! as a reason to buy.

use rust_decimal::Decimal;
use time::OffsetDateTime;

/// One observation of a token and its main pool.
#[derive(Debug, Clone, PartialEq)]
pub struct TokenSnapshot {
    pub mint: String,
    /// When the data was observed (data time, not the time it was processed).
    pub observed_at: OffsetDateTime,
    pub pool_created_at: OffsetDateTime,
    /// Mid price of one token in USD. Not executable: fills use [`crate::paper`].
    pub price_usd: Decimal,
    /// Total pool liquidity in USD (both sides of the pool).
    pub liquidity_usd: Decimal,
    pub mint_authority_revoked: bool,
    pub freeze_authority_revoked: bool,
    /// Token-2022 mint extensions. Always empty for classic SPL Token mints.
    pub extensions: Vec<Extension>,
    pub lp: LpStatus,
    /// Share of supply held by the largest holders, pool and burn addresses
    /// excluded, as a fraction (`0.25` = 25%).
    pub top_holders_share: Decimal,
    /// Price impact of a quoted sell of the full intended position size.
    /// `None` means no sell route was found: a possible honeypot.
    pub sell_impact_bps: Option<u32>,
    pub buys_5m: u32,
    pub sells_5m: u32,
    pub price_change_5m_bps: i32,
    /// Paid promotion ("boosts") and trending placement. Recorded so it can be
    /// shown and audited, but no rule may ever treat it as a positive signal:
    /// it measures what someone paid, not what the token is worth.
    pub boosts: u32,
    pub trending_rank: Option<u32>,
}

/// State of the pool's liquidity-provider tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LpStatus {
    Burned,
    Locked { until: OffsetDateTime },
    Unlocked,
    Unknown,
}

/// A Token-2022 extension present on the mint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extension {
    pub kind: ExtensionKind,
    /// `true` when the extension has no authority or its authority is revoked.
    pub authority_revoked: bool,
}

/// Token-2022 extension types the filter can name in an allowlist. Anything
/// the ingest does not recognise is `Unknown` and is always rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionKind {
    MetadataPointer,
    TokenMetadata,
    TransferFeeConfig,
    TransferHook,
    PermanentDelegate,
    DefaultAccountState,
    Pausable,
    MintCloseAuthority,
    NonTransferable,
    ConfidentialTransfer,
    InterestBearing,
    /// Raw extension type number as read on chain.
    Unknown(u16),
}

/// An open position (one lot): what was bought, for how much, and when.
#[derive(Debug, Clone, PartialEq)]
pub struct Position {
    pub mint: String,
    pub tokens: Decimal,
    /// Total USD paid, including entry fees.
    pub cost_usd: Decimal,
    pub opened_at: OffsetDateTime,
    /// Pool liquidity at entry, used by the liquidity-drop exit.
    pub entry_liquidity_usd: Decimal,
}
