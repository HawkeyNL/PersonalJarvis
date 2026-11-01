//! Deterministic core of the on-chain memecoin trader.
//!
//! This crate only *calculates*. It has no I/O, no database, no network, no
//! async and no clock reads: every timestamp is passed in by the caller. That
//! keeps each decision reproducible from recorded data, which is what paper
//! trading, backtests and the promotion gate need.
//!
//! It never signs, sends or approves anything. Live execution, the signed
//! owner mandate and key custody live elsewhere (see a private ADR) and stay
//! disabled by default. The mandate remains the only source of limits:
//! numbers such as the maximum position value are inputs here, never outputs.
//!
//! A candidate flows through the modules in this order:
//!
//! 1. [`filter`]: hard safety rules. A token must pass every rule or it is
//!    rejected with a reason. Promotion ("boosted", trending) is ignored.
//! 2. [`strategy`]: versioned, bounded parameters turn a snapshot that passed
//!    the filter into an entry [`strategy::TradeIntent`], and an open position
//!    into an exit intent.
//! 3. [`research`]: the LLM's opinion can only shrink or veto an entry size.
//! 4. [`paper`]: simulated, size-aware fills and a lot ledger with net P&L.
//! 5. [`gate`]: is the paper record good enough to consider going live?
//! 6. [`goals`]: how far the owner's goal (e.g. 2x) is, and what to do there.
//!
//! Money and token amounts are `rust_decimal::Decimal` (USD for money), never
//! floats. Ratios are basis points (`bps`, 10 000 = 100%) or `Decimal`
//! fractions where noted.

pub mod filter;
pub mod gate;
pub mod goals;
pub mod paper;
pub mod research;
pub mod strategy;
pub mod types;

#[cfg(test)]
pub(crate) mod testutil {
    use crate::paper::CostModel;
    use crate::types::{LpStatus, TokenSnapshot};
    use rust_decimal::Decimal;
    use time::{Duration, OffsetDateTime};

    pub fn d(s: &str) -> Decimal {
        s.parse().expect("test decimal")
    }

    pub fn costs() -> CostModel {
        CostModel {
            dex_fee_bps: 25,
            network_fee_usd: d("0.001"),
            priority_fee_usd: d("0.01"),
            failure_rate: d("0.1"),
        }
    }

    pub fn t0() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::days(20_000)
    }

    /// A snapshot that passes the test filter rules and the test strategy.
    pub fn good_snapshot() -> TokenSnapshot {
        TokenSnapshot {
            mint: "MintGood111".to_owned(),
            observed_at: t0(),
            pool_created_at: t0() - Duration::hours(6),
            price_usd: d("0.001"),
            liquidity_usd: d("200000"),
            mint_authority_revoked: true,
            freeze_authority_revoked: true,
            extensions: Vec::new(),
            lp: LpStatus::Burned,
            top_holders_share: d("0.15"),
            sell_impact_bps: Some(80),
            buys_5m: 70,
            sells_5m: 30,
            price_change_5m_bps: 1_200,
            boosts: 0,
            trending_rank: None,
        }
    }

    /// Tiny deterministic xorshift generator for hand-rolled property tests
    /// (proptest is not a workspace dependency).
    pub struct Rng(u64);

    impl Rng {
        pub fn new(seed: u64) -> Self {
            Self(seed.max(1))
        }

        pub fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        /// Uniform-ish in `0..n`.
        pub fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }

        /// A non-negative decimal with up to `scale` fractional digits.
        pub fn decimal(&mut self, max_units: u64, scale: u32) -> Decimal {
            let raw = self.below(max_units * 10u64.pow(scale) + 1);
            Decimal::new(raw as i64, scale)
        }
    }
}
