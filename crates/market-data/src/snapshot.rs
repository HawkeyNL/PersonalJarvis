//! Map raw reads into [`TokenSnapshot`] for the deterministic core.
//!
//! Every source is optional. When a value is unknown it is mapped to the
//! value the filter or strategy rejects, never to a permissive default:
//!
//! | field | unknown becomes | effect |
//! |---|---|---|
//! | price, liquidity | `0` | `InvalidSnapshot` |
//! | pool creation time (or before Solana genesis) | `observed_at` | `PoolTooYoung` |
//! | mint / freeze authority revoked | `false` | `MintAuthorityActive` |
//! | top-holder share | `1` (100%) | `HolderConcentrationTooHigh` |
//! | sell price impact | `None` | `NoSellRoute` |
//! | LP status | always `Unknown` (see below) | `LpNotSecured` |
//! | 5 min buys / sells | `0` / `0` | no entry (no trades) |
//! | 5 min price change | `i32::MIN` | outside any momentum band |
//!
//! LP status: DexScreener has none, and RugCheck's `lpLockedPct` says neither
//! whether the LP is burned or time-locked nor until when, so this crate never
//! claims `Burned` or `Locked`. Reading the LP mint on chain is a follow-up.
//!
//! Boosts are copied as reported (they are audited, never a positive signal);
//! trending rank is not fetched and stays `None`.

use jarvis_onchain::types::{LpStatus, TokenSnapshot};
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use time::OffsetDateTime;

use crate::dexscreener::Pair;
use crate::jupiter::Quote;
use crate::rpc::{CrossChecked, TokenAccountBalance};
use crate::token2022::MintAccount;
use crate::MarketDataError;

/// How many of the largest token accounts count as "top holders".
pub const TOP_HOLDERS: usize = 10;

/// Solana mainnet-beta genesis (2020-03-16) in Unix ms. An earlier pool
/// creation time is junk and treated as unknown.
const SOLANA_GENESIS_MS: i64 = 1_584_368_940_000;

/// Everything read about one token at `observed_at`.
#[derive(Debug, Clone, Copy)]
pub struct Observation<'a> {
    pub mint: &'a str,
    pub observed_at: OffsetDateTime,
    /// The main pool, from DexScreener.
    pub pair: Option<&'a Pair>,
    pub mint_account: Option<&'a CrossChecked<MintAccount>>,
    pub largest_accounts: Option<&'a CrossChecked<Vec<TokenAccountBalance>>>,
    /// Token accounts left out of the holder share. Only the pool's vault and
    /// burn token accounts, verified on chain; never addresses taken from
    /// DexScreener or another API. Every entry lowers the share.
    pub excluded_accounts: &'a [&'a str],
    /// Raw units of `mint` the position would sell; `sell_quote` must be for
    /// exactly this amount.
    pub sell_amount: u64,
    /// A quote selling `sell_amount` of `mint`.
    pub sell_quote: Option<&'a Quote>,
}

/// Build the snapshot. Fails only when an input is about another token or
/// another amount.
pub fn to_snapshot(o: &Observation<'_>) -> Result<TokenSnapshot, MarketDataError> {
    if let Some(pair) = o.pair {
        if pair.chain_id != "solana" || pair.base_token.address != o.mint {
            return Err(MarketDataError::InvalidInput("pair is for another token"));
        }
    }
    if let Some(quote) = o.sell_quote {
        if quote.input_mint != o.mint || quote.in_amount != o.sell_amount {
            return Err(MarketDataError::InvalidInput(
                "quote does not sell the intended position",
            ));
        }
    }
    let pair = o.pair;
    let mint = o.mint_account.map(CrossChecked::get);
    let m5 = pair.and_then(|p| p.txns.as_ref()).and_then(|t| t.m5);
    Ok(TokenSnapshot {
        mint: o.mint.to_owned(),
        observed_at: o.observed_at,
        pool_created_at: pair
            .and_then(|p| p.pair_created_at)
            .filter(|ms| *ms >= SOLANA_GENESIS_MS)
            .and_then(|ms| {
                OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000).ok()
            })
            .unwrap_or(o.observed_at),
        price_usd: pair.and_then(|p| p.price_usd).unwrap_or(Decimal::ZERO),
        liquidity_usd: pair
            .and_then(|p| p.liquidity.as_ref())
            .and_then(|l| l.usd)
            .unwrap_or(Decimal::ZERO),
        mint_authority_revoked: mint.is_some_and(|m| m.mint_authority.is_none()),
        freeze_authority_revoked: mint.is_some_and(|m| m.freeze_authority.is_none()),
        // Without the mint account both authorities above are `false`, so the
        // filter rejects before it looks at extensions.
        extensions: mint.map(|m| m.extensions.clone()).unwrap_or_default(),
        lp: LpStatus::Unknown,
        top_holders_share: holder_share(o).unwrap_or(Decimal::ONE),
        sell_impact_bps: o.sell_quote.and_then(sell_impact_bps),
        buys_5m: m5.map_or(0, |t| t.buys),
        sells_5m: m5.map_or(0, |t| t.sells),
        price_change_5m_bps: pair
            .and_then(|p| p.price_change.as_ref())
            .and_then(|c| c.m5)
            .and_then(|pct| pct.checked_mul(Decimal::ONE_HUNDRED))
            .and_then(|bps| bps.round().to_i32())
            .unwrap_or(i32::MIN),
        boosts: pair.and_then(|p| p.boosts).map_or(0, |b| b.active),
        trending_rank: None,
    })
}

