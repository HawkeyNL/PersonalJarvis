//! Paper trading: simulated fills and a lot ledger with net P&L.
//!
//! Paper results always look better than live ones. To keep the gap small,
//! fills here are **size-aware**: the order moves the price against itself
//! along a constant-product pool (`x * y = k`) built from the snapshot's
//! liquidity, so a bigger order always gets a worse average price. On top of
//! that every attempt pays network and priority fees, and a caller-supplied
//! random `roll` decides whether the transaction failed (fees paid, nothing
//! filled). The roll is an input so a backtest can be replayed exactly.
//!
//! The ledger keeps one lot per buy, sells first-in-first-out, and reports
//! P&L net of entry and exit fees, failed transactions and attributed AI
//! cost. A *trade* is a full round trip: from the first buy of a mint until
//! it is completely sold, so splitting an exit never inflates the count.
//! Open lots are valued at what selling them would return (pool fee and
//! price impact included), never at the mid price.
//!
//! Every curve output is rounded down to 12 decimals, so a simulated fill
//! never credits more than the curve gives.

use std::collections::BTreeMap;

use rust_decimal::{Decimal, RoundingStrategy};
use time::OffsetDateTime;

use crate::types::Position;

const BPS: Decimal = Decimal::from_parts(10_000, 0, 0, false, 0);

/// Execution costs applied to every simulated attempt.
#[derive(Debug, Clone, PartialEq)]
pub struct CostModel {
    /// Pool fee taken from the input amount.
    pub dex_fee_bps: u32,
    /// Base network fee per attempt, in USD.
    pub network_fee_usd: Decimal,
    /// Priority fee per attempt, in USD.
    pub priority_fee_usd: Decimal,
    /// Chance an attempt fails, as a fraction in `0..=1`.
    pub failure_rate: Decimal,
}

impl CostModel {
    /// Pool fee below 100%, fees non-negative, failure rate within `0..=1`.
    pub fn is_valid(&self) -> bool {
        self.dex_fee_bps < 10_000
            && self.network_fee_usd >= Decimal::ZERO
            && self.priority_fee_usd >= Decimal::ZERO
            && (Decimal::ZERO..=Decimal::ONE).contains(&self.failure_rate)
    }
}

/// Outcome of one simulated attempt. Pool fee and price impact are already
/// inside `amount_out`; `fees_usd` is what the attempt cost on top.
#[derive(Debug, Clone, PartialEq)]
pub enum Fill {
    Filled {
        amount_out: Decimal,
        fees_usd: Decimal,
    },
    Failed {
        fees_usd: Decimal,
    },
}

/// Simulate buying with `usd_in`. Returns tokens received. `None` when the
/// inputs cannot be priced (non-positive values, invalid cost model, or
/// arithmetic overflow from absurd data): fail closed, no fill.
pub fn buy(
    usd_in: Decimal,
    price_usd: Decimal,
    liquidity_usd: Decimal,
    costs: &CostModel,
    roll: Decimal,
) -> Option<Fill> {
    let (quote_reserve, token_reserve) = reserves(price_usd, liquidity_usd)?;
    attempt(costs, roll, || {
        swap_out(usd_in, quote_reserve, token_reserve, costs)
    })
}

/// Simulate selling `tokens`. Returns USD received. `None` as for [`buy`].
pub fn sell(
    tokens: Decimal,
    price_usd: Decimal,
    liquidity_usd: Decimal,
    costs: &CostModel,
    roll: Decimal,
) -> Option<Fill> {
    let (quote_reserve, token_reserve) = reserves(price_usd, liquidity_usd)?;
    attempt(costs, roll, || {
        swap_out(tokens, token_reserve, quote_reserve, costs)
    })
}

/// What selling `tokens` would return after the pool fee and price impact,
/// before transaction fees. Used to value positions at an executable price.
/// A collapsed price can value a position at exactly zero. `None` for an
/// invalid cost model or inputs that cannot be priced.
pub fn sell_value(
    tokens: Decimal,
    price_usd: Decimal,
    liquidity_usd: Decimal,
    costs: &CostModel,
) -> Option<Decimal> {
    if !costs.is_valid() {
        return None;
    }
    let (quote_reserve, token_reserve) = reserves(price_usd, liquidity_usd)?;
    swap_out(tokens, token_reserve, quote_reserve, costs)
}

