//! Hard candidate filters: code, not LLM.
//!
//! Most new memecoins are traps (rug pulls, honeypots, Token-2022 tricks). A
//! token must pass **every** rule here before the strategy may even look at
//! it; the first failing rule is returned as a typed reason so rejects are
//! explainable in the log.
//!
//! The filter can only reject. It never scores a token up, and promotional
//! fields ([`TokenSnapshot::boosts`], [`TokenSnapshot::trending_rank`]) are not
//! read at all.
//!
//! [`FilterRules`] come from the owner's mandate. A learning step may propose
//! its own thresholds, but [`FilterRules::tighten`] only accepts proposals that
//! are at least as strict on every field.

use std::collections::BTreeSet;

use rust_decimal::Decimal;
use time::{Duration, OffsetDateTime};

use crate::types::{ExtensionKind, LpStatus, TokenSnapshot};

/// Thresholds a token must meet. Supplied by the mandate; no defaults here on
/// purpose, so a missing value can never silently become a loose one.
#[derive(Debug, Clone, PartialEq)]
pub struct FilterRules {
    pub min_liquidity_usd: Decimal,
    pub min_pool_age: Duration,
    /// Maximum [`TokenSnapshot::top_holders_share`], as a fraction.
    pub max_top_holders_share: Decimal,
    pub max_sell_impact_bps: u32,
    /// Token-2022 extensions that may be present (each with its authority
    /// revoked). Anything else, and every `Unknown` extension, rejects.
    pub allowed_extensions: Vec<ExtensionKind>,
    /// Older data is not trusted for a buy decision.
    pub max_snapshot_age: Duration,
    /// A time-locked LP must stay locked at least this long from now.
    pub min_lp_lock_remaining: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Accept,
    Reject(RejectReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// Observed in the future or older than `max_snapshot_age`.
    StaleSnapshot,
    /// Values no real pool has (zero price, share above 100%, ...).
    InvalidSnapshot,
    Denied,
    MintAuthorityActive,
    FreezeAuthorityActive,
    UnknownExtension(u16),
    ExtensionNotAllowed(ExtensionKind),
    ExtensionAuthorityActive(ExtensionKind),
    LpNotSecured,
    LpUnlocksTooSoon,
    LiquidityTooLow,
    PoolTooYoung,
    HolderConcentrationTooHigh,
    /// No route to sell the position back: treated as a honeypot.
    NoSellRoute,
    SellImpactTooHigh,
}

/// A learned rule set tried to loosen `field` relative to the configured rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("learned filter rule `{field}` is looser than the configured rule")]
pub struct Loosening {
    pub field: &'static str,
}

impl FilterRules {
    /// Accept `learned` only if it is at least as strict as `self` on every
    /// field; otherwise refuse and name the first looser field.
    pub fn tighten(&self, learned: FilterRules) -> Result<FilterRules, Loosening> {
        let looser = [
            (
                "min_liquidity_usd",
                learned.min_liquidity_usd < self.min_liquidity_usd,
            ),
            ("min_pool_age", learned.min_pool_age < self.min_pool_age),
            (
                "max_top_holders_share",
                learned.max_top_holders_share > self.max_top_holders_share,
            ),
            (
                "max_sell_impact_bps",
                learned.max_sell_impact_bps > self.max_sell_impact_bps,
            ),
            (
                "allowed_extensions",
                learned
                    .allowed_extensions
                    .iter()
                    .any(|kind| !self.allowed_extensions.contains(kind)),
            ),
            (
                "max_snapshot_age",
                learned.max_snapshot_age > self.max_snapshot_age,
            ),
            (
                "min_lp_lock_remaining",
                learned.min_lp_lock_remaining < self.min_lp_lock_remaining,
            ),
        ];
        match looser.iter().find(|(_, is_looser)| *is_looser) {
            Some((field, _)) => Err(Loosening { field }),
            None => Ok(learned),
        }
    }
}

/// Run every hard rule against `s`. `deny_list` holds mints the owner denied
/// or that were linked to an earlier halt.
pub fn check(
    s: &TokenSnapshot,
    rules: &FilterRules,
    deny_list: &BTreeSet<String>,
    now: OffsetDateTime,
) -> Verdict {
    match first_failure(s, rules, deny_list, now) {
        Some(reason) => Verdict::Reject(reason),
        None => Verdict::Accept,
    }
}

fn first_failure(
    s: &TokenSnapshot,
    rules: &FilterRules,
    deny_list: &BTreeSet<String>,
    now: OffsetDateTime,
) -> Option<RejectReason> {
    use RejectReason::*;

    if s.observed_at > now || now - s.observed_at > rules.max_snapshot_age {
        return Some(StaleSnapshot);
    }
    if s.price_usd <= Decimal::ZERO
        || s.liquidity_usd <= Decimal::ZERO
        || s.top_holders_share < Decimal::ZERO
        || s.top_holders_share > Decimal::ONE
        || s.pool_created_at > s.observed_at
    {
        return Some(InvalidSnapshot);
    }
    if deny_list.contains(&s.mint) {
        return Some(Denied);
    }
    if !s.mint_authority_revoked {
        return Some(MintAuthorityActive);
    }
    if !s.freeze_authority_revoked {
        return Some(FreezeAuthorityActive);
    }
    for ext in &s.extensions {
        if let ExtensionKind::Unknown(code) = ext.kind {
            return Some(UnknownExtension(code));
        }
        if !rules.allowed_extensions.contains(&ext.kind) {
            return Some(ExtensionNotAllowed(ext.kind));
        }
        if !ext.authority_revoked {
            return Some(ExtensionAuthorityActive(ext.kind));
        }
    }
    match s.lp {
        LpStatus::Burned => {}
        LpStatus::Locked { until } if until - now >= rules.min_lp_lock_remaining => {}
        LpStatus::Locked { .. } => return Some(LpUnlocksTooSoon),
        LpStatus::Unlocked | LpStatus::Unknown => return Some(LpNotSecured),
    }
    if s.liquidity_usd < rules.min_liquidity_usd {
        return Some(LiquidityTooLow);
    }
    if s.observed_at - s.pool_created_at < rules.min_pool_age {
        return Some(PoolTooYoung);
    }
    if s.top_holders_share > rules.max_top_holders_share {
        return Some(HolderConcentrationTooHigh);
    }
    match s.sell_impact_bps {
        None => Some(NoSellRoute),
        Some(bps) if bps > rules.max_sell_impact_bps => Some(SellImpactTooHigh),
        Some(_) => None,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::testutil::{d, good_snapshot, t0, Rng};
    use crate::types::Extension;

    pub fn rules() -> FilterRules {
        FilterRules {
            min_liquidity_usd: d("50000"),
            min_pool_age: Duration::hours(1),
            max_top_holders_share: d("0.30"),
            max_sell_impact_bps: 200,
            allowed_extensions: vec![ExtensionKind::MetadataPointer, ExtensionKind::TokenMetadata],
            max_snapshot_age: Duration::seconds(30),
            min_lp_lock_remaining: Duration::days(30),
        }
    }

    fn verdict(s: &TokenSnapshot) -> Verdict {
        check(s, &rules(), &BTreeSet::new(), t0())
    }

    fn ext(kind: ExtensionKind, authority_revoked: bool) -> Extension {
        Extension {
            kind,
            authority_revoked,
        }
    }

    #[test]
    fn good_snapshot_passes() {
        assert_eq!(verdict(&good_snapshot()), Verdict::Accept);
    }

    #[test]
    fn each_rule_rejects_with_its_reason() {
        use RejectReason::*;
        type Mutation = fn(&mut TokenSnapshot);
        let cases: &[(Mutation, RejectReason)] = &[
            (
                |s| s.observed_at = t0() + Duration::seconds(1),
                StaleSnapshot,
            ),
            (
                |s| s.observed_at = t0() - Duration::seconds(31),
                StaleSnapshot,
            ),
            (|s| s.price_usd = Decimal::ZERO, InvalidSnapshot),
            (|s| s.liquidity_usd = d("-1"), InvalidSnapshot),
            (|s| s.top_holders_share = d("1.01"), InvalidSnapshot),
            (
                |s| s.pool_created_at = t0() + Duration::seconds(1),
                InvalidSnapshot,
            ),
            (|s| s.mint_authority_revoked = false, MintAuthorityActive),
            (
                |s| s.freeze_authority_revoked = false,
                FreezeAuthorityActive,
            ),
            (
                |s| s.extensions = vec![ext(ExtensionKind::Unknown(99), true)],
                UnknownExtension(99),
            ),
            (
                |s| s.extensions = vec![ext(ExtensionKind::PermanentDelegate, true)],
                ExtensionNotAllowed(ExtensionKind::PermanentDelegate),
            ),
            (
                |s| s.extensions = vec![ext(ExtensionKind::MetadataPointer, false)],
                ExtensionAuthorityActive(ExtensionKind::MetadataPointer),
            ),
            (|s| s.lp = LpStatus::Unlocked, LpNotSecured),
            (|s| s.lp = LpStatus::Unknown, LpNotSecured),
            (
                |s| {
                    s.lp = LpStatus::Locked {
                        until: t0() + Duration::days(29),
                    }
                },
                LpUnlocksTooSoon,
            ),
            (|s| s.liquidity_usd = d("49999.99"), LiquidityTooLow),
            (
                |s| s.pool_created_at = t0() - Duration::minutes(59),
                PoolTooYoung,
            ),
            (
                |s| s.top_holders_share = d("0.31"),
                HolderConcentrationTooHigh,
            ),
            (|s| s.sell_impact_bps = None, NoSellRoute),
            (|s| s.sell_impact_bps = Some(201), SellImpactTooHigh),
        ];
        for (mutate, reason) in cases {
            let mut s = good_snapshot();
            mutate(&mut s);
            assert_eq!(verdict(&s), Verdict::Reject(*reason), "{reason:?}");
        }
    }

    #[test]
    fn boundaries_and_allowed_extensions_pass() {
        let mut s = good_snapshot();
        s.observed_at = t0() - Duration::seconds(30);
        s.pool_created_at = s.observed_at - Duration::hours(1);
        s.liquidity_usd = d("50000");
        s.top_holders_share = d("0.30");
        s.sell_impact_bps = Some(200);
        s.lp = LpStatus::Locked {
            until: t0() + Duration::days(30),
        };
        s.extensions = vec![
            ext(ExtensionKind::MetadataPointer, true),
            ext(ExtensionKind::TokenMetadata, true),
        ];
        assert_eq!(verdict(&s), Verdict::Accept);
    }

    #[test]
    fn deny_list_rejects() {
        let deny = BTreeSet::from(["MintGood111".to_owned()]);
        assert_eq!(
            check(&good_snapshot(), &rules(), &deny, t0()),
            Verdict::Reject(RejectReason::Denied)
        );
    }

    #[test]
    fn promotion_fields_never_change_the_verdict() {
        let mut rng = Rng::new(7);
        for i in 0..500 {
            let mut base = good_snapshot();
            // Mix accepted and rejected candidates.
            base.liquidity_usd = rng.decimal(100_000, 2) + Decimal::ONE;
            base.sell_impact_bps = (i % 5 != 0).then(|| rng.below(400) as u32);
            let mut promoted = base.clone();
            promoted.boosts = rng.next() as u32;
            promoted.trending_rank = Some(rng.below(10) as u32 + 1);
            assert_eq!(verdict(&base), verdict(&promoted));
        }
    }

    #[test]
    fn tighten_accepts_equal_or_stricter() {
        let base = rules();
        assert_eq!(base.tighten(base.clone()), Ok(base.clone()));
        let stricter = FilterRules {
            min_liquidity_usd: d("80000"),
            min_pool_age: Duration::hours(2),
            max_top_holders_share: d("0.2"),
            max_sell_impact_bps: 100,
            allowed_extensions: vec![ExtensionKind::TokenMetadata],
            max_snapshot_age: Duration::seconds(10),
            min_lp_lock_remaining: Duration::days(60),
        };
        assert_eq!(base.tighten(stricter.clone()), Ok(stricter));
    }

    #[test]
    fn tighten_refuses_every_loosening() {
        type Mutation = fn(&mut FilterRules);
        let cases: &[(Mutation, &str)] = &[
            (|r| r.min_liquidity_usd = d("49999"), "min_liquidity_usd"),
            (|r| r.min_pool_age = Duration::minutes(59), "min_pool_age"),
            (
                |r| r.max_top_holders_share = d("0.31"),
                "max_top_holders_share",
            ),
            (|r| r.max_sell_impact_bps = 201, "max_sell_impact_bps"),
            (
                |r| r.allowed_extensions.push(ExtensionKind::TransferHook),
                "allowed_extensions",
            ),
            (
                |r| r.max_snapshot_age = Duration::seconds(31),
                "max_snapshot_age",
            ),
            (
                |r| r.min_lp_lock_remaining = Duration::days(29),
                "min_lp_lock_remaining",
            ),
        ];
        for (mutate, field) in cases {
            let mut learned = rules();
            mutate(&mut learned);
            assert_eq!(rules().tighten(learned), Err(Loosening { field }));
        }
    }
}