/// The larger of Jupiter's reported impact and the loss implied by its own
/// USD values (which also counts fees). The unit of `priceImpact` is not
/// verified live, so the USD cross-check keeps a misread unit from making the
/// impact look smaller. Overflow saturates to `u32::MAX`.
fn sell_impact_bps(quote: &Quote) -> Option<u32> {
    let reported = quote.price_impact_pct.map(|pct| {
        pct.abs()
            .checked_mul(Decimal::ONE_HUNDRED)
            .and_then(|bps| bps.ceil().to_u32())
            .unwrap_or(u32::MAX)
    });
    let from_usd = match (quote.in_usd_value, quote.out_usd_value) {
        (Some(input), Some(output)) if input > Decimal::ZERO => Some(
            input
                .checked_sub(output)
                .map(|loss| loss.max(Decimal::ZERO))
                .and_then(|loss| loss.checked_div(input))
                .and_then(|loss| loss.checked_mul(Decimal::from(10_000)))
                .and_then(|bps| bps.ceil().to_u32())
                .unwrap_or(u32::MAX),
        ),
        _ => None,
    };
    reported.max(from_usd)
}

/// Share of supply in the [`TOP_HOLDERS`] largest non-excluded accounts.
fn holder_share(o: &Observation<'_>) -> Option<Decimal> {
    let supply = o.mint_account?.get().supply;
    if supply == 0 {
        return None;
    }
    let mut amounts: Vec<u64> = o
        .largest_accounts?
        .get()
        .iter()
        .filter(|a| !o.excluded_accounts.contains(&a.address.as_str()))
        .map(|a| a.amount)
        .collect();
    amounts.sort_unstable_by(|a, b| b.cmp(a));
    let held: u128 = amounts
        .iter()
        .take(TOP_HOLDERS)
        .map(|a| u128::from(*a))
        .sum();
    Decimal::from_u128(held)?.checked_div(Decimal::from(supply))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dexscreener::{BuysSells, Liquidity, PairToken, PriceChange, Txns};
    use crate::token2022::TokenProgram;
    use jarvis_onchain::filter::{check, FilterRules, Verdict};
    use std::collections::BTreeSet;
    use time::Duration;

    const MINT: &str = "7GCihgDB8fe6KNjn2MYtkzZcRjQy3t9GHdC8uHYmW2hr";
    const VAULT: &str = "8xKqnB5Y5WkYpM3cW2qN6h9a4zH7tVjRrL2sE1dFgPbQ";
    const SELL: u64 = 2_500_000_000;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_761_930_000).unwrap()
    }

    fn pair() -> Pair {
        let tokens: Vec<Pair> =
            serde_json::from_str(include_str!("../fixtures/dexscreener_tokens.json")).unwrap();
        tokens.into_iter().next().unwrap()
    }

    fn mint_account() -> MintAccount {
        MintAccount {
            program: TokenProgram::Spl,
            supply: 1_000_000_000_000_000,
            decimals: 6,
            mint_authority: None,
            freeze_authority: None,
            extensions: Vec::new(),
        }
    }

    fn mint() -> CrossChecked<MintAccount> {
        CrossChecked(mint_account())
    }

    fn holders() -> CrossChecked<Vec<TokenAccountBalance>> {
        let reply: serde_json::Value = serde_json::from_str(include_str!(
            "../fixtures/rpc_get_token_largest_accounts.json"
        ))
        .unwrap();
        CrossChecked(serde_json::from_value(reply["result"]["value"].clone()).unwrap())
    }

    fn quote() -> Quote {
        Quote {
            input_mint: MINT.to_owned(),
            output_mint: "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v".to_owned(),
            in_amount: SELL,
            out_amount: 1_043_211,
            price_impact_pct: Some(d("1.371")),
            in_usd_value: None,
            out_usd_value: None,
        }
    }

    fn empty() -> Observation<'static> {
        Observation {
            mint: MINT,
            observed_at: now(),
            pair: None,
            mint_account: None,
            largest_accounts: None,
            excluded_accounts: &[],
            sell_amount: SELL,
            sell_quote: None,
        }
    }

    /// Loose rules: anything a strict mapping lets through would pass them.
    fn loose_rules() -> FilterRules {
        FilterRules {
            min_liquidity_usd: Decimal::ZERO,
            min_pool_age: Duration::ZERO,
            max_top_holders_share: Decimal::ONE,
            max_sell_impact_bps: u32::MAX,
            allowed_extensions: Vec::new(),
            max_snapshot_age: Duration::hours(1),
            min_lp_lock_remaining: Duration::ZERO,
        }
    }

    #[test]
    fn maps_full_observation() {
        let (pair, mint, holders, quote) = (pair(), mint(), holders(), quote());
        let snapshot = to_snapshot(&Observation {
            pair: Some(&pair),
            mint_account: Some(&mint),
            largest_accounts: Some(&holders),
            excluded_accounts: &[VAULT],
            sell_quote: Some(&quote),
            ..empty()
        })
        .unwrap();
        assert_eq!(snapshot.price_usd, d("0.0004213"));
        assert_eq!(snapshot.liquidity_usd, d("184532.17"));
        assert_eq!(
            snapshot.pool_created_at,
            OffsetDateTime::from_unix_timestamp(1_761_900_000).unwrap()
        );
        assert!(snapshot.mint_authority_revoked && snapshot.freeze_authority_revoked);
        // 50e12 + 30e12 of 1e18, the pool vault excluded.
        assert_eq!(snapshot.top_holders_share, d("0.08"));
        assert_eq!(snapshot.sell_impact_bps, Some(138));
        assert_eq!((snapshot.buys_5m, snapshot.sells_5m), (41, 17));
        assert_eq!(snapshot.price_change_5m_bps, 325);
        assert_eq!(snapshot.boosts, 500);
        assert_eq!(snapshot.trending_rank, None);
        assert_eq!(snapshot.lp, LpStatus::Unknown);
    }

    #[test]
    fn unknown_everything_maps_strict_and_is_rejected() {
        let snapshot = to_snapshot(&empty()).unwrap();
        assert_eq!(snapshot.price_usd, Decimal::ZERO);
        assert_eq!(snapshot.liquidity_usd, Decimal::ZERO);
        assert_eq!(snapshot.pool_created_at, now());
        assert!(!snapshot.mint_authority_revoked && !snapshot.freeze_authority_revoked);
        assert_eq!(snapshot.top_holders_share, Decimal::ONE);
        assert_eq!(snapshot.sell_impact_bps, None);
        assert_eq!((snapshot.buys_5m, snapshot.sells_5m), (0, 0));
        assert_eq!(snapshot.price_change_5m_bps, i32::MIN);
        assert_eq!(snapshot.lp, LpStatus::Unknown);
        assert!(matches!(
            check(&snapshot, &loose_rules(), &BTreeSet::new(), now()),
            Verdict::Reject(_)
        ));
    }

    /// Each unknown field on its own, with everything else known and good,
    /// still maps to the strict side.
    #[test]
    fn each_unknown_field_is_strict_on_its_own() {
        let (mint, holders, quote) = (mint(), holders(), quote());
        let mut bare = pair();
        bare.price_usd = None;
        bare.liquidity = Some(Liquidity { usd: None });
        bare.pair_created_at = None;
        bare.txns = Some(Txns { m5: None });
        bare.price_change = Some(PriceChange { m5: None });
        bare.boosts = None;
        let base = Observation {
            pair: Some(&bare),
            mint_account: Some(&mint),
            largest_accounts: Some(&holders),
            excluded_accounts: &[VAULT],
            sell_quote: Some(&quote),
            ..empty()
        };
        let s = to_snapshot(&base).unwrap();
        assert_eq!(
            (s.price_usd, s.liquidity_usd),
            (Decimal::ZERO, Decimal::ZERO)
        );
        assert_eq!(s.pool_created_at, now());
        assert_eq!(
            (s.buys_5m, s.sells_5m, s.price_change_5m_bps),
            (0, 0, i32::MIN)
        );
        assert_eq!(s.boosts, 0);

        let s = to_snapshot(&Observation {
            largest_accounts: None,
            ..base
        })
        .unwrap();
        assert_eq!(s.top_holders_share, Decimal::ONE);

        let empty_supply = CrossChecked(MintAccount {
            supply: 0,
            ..mint_account()
        });
        let s = to_snapshot(&Observation {
            mint_account: Some(&empty_supply),
            ..base
        })
        .unwrap();
        assert_eq!(s.top_holders_share, Decimal::ONE);

        let no_impact = Quote {
            price_impact_pct: None,
            ..quote.clone()
        };
        let s = to_snapshot(&Observation {
            sell_quote: Some(&no_impact),
            ..base
        })
        .unwrap();
        assert_eq!(s.sell_impact_bps, None);

        let live = CrossChecked(MintAccount {
            mint_authority: Some([1; 32]),
            ..mint_account()
        });
        let s = to_snapshot(&Observation {
            mint_account: Some(&live),
            ..base
        })
        .unwrap();
        assert!(!s.mint_authority_revoked);

        for junk in [0, -1, SOLANA_GENESIS_MS - 1, i64::MAX] {
            let mut odd = pair();
            odd.pair_created_at = Some(junk);
            let s = to_snapshot(&Observation {
                pair: Some(&odd),
                ..base
            })
            .unwrap();
            assert!(s.pool_created_at >= now(), "{junk}");
        }
    }

    #[test]
    fn sell_impact_takes_the_worse_of_reported_and_usd_loss() {
        let impact = |reported: Option<&str>, usd: Option<(&str, &str)>| {
            sell_impact_bps(&Quote {
                price_impact_pct: reported.map(d),
                in_usd_value: usd.map(|(i, _)| d(i)),
                out_usd_value: usd.map(|(_, o)| d(o)),
                ..quote()
            })
        };
        // A fraction misread as percent (0.0137) is caught by the USD loss.
        assert_eq!(impact(Some("0.0137"), Some(("100", "98.63"))), Some(137));
        assert_eq!(impact(Some("2.5"), Some(("100", "99"))), Some(250));
        assert_eq!(impact(None, Some(("100", "101"))), Some(0));
        assert_eq!(impact(None, Some(("0", "1"))), None);
        assert_eq!(impact(Some("-0.5"), None), Some(50));
        assert_eq!(impact(None, None), None);
    }

    #[test]
    fn excluded_accounts_only_drop_listed_accounts() {
        let (mint, holders) = (mint(), holders());
        let base = Observation {
            mint_account: Some(&mint),
            largest_accounts: Some(&holders),
            ..empty()
        };
        assert_eq!(to_snapshot(&base).unwrap().top_holders_share, d("0.48"));
    }

    #[test]
    fn boosts_change_nothing_but_the_boost_count() {
        let (mint, holders, quote) = (mint(), holders(), quote());
        let mut quiet = pair();
        quiet.boosts = None;
        let loud = pair();
        let observe = |p: &Pair| {
            to_snapshot(&Observation {
                pair: Some(p),
                mint_account: Some(&mint),
                largest_accounts: Some(&holders),
                excluded_accounts: &[VAULT],
                sell_quote: Some(&quote),
                ..empty()
            })
            .unwrap()
        };
        let mut boosted = observe(&loud);
        assert_eq!(boosted.boosts, 500);
        boosted.boosts = 0;
        assert_eq!(boosted, observe(&quiet));
    }

    #[test]
    fn inputs_about_another_token_or_amount_are_refused() {
        let mut other = pair();
        other.base_token = PairToken {
            address: "So11111111111111111111111111111111111111112".to_owned(),
        };
        assert!(to_snapshot(&Observation {
            pair: Some(&other),
            ..empty()
        })
        .is_err());

        let buy = Quote {
            input_mint: "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v".to_owned(),
            ..quote()
        };
        assert!(to_snapshot(&Observation {
            sell_quote: Some(&buy),
            ..empty()
        })
        .is_err());

        let small = Quote {
            in_amount: 1,
            ..quote()
        };
        assert!(to_snapshot(&Observation {
            sell_quote: Some(&small),
            ..empty()
        })
        .is_err());
    }

    #[test]
    fn extreme_values_saturate_to_the_strict_side() {
        for huge in ["1e20", "1e27", "79228162514264337593543950335"] {
            let value = Decimal::from_scientific(huge)
                .or_else(|_| huge.parse())
                .unwrap();
            let quote = Quote {
                price_impact_pct: Some(value),
                in_usd_value: Some(Decimal::ONE),
                out_usd_value: Some(-value),
                ..quote()
            };
            let mut wild = pair();
            wild.txns = Some(Txns {
                m5: Some(BuysSells { buys: 1, sells: 1 }),
            });
            wild.price_change = Some(PriceChange { m5: Some(value) });
            let s = to_snapshot(&Observation {
                pair: Some(&wild),
                sell_quote: Some(&quote),
                ..empty()
            })
            .unwrap();
            assert_eq!(s.sell_impact_bps, Some(u32::MAX), "{huge}");
            assert_eq!(s.price_change_5m_bps, i32::MIN, "{huge}");
        }
    }
}