/// Half the pool liquidity sits on each side of a constant-product pool.
fn reserves(price_usd: Decimal, liquidity_usd: Decimal) -> Option<(Decimal, Decimal)> {
    if price_usd <= Decimal::ZERO || liquidity_usd <= Decimal::ZERO {
        return None;
    }
    let quote_reserve = liquidity_usd / Decimal::TWO;
    let token_reserve = quote_reserve.checked_div(price_usd)?;
    Some((quote_reserve, token_reserve))
}

fn attempt(
    costs: &CostModel,
    roll: Decimal,
    fill: impl FnOnce() -> Option<Decimal>,
) -> Option<Fill> {
    if !costs.is_valid() || !(Decimal::ZERO..Decimal::ONE).contains(&roll) {
        return None;
    }
    let fees_usd = costs.network_fee_usd.checked_add(costs.priority_fee_usd)?;
    // An order too small to receive anything does not fill.
    let amount_out = fill().filter(|out| *out > Decimal::ZERO)?;
    if roll < costs.failure_rate {
        return Some(Fill::Failed { fees_usd });
    }
    Some(Fill::Filled {
        amount_out,
        fees_usd,
    })
}

fn swap_out(
    amount_in: Decimal,
    reserve_in: Decimal,
    reserve_out: Decimal,
    costs: &CostModel,
) -> Option<Decimal> {
    let after_fee = amount_in.checked_mul(BPS - Decimal::from(costs.dex_fee_bps))? / BPS;
    curve_out(after_fee, reserve_in, reserve_out)
}

/// Constant-product output: `out = reserve_out * in / (reserve_in + in)`.
fn curve_out(amount_in: Decimal, reserve_in: Decimal, reserve_out: Decimal) -> Option<Decimal> {
    if amount_in <= Decimal::ZERO {
        return None;
    }
    let out = reserve_out
        .checked_mul(amount_in)?
        .checked_div(reserve_in.checked_add(amount_in)?)?
        .round_dp_with_strategy(12, RoundingStrategy::ToZero);
    Some(out)
}

/// Current price and liquidity used to value open lots.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mark {
    pub price_usd: Decimal,
    pub liquidity_usd: Decimal,
}

/// One completed round trip: from the first buy of a mint until the
/// position in it is fully sold, with every fee that belongs to it.
#[derive(Debug, Clone, PartialEq)]
pub struct ClosedTrade {
    pub mint: String,
    /// Open time of the oldest lot in the round trip.
    pub opened_at: OffsetDateTime,
    /// Time of the sell that emptied the position.
    pub closed_at: OffsetDateTime,
    /// Sum over its sells of `usd_out - exit fees - cost basis (incl. entry fees)`.
    pub net_pnl_usd: Decimal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LedgerError {
    #[error("amount must be positive and fees non-negative")]
    InvalidAmount,
    #[error("sell exceeds the tokens held in open lots")]
    InsufficientTokens,
    /// Events must be recorded in time order (see [`Ledger`]).
    #[error("event is dated before the previous ledger event")]
    OutOfOrder,
    #[error("amounts too large to account for")]
    Overflow,
}

/// Net result. `net_usd = realised + unrealised - failed tx fees - AI cost`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pnl {
    /// Completed round trips plus partial sells of positions still open.
    pub realised_usd: Decimal,
    /// Open lots at executable value minus their cost (exit tx fees not included).
    pub unrealised_usd: Decimal,
    pub failed_tx_fees_usd: Decimal,
    pub ai_cost_usd: Decimal,
    pub net_usd: Decimal,
}

/// The ledger is fed from simulated fills on untrusted market data, so every
/// sum is checked: absurd values give [`LedgerError::Overflow`], not a panic.
///
/// Buys and sells must arrive in time order: an event dated before the
/// previous one is refused, so the record cannot be back-dated and a
/// back-dated buy cannot stretch it. Forward-dated events are not detected
/// here; whoever feeds the ledger must stamp events with its own clock.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Ledger {
    lots: Vec<Position>,
    closed: Vec<ClosedTrade>,
    /// Round trips still open: realised P&L of partial sells and the open
    /// time of their oldest lot, per mint.
    open_trips: BTreeMap<String, (Decimal, OffsetDateTime)>,
    failed_tx_fees_usd: Decimal,
    ai_cost_usd: Decimal,
    last_event_at: Option<OffsetDateTime>,
}

impl Ledger {
    pub fn lots(&self) -> &[Position] {
        &self.lots
    }

