//! Research adjustment: the only place LLM output touches a trade.
//!
//! The Memecoin Trader agent reads hostile web content (token pages, socials,
//! news) and can be prompt-injected. So its typed, schema-validated verdict is
//! allowed exactly one direction: keep the size, make it smaller, or veto.
//! There is no variant that grows a position or creates one, so even a fully
//! compromised agent can only make the system *less* willing to buy.

use rust_decimal::Decimal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResearchAdjustment {
    Keep,
    /// Keep this share of the size, in basis points; values above 10 000 are
    /// treated as 10 000 (no change).
    Reduce {
        keep_bps: u16,
    },
    Veto,
}

/// Apply the agent's verdict to an entry size. The result is always between
/// zero and `size`; a negative input size returns zero.
pub fn apply_research(size: Decimal, adjustment: ResearchAdjustment) -> Decimal {
    if size <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    let reduced = match adjustment {
        ResearchAdjustment::Keep => size,
        ResearchAdjustment::Veto => Decimal::ZERO,
        ResearchAdjustment::Reduce { keep_bps } => {
            size * Decimal::new(i64::from(keep_bps.min(10_000)), 4)
        }
    };
    // Belt and braces against rounding: never above the input.
    reduced.min(size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{d, Rng};

    #[test]
    fn examples() {
        assert_eq!(apply_research(d("25"), ResearchAdjustment::Keep), d("25"));
        assert_eq!(apply_research(d("25"), ResearchAdjustment::Veto), d("0"));
        assert_eq!(
            apply_research(d("25"), ResearchAdjustment::Reduce { keep_bps: 4_000 }),
            d("10")
        );
        assert_eq!(
            apply_research(d("25"), ResearchAdjustment::Reduce { keep_bps: u16::MAX }),
            d("25")
        );
        assert_eq!(apply_research(d("-5"), ResearchAdjustment::Keep), d("0"));
    }

    #[test]
    fn never_grows_and_never_goes_negative() {
        let mut rng = Rng::new(42);
        for i in 0..5_000 {
            let mut size = rng.decimal(1_000_000, 8);
            if i % 7 == 0 {
                size = -size;
            }
            if i % 101 == 0 {
                size = Decimal::MAX;
            }
            let adjustment = match rng.below(3) {
                0 => ResearchAdjustment::Keep,
                1 => ResearchAdjustment::Veto,
                _ => ResearchAdjustment::Reduce {
                    keep_bps: rng.next() as u16,
                },
            };
            let out = apply_research(size, adjustment);
            assert!(out >= Decimal::ZERO, "{size} {adjustment:?}");
            assert!(out <= size.max(Decimal::ZERO), "{size} {adjustment:?}");
        }
    }
}
