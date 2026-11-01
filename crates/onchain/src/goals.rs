//! Progress toward the owner's goal, such as 2x or 20x the stake.
//!
//! The hot wallet never holds more than the stake: everything above it is
//! swept to the owner's own address. So the wallet balance alone says little.
//! The multiple counts both: `(wallet value + total swept) / stake`.
//!
//! When the goal is reached the owner has chosen in advance what happens
//! ([`AtGoal`]). This module only calculates the resulting [`GoalAction`]; the
//! signer is the only component that can actually sweep or stop.

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

/// What the owner wants once the goal is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtGoal {
    /// Only notify; keep trading within the stake.
    KeepRunning,
    /// Send the stake home, keep trading with the profit.
    SweepPrincipal,
    /// Send everything home and open nothing new until a new mandate.
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalAction {
    KeepRunning,
    /// Sweep this much to the owner's address, then keep running.
    Sweep {
        usd: Decimal,
    },
    /// Sweep the whole wallet and stop entering positions.
    SweepAllAndStop {
        usd: Decimal,
    },
}

/// The action for the current state. Before the goal it is always
/// `KeepRunning`.
///
/// `swept_usd` includes the routine profit sweeps above the stake, so it
/// cannot say how much of the stake itself came home. That is
/// `principal_returned_usd` (part of `swept_usd`). `SweepPrincipal` sweeps
/// only the stake not yet returned, so repeating the call never sweeps it
/// twice.
pub fn action(
    stake_usd: Decimal,
    wallet_usd: Decimal,
    swept_usd: Decimal,
    principal_returned_usd: Decimal,
    goal: Decimal,
    at_goal: AtGoal,
) -> Option<GoalAction> {
    if principal_returned_usd < Decimal::ZERO || principal_returned_usd > swept_usd {
        return None;
    }
    let p = progress(stake_usd, wallet_usd, swept_usd, goal)?;
    if !p.reached {
        return Some(GoalAction::KeepRunning);
    }
    Some(match at_goal {
        AtGoal::KeepRunning => GoalAction::KeepRunning,
        AtGoal::SweepPrincipal => {
            let usd = (stake_usd - principal_returned_usd)
                .max(Decimal::ZERO)
                .min(wallet_usd);
            if usd.is_zero() {
                GoalAction::KeepRunning
            } else {
                GoalAction::Sweep { usd }
            }
        }
        AtGoal::Stop => GoalAction::SweepAllAndStop { usd: wallet_usd },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{d, Rng};

    #[test]
    fn progress_counts_wallet_and_swept() {
        let p = progress(d("100"), d("100"), d("34"), d("2")).unwrap();
        assert_eq!(p.multiple, d("1.34"));
        assert_eq!(p.fraction_to_goal, d("0.34"));
        assert_eq!(p.remaining_usd, d("66"));
        assert!(!p.reached);

        let p = progress(d("100"), d("40"), d("160"), d("2")).unwrap();
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
            action(d("0"), d("1"), d("1"), d("0"), d("2"), AtGoal::Stop),
            None
        );
        assert_eq!(
            action(d("100"), d("1"), d("1"), d("2"), d("2"), AtGoal::Stop),
            None
        );
        assert_eq!(
            action(d("100"), d("1"), d("1"), d("-1"), d("2"), AtGoal::Stop),
            None
        );
        // A goal a hair above 1x overflows the division: no panic, no answer.
        let tiny = d("1.0000000000000000000000000001");
        assert_eq!(progress(d("1"), d("1000000"), d("0"), tiny), None);
    }

    #[test]
    fn actions_at_and_before_goal() {
        use GoalAction::*;
        let before = |at| action(d("100"), d("100"), d("34"), d("0"), d("2"), at);
        for at in [AtGoal::KeepRunning, AtGoal::SweepPrincipal, AtGoal::Stop] {
            assert_eq!(before(at), Some(KeepRunning));
        }
        let reached =
            |returned: &str, at| action(d("100"), d("100"), d("100"), d(returned), d("2"), at);
        assert_eq!(reached("100", AtGoal::KeepRunning), Some(KeepRunning));
        assert_eq!(
            reached("100", AtGoal::Stop),
            Some(SweepAllAndStop { usd: d("100") })
        );
        // 100 of profit was swept above the stake, but none of the stake.
        assert_eq!(
            reached("0", AtGoal::SweepPrincipal),
            Some(Sweep { usd: d("100") })
        );
        // 40 of the stake already came home.
        assert_eq!(
            action(
                d("100"),
                d("160"),
                d("40"),
                d("40"),
                d("2"),
                AtGoal::SweepPrincipal
            ),
            Some(Sweep { usd: d("60") })
        );
        // Stake already fully home: nothing left to sweep.
        assert_eq!(reached("100", AtGoal::SweepPrincipal), Some(KeepRunning));
    }

    #[test]
    fn sweep_never_exceeds_wallet_or_outstanding_stake() {
        let mut rng = Rng::new(21);
        for _ in 0..2_000 {
            let stake = rng.decimal(10_000, 2) + d("0.01");
            let wallet = rng.decimal(20_000, 2);
            let swept = rng.decimal(200_000, 2);
            let returned = rng.decimal(20_000, 2).min(swept);
            let goal = rng.decimal(30, 1) + d("1.1");
            match action(stake, wallet, swept, returned, goal, AtGoal::SweepPrincipal) {
                Some(GoalAction::Sweep { usd }) => {
                    assert!(usd > Decimal::ZERO && usd <= wallet && usd <= stake - returned);
                }
                Some(GoalAction::KeepRunning) => {}
                other => panic!("unexpected {other:?}"),
            }
        }
    }
}