    /// Completed round trips only.
    pub fn closed_trades(&self) -> &[ClosedTrade] {
        &self.closed
    }

    pub fn failed_tx_fees_usd(&self) -> Decimal {
        self.failed_tx_fees_usd
    }

    pub fn ai_cost_usd(&self) -> Decimal {
        self.ai_cost_usd
    }

    /// Record a filled buy. `cost_usd` must already include its fees.
    pub fn open(&mut self, lot: Position) -> Result<(), LedgerError> {
        if lot.tokens <= Decimal::ZERO || lot.cost_usd < Decimal::ZERO {
            return Err(LedgerError::InvalidAmount);
        }
        self.check_order(lot.opened_at)?;
        self.last_event_at = Some(lot.opened_at);
        self.lots.push(lot);
        Ok(())
    }

    fn check_order(&self, at: OffsetDateTime) -> Result<(), LedgerError> {
        match self.last_event_at {
            Some(last) if at < last => Err(LedgerError::OutOfOrder),
            _ => Ok(()),
        }
    }

    /// Record a filled sell of `tokens` of `mint`, consuming lots oldest
    /// first. Returns this sell's net P&L. When it empties the position the
    /// round trip moves to [`Ledger::closed_trades`]. On error nothing
    /// changes.
    pub fn close(
        &mut self,
        mint: &str,
        tokens: Decimal,
        usd_out: Decimal,
        fees_usd: Decimal,
        at: OffsetDateTime,
    ) -> Result<Decimal, LedgerError> {
        if tokens <= Decimal::ZERO || usd_out < Decimal::ZERO || fees_usd < Decimal::ZERO {
            return Err(LedgerError::InvalidAmount);
        }
        self.check_order(at)?;
        let held = checked_sum(
            self.lots
                .iter()
                .filter(|l| l.mint == mint)
                .map(|l| l.tokens),
        )?;
        if tokens > held {
            return Err(LedgerError::InsufficientTokens);
        }
        // Work on a copy so a failure halfway leaves the ledger untouched.
        let mut lots = self.lots.clone();
        let mut left = tokens;
        let mut cost = Decimal::ZERO;
        let mut oldest = None;
        for lot in lots.iter_mut().filter(|l| l.mint == mint) {
            if left.is_zero() {
                break;
            }
            oldest.get_or_insert(lot.opened_at);
            let take = left.min(lot.tokens);
            // A full lot takes its exact remaining cost; a partial share is
            // rounded to a fixed scale (and never above what is left) so the
            // subtraction stays exact. Together the shares add up to what
            // was paid.
            let share = if take == lot.tokens {
                lot.cost_usd
            } else {
                lot.cost_usd
                    .checked_mul(take / lot.tokens)
                    .ok_or(LedgerError::Overflow)?
                    .round_dp(12)
                    .min(lot.cost_usd)
            };
            lot.tokens -= take;
            lot.cost_usd -= share;
            cost = cost.checked_add(share).ok_or(LedgerError::Overflow)?;
            left -= take;
        }
        let net_pnl_usd = usd_out
            .checked_sub(fees_usd)
            .and_then(|v| v.checked_sub(cost))
            .ok_or(LedgerError::Overflow)?;
        let (trip_pnl, trip_opened) = match self.open_trips.get(mint) {
            Some(&(pnl, opened)) => (pnl, opened),
            None => (Decimal::ZERO, oldest.unwrap_or(at)),
        };
        let trip_pnl = trip_pnl
            .checked_add(net_pnl_usd)
            .ok_or(LedgerError::Overflow)?;

        lots.retain(|l| !l.tokens.is_zero());
        let still_held = lots.iter().any(|l| l.mint == mint);
        self.lots = lots;
        self.last_event_at = Some(at);
        if still_held {
            self.open_trips
                .insert(mint.to_owned(), (trip_pnl, trip_opened));
        } else {
            self.open_trips.remove(mint);
            self.closed.push(ClosedTrade {
                mint: mint.to_owned(),
                opened_at: trip_opened,
                closed_at: at,
                net_pnl_usd: trip_pnl,
            });
        }
        Ok(net_pnl_usd)
    }

    /// Fees of an attempt that failed on chain: a real loss with no fill.
    pub fn record_failed_tx(&mut self, fees_usd: Decimal) -> Result<(), LedgerError> {
        self.failed_tx_fees_usd = add_cost(self.failed_tx_fees_usd, fees_usd)?;
        Ok(())
    }

