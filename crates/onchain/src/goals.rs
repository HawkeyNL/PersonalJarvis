//! Progress toward the owner's goal, such as 2x or 20x the stake.
//!
//! The hot wallet is capped at the stake (the mandate's capital cap).
//! Whatever it earns above the cap is swept to the owner's own address
//! continuously, so the principal keeps trading and the profit is already
//! home. That is why the wallet balance alone says little, and why the
//! multiple counts both: `(wallet value + total swept) / stake`.
//!
//! Because profit leaves as it is made, there is no separate "take the
//! profit" step at the goal. The owner picks in advance between two
//! [`GoalAction`]s: keep running, or stop (no new entries, exit every
//! position, sweep the whole wallet). This module only calculates; the signer
//! is the only component that can actually exit, sweep or stop.

use rust_decimal::Decimal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// `(wallet + swept) / stake`, e.g. `1.34`.
    pub multiple: Decimal,
    /// Share of the way from 1x to the goal, clamped to `0..=1`.
    pub fraction_to_goal: Decimal,
    /// USD still needed to reach the goal; zero once reached.
    pub remaining_usd: Decimal,
    pub reached: bool,
}

/// `None` for inputs that have no meaning: a non-positive stake, negative
/// balances, or a goal of 1x or less.
pub fn progress(
    stake_usd: Decimal,
    wallet_usd: Decimal,
    swept_usd: Decimal,
    goal: Decimal,
) -> Option<Progress> {
    if stake_usd <= Decimal::ZERO
        || wallet_usd < Decimal::ZERO
        || swept_usd < Decimal::ZERO
        || goal <= Decimal::ONE
    {
        return None;
    }
    let total = wallet_usd.checked_add(swept_usd)?;
    let multiple = total.checked_div(stake_usd)?;
    let target = stake_usd.checked_mul(goal)?;
    let fraction_to_goal = (multiple - Decimal::ONE)
        .checked_div(goal - Decimal::ONE)?
        .clamp(Decimal::ZERO, Decimal::ONE);
    Some(Progress {
        multiple,
        fraction_to_goal,
        remaining_usd: (target - total).max(Decimal::ZERO),
        reached: total >= target,
    })
}

/// What happens at the goal; also the owner's choice made in advance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalAction {
    /// Keep trading within the cap; profit keeps being swept home.
    KeepRunning,
    /// Open nothing new, exit every position and sweep the whole wallet.
    Stop,
}

/// The action for the current state: always `KeepRunning` before the goal,
/// the owner's `at_goal` choice once it is reached. `None` for inputs
/// [`progress`] rejects.
pub fn action(
    stake_usd: Decimal,
    wallet_usd: Decimal,
    swept_usd: Decimal,
    goal: Decimal,
    at_goal: GoalAction,
) -> Option<GoalAction> {
    let reached = progress(stake_usd, wallet_usd, swept_usd, goal)?.reached;
    Some(if reached {
        at_goal
    } else {
        GoalAction::KeepRunning
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::d;

    #[test]
    fn progress_counts_wallet_and_swept() {
        let p = progress(d("100"), d("100"), d("34"), d("2")).unwrap();
        assert_eq!(p.multiple, d("1.34"));
        assert_eq!(p.fraction_to_goal, d("0.34"));
        assert_eq!(p.remaining_usd, d("66"));
        assert!(!p.reached);

        let p = progress(d("100"), d("100"), d("100"), d("2")).unwrap();
        assert!(p.reached);
        assert_eq!(p.fraction_to_goal, Decimal::ONE);
        assert_eq!(p.remaining_usd, Decimal::ZERO);

        let p = progress(d("100"), d("20"), d("0"), d("20")).unwrap();
        assert_eq!(p.multiple, d("0.2"));
        assert_eq!(
            p.fraction_to_goal,
            Decimal::ZERO,
            "a loss shows no progress"
        );
    }

    #[test]
    fn meaningless_inputs_return_none() {
        assert_eq!(progress(d("0"), d("1"), d("1"), d("2")), None);
        assert_eq!(progress(d("100"), d("-1"), d("1"), d("2")), None);
        assert_eq!(progress(d("100"), d("1"), d("-1"), d("2")), None);
        assert_eq!(progress(d("100"), d("1"), d("1"), d("1")), None);
        assert_eq!(
            action(d("0"), d("1"), d("1"), d("2"), GoalAction::Stop),
            None
        );
        // A goal a hair above 1x overflows the division: no panic, no answer.
        let tiny = d("1.0000000000000000000000000001");
        assert_eq!(progress(d("1"), d("1000000"), d("0"), tiny), None);
    }

    #[test]
    fn owner_choice_applies_only_once_reached() {
        use GoalAction::*;
        for at_goal in [KeepRunning, Stop] {
            assert_eq!(
                action(d("100"), d("100"), d("99.99"), d("2"), at_goal),
                Some(KeepRunning)
            );
            assert_eq!(
                action(d("100"), d("100"), d("100"), d("2"), at_goal),
                Some(at_goal)
            );
        }
        // 20x is far away even with a full wallet and some profit home.
        assert_eq!(
            action(d("100"), d("100"), d("500"), d("20"), Stop),
            Some(KeepRunning)
        );
    }
}