    /// AI spend attributed to this trader (research, vetoes). Counted as a
    /// cost so the trader is judged on what it really earns.
    pub fn record_ai_cost(&mut self, usd: Decimal) -> Result<(), LedgerError> {
        self.ai_cost_usd = add_cost(self.ai_cost_usd, usd)?;
        Ok(())
    }

    /// Net P&L with open lots valued per mint at `marks`, after the pool fee
    /// and price impact of `costs`. A mint without a usable mark is valued
    /// at zero: unknown is treated as lost.
    pub fn pnl(
        &self,
        marks: &BTreeMap<String, Mark>,
        costs: &CostModel,
    ) -> Result<Pnl, LedgerError> {
        let closed = checked_sum(self.closed.iter().map(|t| t.net_pnl_usd))?;
        let partial = checked_sum(self.open_trips.values().map(|(pnl, _)| *pnl))?;
        let realised_usd = closed.checked_add(partial).ok_or(LedgerError::Overflow)?;
        let unrealised_usd = checked_sum(
            self.open_by_mint(marks, costs)?
                .into_iter()
                .map(|(_, unrealised)| unrealised),
        )?;
        let net_usd = checked_sum(
            [
                realised_usd,
                unrealised_usd,
                -self.failed_tx_fees_usd,
                -self.ai_cost_usd,
            ]
            .into_iter(),
        )?;
        Ok(Pnl {
            realised_usd,
            unrealised_usd,
            failed_tx_fees_usd: self.failed_tx_fees_usd,
            ai_cost_usd: self.ai_cost_usd,
            net_usd,
        })
    }

    /// Sum of the losing open round trips: per mint, partial-sell P&L plus
    /// unrealised P&L, counted only when negative. Open winners never offset
    /// open losers.
    pub fn open_losses(
        &self,
        marks: &BTreeMap<String, Mark>,
        costs: &CostModel,
    ) -> Result<Decimal, LedgerError> {
        let mut total = Decimal::ZERO;
        for (mint, unrealised) in self.open_by_mint(marks, costs)? {
            let partial = self
                .open_trips
                .get(mint)
                .map_or(Decimal::ZERO, |(pnl, _)| *pnl);
            let trip = partial
                .checked_add(unrealised)
                .ok_or(LedgerError::Overflow)?;
            total = total
                .checked_add(trip.min(Decimal::ZERO))
                .ok_or(LedgerError::Overflow)?;
        }
        Ok(total)
    }

    /// Unrealised P&L per held mint.
    fn open_by_mint(
        &self,
        marks: &BTreeMap<String, Mark>,
        costs: &CostModel,
    ) -> Result<Vec<(&str, Decimal)>, LedgerError> {
        let mut open: BTreeMap<&str, (Decimal, Decimal)> = BTreeMap::new();
        for lot in &self.lots {
            let (tokens, cost) = open.entry(lot.mint.as_str()).or_default();
            *tokens = tokens
                .checked_add(lot.tokens)
                .ok_or(LedgerError::Overflow)?;
            *cost = cost
                .checked_add(lot.cost_usd)
                .ok_or(LedgerError::Overflow)?;
        }
        Ok(open
            .into_iter()
            .map(|(mint, (tokens, cost))| {
                let value = marks
                    .get(mint)
                    .and_then(|m| sell_value(tokens, m.price_usd, m.liquidity_usd, costs))
                    .unwrap_or(Decimal::ZERO);
                (mint, value - cost)
            })
            .collect())
    }
}

fn add_cost(total: Decimal, usd: Decimal) -> Result<Decimal, LedgerError> {
    if usd < Decimal::ZERO {
        return Err(LedgerError::InvalidAmount);
    }
    total.checked_add(usd).ok_or(LedgerError::Overflow)
}

pub(crate) fn checked_sum(
    mut values: impl Iterator<Item = Decimal>,
) -> Result<Decimal, LedgerError> {
    values.try_fold(Decimal::ZERO, |acc, v| {
        acc.checked_add(v).ok_or(LedgerError::Overflow)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{costs, d, t0, Rng};
    use time::Duration;

    fn filled(fill: Option<Fill>) -> (Decimal, Decimal) {
        match fill {
            Some(Fill::Filled {
                amount_out,
                fees_usd,
            }) => (amount_out, fees_usd),
            other => panic!("expected a fill, got {other:?}"),
        }
    }

    #[test]
    fn small_buy_is_close_to_mid_price_minus_fee() {
        let (tokens, fees) = filled(buy(d("1"), d("0.001"), d("2000000"), &costs(), d("0.5")));
        // 1 USD at 0.001 is 1000 tokens; 0.25% pool fee and ~0.0001% impact.
        assert!(tokens < d("997.5") && tokens > d("997.4"), "{tokens}");
        assert_eq!(fees, d("0.011"));
    }

    #[test]
    fn bigger_orders_get_worse_average_prices() {
        let mut rng = Rng::new(11);
        for _ in 0..500 {
            let price = rng.decimal(10, 6) + d("0.000001");
            let liquidity = rng.decimal(1_000_000, 2) + d("1000");
            let small = rng.decimal(10_000, 2) + d("0.01");
            let big = small + rng.decimal(10_000, 2) + d("0.01");

            let (small_tokens, _) = filled(buy(small, price, liquidity, &costs(), d("0.5")));
            let (big_tokens, _) = filled(buy(big, price, liquidity, &costs(), d("0.5")));
            assert!(
                big / big_tokens > small / small_tokens,
                "buy price must worsen"
            );

            let small_q = small / price;
            let big_q = big / price;
            let (small_usd, _) = filled(sell(small_q, price, liquidity, &costs(), d("0.5")));
            let (big_usd, _) = filled(sell(big_q, price, liquidity, &costs(), d("0.5")));
            assert!(
                big_usd / big_q < small_usd / small_q,
                "sell price must worsen"
            );
            // Never get more than mid value back.
            assert!(small_usd < small_q * price);
        }
    }

    #[test]
    fn roll_below_failure_rate_fails_and_still_pays_fees() {
        assert_eq!(
            buy(d("10"), d("0.001"), d("100000"), &costs(), d("0.05")),
            Some(Fill::Failed {
                fees_usd: d("0.011")
            })
        );
        assert!(matches!(
            buy(d("10"), d("0.001"), d("100000"), &costs(), d("0.1")),
            Some(Fill::Filled { .. })
        ));
    }

    #[test]
    fn unpriceable_inputs_do_not_fill() {
        let bad_costs = CostModel {
            failure_rate: d("1.5"),
            ..costs()
        };
        assert_eq!(buy(d("10"), d("0"), d("100000"), &costs(), d("0.5")), None);
        assert_eq!(buy(d("10"), d("0.001"), d("0"), &costs(), d("0.5")), None);
        assert_eq!(
            buy(d("0"), d("0.001"), d("100000"), &costs(), d("0.5")),
            None
        );
        assert_eq!(
            buy(d("10"), d("0.001"), d("100000"), &costs(), d("1")),
            None
        );
        assert_eq!(
            buy(d("10"), d("0.001"), d("100000"), &bad_costs, d("0.5")),
            None
        );
        // Absurd hostile values overflow checked math instead of panicking.
        assert_eq!(
            buy(
                Decimal::MAX,
                d("0.0000001"),
                Decimal::MAX,
                &costs(),
                d("0.5")
            ),
            None
        );
    }

    fn lot(mint: &str, tokens: &str, cost: &str, hours: i64) -> Position {
        Position {
            mint: mint.to_owned(),
            tokens: d(tokens),
            cost_usd: d(cost),
            opened_at: t0() + Duration::hours(hours),
            entry_liquidity_usd: d("100000"),
        }
    }

    fn mark(mint: &str, price: &str, liquidity: &str) -> (String, Mark) {
        (
            mint.to_owned(),
            Mark {
                price_usd: d(price),
                liquidity_usd: d(liquidity),
            },
        )
    }

    #[test]
    fn sell_value_includes_pool_fee_and_impact() {
        let gross = d("500") * d("0.02");
        let value = sell_value(d("500"), d("0.02"), d("1000000000"), &costs()).unwrap();
        // 0.25% pool fee plus a hair of impact.
        assert!(
            value < gross * d("0.9975") && value > gross * d("0.9974"),
            "{value}"
        );
    }

    #[test]
    fn ledger_sells_fifo_and_nets_every_cost() {
        let mut ledger = Ledger::default();
        ledger.open(lot("A", "1000", "10.10", 0)).unwrap();
        ledger.open(lot("A", "1000", "20.20", 1)).unwrap();
        ledger.open(lot("B", "50", "5", 2)).unwrap();

        // Sell 1500 A: all of lot 1 (10.10) + half of lot 2 (10.10).
        let at = t0() + Duration::hours(3);
        let pnl = ledger
            .close("A", d("1500"), d("30"), d("0.02"), at)
            .unwrap();
        assert_eq!(pnl, d("9.78"));
        assert!(ledger.closed_trades().is_empty(), "A is still held");
        assert_eq!(ledger.lots().len(), 2);
        assert_eq!(ledger.lots()[0].tokens, d("500"));
        assert_eq!(ledger.lots()[0].cost_usd, d("10.10"));

        ledger.record_failed_tx(d("0.03")).unwrap();
        ledger.record_ai_cost(d("1.25")).unwrap();

        let marks = BTreeMap::from([mark("A", "0.02", "1000000000")]);
        let pnl = ledger.pnl(&marks, &costs()).unwrap();
        assert_eq!(pnl.realised_usd, d("9.78"));
        // A: 500 tokens ~ 10 USD after fee - 10.10; B has no mark: -5.
        let a_value = sell_value(d("500"), d("0.02"), d("1000000000"), &costs()).unwrap();
        assert_eq!(pnl.unrealised_usd, a_value - d("10.10") - d("5"));
        assert_eq!(
            pnl.net_usd,
            d("9.78") + pnl.unrealised_usd - d("0.03") - d("1.25")
        );

        // Selling the rest completes one round trip carrying both sells.
        ledger.close("A", d("500"), d("11"), d("0.01"), at).unwrap();
        let trips = ledger.closed_trades();
        assert_eq!(trips.len(), 1);
        assert_eq!(trips[0].opened_at, t0());
        assert_eq!(trips[0].net_pnl_usd, d("9.78") + d("0.89"));
    }

    #[test]
    fn split_exits_count_as_one_trade() {
        let mut ledger = Ledger::default();
        ledger.open(lot("A", "300", "30", 0)).unwrap();
        for _ in 0..3 {
            ledger.close("A", d("100"), d("11"), d("0"), t0()).unwrap();
        }
        assert_eq!(ledger.closed_trades().len(), 1);
        assert_eq!(ledger.closed_trades()[0].net_pnl_usd, d("3"));

        // A new buy after that starts a second round trip.
        ledger.open(lot("A", "10", "1", 1)).unwrap();
        ledger
            .close("A", d("10"), d("2"), d("0"), t0() + Duration::hours(2))
            .unwrap();
        assert_eq!(ledger.closed_trades().len(), 2);
    }

    #[test]
    fn open_losses_do_not_net_against_open_winners() {
        let mut ledger = Ledger::default();
        ledger.open(lot("WIN", "100", "10", 0)).unwrap();
        ledger.open(lot("LOSE", "100", "10", 0)).unwrap();
        let marks = BTreeMap::from([
            mark("WIN", "1", "1000000000"),
            mark("LOSE", "0.01", "1000000000"),
        ]);
        let lose_value = sell_value(d("100"), d("0.01"), d("1000000000"), &costs()).unwrap();
        assert_eq!(
            ledger.open_losses(&marks, &costs()),
            Ok(lose_value - d("10"))
        );
        // WIN's partial sell lost 4, but the rest is up by more: the trip is
        // a net winner overall, so it adds nothing and LOSE stays the total.
        ledger.close("WIN", d("50"), d("1"), d("0"), t0()).unwrap();
        let win_rest = sell_value(d("50"), d("1"), d("1000000000"), &costs()).unwrap();
        assert!(
            d("-4") + win_rest - d("5") > Decimal::ZERO,
            "WIN is still up overall"
        );
        assert_eq!(
            ledger.open_losses(&marks, &costs()),
            Ok(lose_value - d("10"))
        );
    }

    #[test]
    fn ledger_rejects_bad_amounts() {
        let mut ledger = Ledger::default();
        assert_eq!(
            ledger.open(lot("A", "0", "1", 0)),
            Err(LedgerError::InvalidAmount)
        );
        ledger.open(lot("A", "10", "1", 0)).unwrap();
        assert_eq!(
            ledger.close("A", d("11"), d("1"), d("0"), t0()),
            Err(LedgerError::InsufficientTokens)
        );
        assert_eq!(
            ledger.close("A", d("1"), d("1"), d("-1"), t0()),
            Err(LedgerError::InvalidAmount)
        );
        assert_eq!(
            ledger.record_ai_cost(d("-1")),
            Err(LedgerError::InvalidAmount)
        );
        assert_eq!(
            ledger.record_failed_tx(d("-1")),
            Err(LedgerError::InvalidAmount)
        );
        assert_eq!(
            ledger.close("A", d("1"), d("1"), d("0"), t0() - Duration::seconds(1)),
            Err(LedgerError::OutOfOrder)
        );
        assert_eq!(
            ledger.close("A", d("1"), Decimal::MAX, Decimal::ZERO, t0()),
            Ok(Decimal::MAX - d("0.1"))
        );
        ledger.record_ai_cost(Decimal::MAX).unwrap();
        assert_eq!(
            ledger.record_ai_cost(Decimal::MAX),
            Err(LedgerError::Overflow)
        );
        assert_eq!(
            ledger.lots()[0].tokens,
            d("9"),
            "failed calls change nothing"
        );
    }

    #[test]
    fn hostile_sizes_do_not_panic() {
        // A near-zero price yields an enormous token count; partial sells and
        // P&L must stay exact or report overflow, never panic.
        let mut ledger = Ledger::default();
        let huge = Decimal::MAX / d("2");
        ledger.open(lot("A", "1", "40", 0)).unwrap();
        ledger.lots[0].tokens = huge;
        ledger
            .close("A", huge / d("3"), d("1"), d("0"), t0())
            .unwrap();
        ledger.open(lot("A", "1", "1", 0)).unwrap();
        ledger.lots[1].tokens = Decimal::MAX;
        assert_eq!(
            ledger.pnl(&BTreeMap::new(), &costs()).map(|_| ()),
            Err(LedgerError::Overflow)
        );
    }

    #[test]
    fn partial_sells_preserve_total_cost() {
        let mut rng = Rng::new(3);
        for _ in 0..200 {
            let mut ledger = Ledger::default();
            ledger.open(lot("A", "3", "10", 0)).unwrap();
            let mut paid_back = Decimal::ZERO;
            while let Some(held) = ledger.lots().first().map(|l| l.tokens) {
                let part = (rng.decimal(3, 4) + d("0.0001")).min(held);
                // Sell at zero proceeds: the loss is exactly the cost consumed.
                paid_back -= ledger.close("A", part, d("0"), d("0"), t0()).unwrap();
            }
            assert_eq!(paid_back, d("10"));
            assert_eq!(ledger.closed_trades().len(), 1);
        }
    }

    #[test]
    fn curve_output_rounds_down() {
        // 2 * 1 / (2 + 1) = 0.6666...; rounding to nearest would end in 7.
        assert_eq!(curve_out(d("1"), d("2"), d("2")), Some(d("0.666666666666")));
        // Below the 12th decimal the output is zero: valued, but not filled.
        assert_eq!(
            curve_out(d("1"), d("1000000000000000"), d("1")),
            Some(Decimal::ZERO)
        );
        assert_eq!(
            sell(
                d("1"),
                d("0.000000000000000000001"),
                d("100000"),
                &costs(),
                d("0.5")
            ),
            None
        );
    }

    #[test]
    fn collapsed_price_values_at_zero_and_bad_costs_value_nothing() {
        let tiny = d("0.00000000000000000001");
        assert_eq!(
            sell_value(d("40000"), tiny, d("200000"), &costs()),
            Some(Decimal::ZERO)
        );
        for bad in [
            CostModel {
                dex_fee_bps: 10_000,
                ..costs()
            },
            CostModel {
                network_fee_usd: d("-1"),
                ..costs()
            },
            CostModel {
                failure_rate: d("2"),
                ..costs()
            },
        ] {
            assert!(!bad.is_valid());
            assert_eq!(sell_value(d("1"), d("1"), d("100000"), &bad), None);
        }
    }

    #[test]
    fn back_dated_events_are_refused() {
        let mut ledger = Ledger::default();
        ledger.open(lot("A", "10", "1", 5)).unwrap();
        assert_eq!(
            ledger.open(lot("B", "10", "1", 4)),
            Err(LedgerError::OutOfOrder)
        );
        ledger.open(lot("B", "10", "1", 5)).unwrap();
        ledger
            .close("A", d("10"), d("1"), d("0"), t0() + Duration::hours(6))
            .unwrap();
        assert_eq!(
            ledger.open(lot("C", "10", "1", 5)),
            Err(LedgerError::OutOfOrder)
        );
    }
}
