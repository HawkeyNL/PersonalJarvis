//! Candidate mints from pump.fun migrations to PumpSwap.
//!
//! When a pump.fun bonding curve completes, pump.fun calls `migrate` or
//! `migrate_v2` on its program, which creates the PumpSwap pool through a CPI
//! into the PumpSwap program. Both instructions take the Global account's
//! `withdraw_authority` as a writable account, so polling
//! `getSignaturesForAddress` on that account lists the migrations (plus a few
//! failed or repeated calls) without paging through every pump.fun trade.
//!
//! Each new signature is read with `getTransaction` and only counted when a
//! top-level pump.fun migrate instruction succeeded and invoked PumpSwap; a
//! migrate on a curve that is already migrated succeeds as a no-op without
//! that call. Its mint and pool are then kept for [`MAX_AGE`] and offered as
//! candidates once they are [`MIN_AGE`] old.
//!
//! Checked 2026-11-08 against the pump IDL
//! (<https://github.com/pump-fun/pump-public-docs/blob/main/idl/pump.json>:
//! `migrate` accounts `mint` #2, `pump_amm` #8, `pool` #9; `migrate_v2`
//! accounts `base_mint` #2, `pump_amm` #9, `pool` #10; both without
//! arguments), the mainnet Global account (`withdraw_authority`), and
//! recorded mainnet migrations whose pools are owned by PumpSwap and hold the
//! parsed mint.
//!
//! A discovered mint is only a name to look at. It goes through the same
//! cross-checked reads and hard filters as a watchlist mint before anything
//! is simulated, so discovery reads the primary RPC provider alone: a wrong
//! or missing answer can only cost a candidate, never let one through.

use std::collections::HashMap;

use time::{Duration, OffsetDateTime};

use crate::pools::PUMPSWAP_PROGRAM;
use crate::rpc::{CrossCheckedRpc, RpcClient, Transaction};
use crate::{validate_address, validate_signature, MarketDataError, Provider};

pub const PUMP_PROGRAM: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
/// `withdraw_authority` of the pump.fun Global account
/// (`4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf`), writable in every
/// migration.
pub const MIGRATION_AUTHORITY: &str = "39azUYFWPz3VHgKCf3VChUwbpURdCHRxjWVowf5jUJjg";

/// Base58 instruction data of `migrate` and `migrate_v2`: their 8-byte
/// Anchor discriminators, as neither takes arguments.
const MIGRATE: &str = "T5bZvAk4s5f";
const MIGRATE_V2: &str = "YQq8B6nbicx";

/// Signatures asked for per poll.
pub const MAX_SIGNATURES_PER_POLL: usize = 200;
/// `getTransaction` calls per poll; the rest waits for the next poll.
pub const MAX_TRANSACTIONS_PER_POLL: usize = 25;
/// Mints kept; the oldest migration is dropped first.
pub const MAX_MINTS: usize = 2000;
/// A migration is offered as a candidate from this age on ...
pub const MIN_AGE: Duration = Duration::minutes(30);
/// ... and forgotten after this one.
pub const MAX_AGE: Duration = Duration::hours(72);
/// A block time further ahead of our clock than this is refused.
const MAX_CLOCK_SKEW: Duration = Duration::minutes(5);

/// A completed pump.fun migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migration {
    pub mint: String,
    /// The PumpSwap pool the migration created. Untrusted like every pool
    /// address: [`crate::pools::read`] checks it again.
    pub pool: String,
    pub migrated_at: OffsetDateTime,
}

/// What one poll did, for the log.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PollStats {
    pub signatures: usize,
    pub fetched: usize,
    pub found: usize,
    pub skipped: usize,
    /// More new signatures than one page holds: the older ones were not read.
    pub gap: bool,
}

/// Discovery state, kept in memory only: a restart starts again from the
/// newest [`MAX_SIGNATURES_PER_POLL`] signatures.
#[derive(Debug, Default)]
pub struct Discovery {
    /// Newest signature handled; the next poll stops before it.
    until: Option<String>,
    mints: HashMap<String, Migration>,
}

impl Discovery {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read new migrations from the primary provider (see the module docs
    /// for why one provider is enough).
    ///
    /// Signatures are handled oldest first and the cursor moves past each one
    /// as soon as it is handled, so `until` alone resumes exactly where a
    /// capped, failed or cancelled poll stopped; `before` is never needed. A
    /// read that may succeed later (rate limit, busy, transport, HTTP status)
    /// ends the poll with that error and is retried; anything else about a
    /// signature (failed transaction, not a migration, malformed reply) skips
    /// it.
    pub async fn poll(
        &mut self,
        rpc: &CrossCheckedRpc,
        now: OffsetDateTime,
    ) -> Result<PollStats, MarketDataError> {
        self.poll_with(rpc.primary(), now).await
    }

    async fn poll_with(
        &mut self,
        rpc: &RpcClient,
        now: OffsetDateTime,
    ) -> Result<PollStats, MarketDataError> {
        self.prune(now);
        let page = rpc
            .signatures_for_address(
                MIGRATION_AUTHORITY,
                self.until.as_deref(),
                MAX_SIGNATURES_PER_POLL,
            )
            .await?;
        // A bad entry would become the cursor and break every later poll.
        if page
            .iter()
            .any(|s| validate_signature(&s.signature).is_err())
        {
            return Err(MarketDataError::InvalidData {
                provider: Provider::SolanaRpc(0),
                reason: "signature list holds an invalid signature",
            });
        }
        // ponytail: a full page after the cursor skips what lies between; at
        // a two-minute poll that needs 200 migration-account transactions in
        // two minutes. Page back with `before` if `gap` ever shows up.
        let mut stats = PollStats {
            signatures: page.len(),
            gap: self.until.is_some() && page.len() == MAX_SIGNATURES_PER_POLL,
            ..PollStats::default()
        };
        for info in page.iter().rev() {
            let too_old = info
                .block_time
                .and_then(|t| OffsetDateTime::from_unix_timestamp(t).ok())
                .is_some_and(|at| now - at > MAX_AGE);
            if info.err.is_some() || too_old {
                stats.skipped += 1;
            } else {
                if stats.fetched == MAX_TRANSACTIONS_PER_POLL {
                    break;
                }
                stats.fetched += 1;
                let found = match rpc.transaction(&info.signature).await {
                    Ok(Some(tx)) => {
                        let at = tx.block_time.or(info.block_time);
                        self.record(&tx, at, now)
                    }
                    Ok(None) => 0,
                    Err(error) if retryable(&error) => return Err(error),
                    Err(_) => 0,
                };
                if found == 0 {
                    stats.skipped += 1;
                }
                stats.found += found;
            }
            self.until = Some(info.signature.clone());
        }
        Ok(stats)
    }

    /// Keep the migrations in `tx`; returns how many were new.
    fn record(&mut self, tx: &Transaction, block_time: Option<i64>, now: OffsetDateTime) -> usize {
        let Some(at) = block_time.and_then(|t| OffsetDateTime::from_unix_timestamp(t).ok()) else {
            return 0;
        };
        if at > now + MAX_CLOCK_SKEW || now - at > MAX_AGE {
            return 0;
        }
        parse(tx)
            .into_iter()
            .filter(|(mint, pool)| {
                self.insert(Migration {
                    mint: mint.clone(),
                    pool: pool.clone(),
                    migrated_at: at,
                })
            })
            .count()
    }

    /// `false` for a mint already known (the first migration seen is kept)
    /// or one older than every kept mint while the store is full.
    fn insert(&mut self, migration: Migration) -> bool {
        if self.mints.contains_key(&migration.mint) {
            return false;
        }
        if self.mints.len() >= MAX_MINTS {
            let Some(oldest) = self
                .mints
                .values()
                .min_by_key(|m| m.migrated_at)
                .filter(|m| m.migrated_at < migration.migrated_at)
                .map(|m| m.mint.clone())
            else {
                return false;
            };
            self.mints.remove(&oldest);
        }
        self.mints.insert(migration.mint.clone(), migration);
        true
    }

    fn prune(&mut self, now: OffsetDateTime) {
        self.mints.retain(|_, m| now - m.migrated_at <= MAX_AGE);
    }

    /// Migrations between [`MIN_AGE`] and [`MAX_AGE`] old, newest first.
    pub fn due(&self, now: OffsetDateTime) -> Vec<&Migration> {
        let mut due: Vec<&Migration> = self
            .mints
            .values()
            .filter(|m| (MIN_AGE..=MAX_AGE).contains(&(now - m.migrated_at)))
            .collect();
        due.sort_by(|a, b| {
            b.migrated_at
                .cmp(&a.migrated_at)
                .then_with(|| a.mint.cmp(&b.mint))
        });
        due
    }

    /// Whether `mint` is kept (due or not yet).
    pub fn knows(&self, mint: &str) -> bool {
        self.mints.contains_key(mint)
    }

    /// Every kept migration, in no particular order.
    pub fn migrations(&self) -> impl Iterator<Item = &Migration> {
        self.mints.values()
    }

    /// Keep a migration seen before, e.g. one stored before a restart, with
    /// the same checks as a polled one. `false` when it is refused or
    /// already known.
    pub fn remember(&mut self, migration: Migration, now: OffsetDateTime) -> bool {
        let valid = validate_address(&migration.mint).is_ok()
            && validate_address(&migration.pool).is_ok()
            && migration.migrated_at <= now + MAX_CLOCK_SKEW
            && now - migration.migrated_at <= MAX_AGE;
        valid && self.insert(migration)
    }
}

/// Worth retrying on a later poll rather than skipping the signature.
fn retryable(error: &MarketDataError) -> bool {
    matches!(
        error,
        MarketDataError::RateLimited { .. }
            | MarketDataError::Busy { .. }
            | MarketDataError::Http { .. }
            | MarketDataError::Status { .. }
    )
}

/// `(mint, pool)` of every successful top-level migration in `tx` that
/// created its PumpSwap pool. Anything unexpected yields nothing.
fn parse(tx: &Transaction) -> Vec<(String, String)> {
    let Some(meta) = tx.meta.as_ref().filter(|meta| meta.err.is_none()) else {
        return Vec::new();
    };
    let message = &tx.transaction.message;
    let keys: Vec<&str> = message
        .account_keys
        .iter()
        .chain(
            meta.loaded_addresses
                .iter()
                .flat_map(|l| l.writable.iter().chain(&l.readonly)),
        )
        .map(String::as_str)
        .collect();
    let key = |index: usize| keys.get(index).copied();
    let created_pool = |at: usize| {
        meta.inner_instructions
            .iter()
            .flatten()
            .filter(|group| group.index == at)
            .flat_map(|group| &group.instructions)
            .any(|inner| key(inner.program_id_index) == Some(PUMPSWAP_PROGRAM))
    };
    message
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(at, ix)| {
            if key(ix.program_id_index) != Some(PUMP_PROGRAM) {
                return None;
            }
            let (mint, amm, pool) = match ix.data.as_str() {
                MIGRATE => (2, 8, 9),
                MIGRATE_V2 => (2, 9, 10),
                _ => return None,
            };
            let account = |i: usize| ix.accounts.get(i).and_then(|&k| key(k));
            let (mint, pool) = (account(mint)?, account(pool)?);
            (account(amm)? == PUMPSWAP_PROGRAM
                && created_pool(at)
                && validate_address(mint).is_ok()
                && validate_address(pool).is_ok())
            .then(|| (mint.to_owned(), pool.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{ok_json, response, serve_with};
    use serde_json::Value;

    // Verbatim mainnet replies recorded 2026-11-08 from the public RPC:
    // the newest 20 signatures of MIGRATION_AUTHORITY, and three of their
    // transactions (`migrate_v2` in a v0 transaction with a lookup table,
    // `migrate` in a v0 transaction without, and a no-op `migrate_v2` on an
    // already migrated curve in a v1 transaction), plus a `migrate_v2` in a
    // v1 transaction from shortly before (re-serialized, same content).
    const SIGNATURES: &str = include_str!("../fixtures/discovery/signatures.json");
    const MIGRATE_V2_TX: &str = include_str!("../fixtures/discovery/migrate_v2.json");
    const MIGRATE_TX: &str = include_str!("../fixtures/discovery/migrate.json");
    const MIGRATE_V2_TX_V1: &str = include_str!("../fixtures/discovery/migrate_v2_tx_v1.json");
    const ALREADY_MIGRATED_TX: &str = include_str!("../fixtures/discovery/already_migrated.json");

    const MIGRATED_V2: (&str, &str) = (
        "6wegoUXDKBojcjL8BoSYVAqTL2o5iEaWwJxqBmYLpump",
        "5YvhDXpsgytA4kUwHGn4w8FFPAJwj1xhWLB63sz5DeXS",
    );
    const MIGRATED: (&str, &str) = (
        "4de4R5EtpWTGK2y2UzAv73f2zxGY2Bmb3giWkRz8pump",
        "E9PP5dWgJyqS37K1josmPuuTVzJBSkvUVQtYy6Grtizm",
    );
    /// Block time of the newest recorded signature.
    const NEWEST: i64 = 1_794_153_781;

    fn tx(reply: &str) -> Transaction {
        let reply: Value = serde_json::from_str(reply).unwrap();
        serde_json::from_value(reply["result"].clone()).unwrap()
    }

    fn edited(reply: &str, edit: impl FnOnce(&mut Value)) -> Transaction {
        let mut reply: Value = serde_json::from_str(reply).unwrap();
        edit(&mut reply["result"]);
        serde_json::from_value(reply["result"].clone()).unwrap()
    }

    fn pair((mint, pool): (&str, &str)) -> Vec<(String, String)> {
        vec![(mint.to_owned(), pool.to_owned())]
    }

    fn at(secs: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(secs).unwrap()
    }

    #[test]
    fn discriminators_match_the_idl() {
        // `discriminator` of `migrate` and `migrate_v2` in the pump IDL.
        let base58 = crate::pools::base58;
        assert_eq!(base58(&[155, 234, 231, 146, 236, 158, 162, 30]), MIGRATE);
        assert_eq!(base58(&[187, 203, 18, 31, 206, 237, 254, 41]), MIGRATE_V2);
    }

    #[test]
    fn parses_recorded_migrations() {
        assert_eq!(parse(&tx(MIGRATE_V2_TX)), pair(MIGRATED_V2));
        assert_eq!(parse(&tx(MIGRATE_TX)), pair(MIGRATED));
        assert_eq!(
            parse(&tx(MIGRATE_V2_TX_V1)),
            pair((
                "ErvM3kEjJeCJZ24RSs4H75nSHGxkNxZaDyDKb2odpump",
                "Hm9jCQsFW19Wj3jeDgXRGzERi5R8tuDCPGT2RxZHDdwy"
            ))
        );
        // Succeeds on chain, but the curve was migrated before.
        assert!(parse(&tx(ALREADY_MIGRATED_TX)).is_empty());
    }

    #[test]
    fn anything_unexpected_yields_nothing() {
        let failed = edited(MIGRATE_V2_TX, |r| {
            r["meta"]["err"] = serde_json::json!({"InstructionError": [1, {"Custom": 1}]});
        });
        let no_meta = edited(MIGRATE_V2_TX, |r| r["meta"] = Value::Null);
        let other_data = edited(MIGRATE_V2_TX, |r| {
            r["transaction"]["message"]["instructions"][1]["data"] = "3Bxs4VF5We".into();
        });
        let short_accounts = edited(MIGRATE_V2_TX, |r| {
            r["transaction"]["message"]["instructions"][1]["accounts"] = serde_json::json!([0, 1]);
        });
        let index_out_of_range = edited(MIGRATE_V2_TX, |r| {
            r["transaction"]["message"]["instructions"][1]["accounts"][2] = 9999.into();
        });
        // Lookup-table keys follow the 25 message keys, writable first; an
        // index into them needs the table.
        let from_lookup = |r: &mut Value| {
            r["transaction"]["message"]["instructions"][1]["accounts"][2] = 26.into();
        };
        assert_eq!(
            parse(&edited(MIGRATE_V2_TX, from_lookup)),
            pair((
                "2mgwNFGHcnKyPSYTXUe6duCZ7A2trEUcyoM4zVtnSdMK",
                MIGRATED_V2.1
            ))
        );
        let no_lookup = edited(MIGRATE_V2_TX, |r| {
            from_lookup(r);
            r["meta"]["loadedAddresses"] = Value::Null;
        });
        let other_amm = edited(MIGRATE_V2_TX, |r| {
            let accounts = &mut r["transaction"]["message"]["instructions"][1]["accounts"];
            accounts[9] = accounts[2].clone();
        });
        let no_cpi = edited(MIGRATE_V2_TX, |r| {
            r["meta"]["innerInstructions"] = serde_json::json!([]);
        });
        for tx in [
            failed,
            no_meta,
            other_data,
            short_accounts,
            index_out_of_range,
            no_lookup,
            other_amm,
            no_cpi,
        ] {
            assert!(parse(&tx).is_empty());
        }
        // A reply that does not fit the shape fails to decode, it does not panic.
        assert!(serde_json::from_str::<Transaction>(r#"{"transaction":{}}"#).is_err());
    }

    fn migration(mint: &str, migrated_at: OffsetDateTime) -> Migration {
        Migration {
            mint: mint.to_owned(),
            pool: "pool".to_owned(),
            migrated_at,
        }
    }

    #[test]
    fn keeps_mints_deduplicated_bounded_and_within_the_window() {
        let now = at(NEWEST);
        let mut discovery = Discovery::new();
        assert!(discovery.insert(migration("a", now - Duration::minutes(10))));
        assert!(!discovery.insert(migration("a", now - Duration::hours(1))));
        assert!(discovery.insert(migration("b", now - Duration::hours(1))));
        assert!(discovery.insert(migration("c", now - Duration::hours(71))));
        assert!(discovery.insert(migration("d", now - Duration::hours(73))));
        let due: Vec<&str> = discovery.due(now).iter().map(|m| m.mint.as_str()).collect();
        // `a` is too young (and keeps its first time), `d` too old.
        assert_eq!(due, ["b", "c"]);
        discovery.prune(now);
        assert!(discovery.knows("a") && !discovery.knows("d"));

        let mut full = Discovery::new();
        for i in 0..MAX_MINTS {
            let minutes = i64::try_from(i).unwrap();
            assert!(full.insert(migration(
                &format!("m{i}"),
                now - Duration::minutes(minutes)
            )));
        }
        // The oldest is dropped for a newer one; an even older one is refused.
        let oldest = format!("m{}", MAX_MINTS - 1);
        assert!(!full.insert(migration("older", now - Duration::days(30))));
        assert!(full.insert(migration("new", now)));
        assert_eq!(full.mints.len(), MAX_MINTS);
        assert!(full.knows("new") && !full.knows(&oldest));

        // Stored migrations come back with the same checks.
        let mint = "So11111111111111111111111111111111111111112";
        let pool = "DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263";
        let stored = |mint: &str, age| Migration {
            mint: mint.to_owned(),
            pool: pool.to_owned(),
            migrated_at: now - age,
        };
        let mut restored = Discovery::new();
        assert!(!restored.remember(stored("not base58 0OIl", Duration::hours(1)), now));
        assert!(!restored.remember(stored(mint, Duration::hours(73)), now));
        assert!(!restored.remember(stored(mint, -Duration::hours(1)), now));
        assert!(restored.remember(stored(mint, Duration::hours(1)), now));
        assert!(!restored.remember(stored(mint, Duration::hours(2)), now));
        assert_eq!(restored.migrations().count(), 1);

        // Block times in the future or beyond the window are refused.
        let mut discovery = Discovery::new();
        let v2 = tx(MIGRATE_V2_TX);
        let block = v2.block_time.unwrap();
        assert_eq!(
            discovery.record(&v2, Some(block), at(block) - Duration::hours(1)),
            0
        );
        assert_eq!(
            discovery.record(&v2, Some(block), at(block) + Duration::hours(73)),
            0
        );
        assert_eq!(discovery.record(&v2, None, at(block)), 0);
        assert_eq!(discovery.record(&v2, Some(block), at(block)), 1);
        assert_eq!(discovery.record(&v2, Some(block), at(block)), 0);
    }

    /// A provider answering `getSignaturesForAddress` with `signatures` and
    /// `getTransaction` from the recorded transactions (`null` otherwise).
    async fn provider(
        signatures: String,
    ) -> (RpcClient, std::sync::Arc<std::sync::Mutex<Vec<u8>>>) {
        let transactions: Vec<(String, &str)> = [MIGRATE_V2_TX, MIGRATE_TX, ALREADY_MIGRATED_TX]
            .into_iter()
            .map(|reply| {
                let value: Value = serde_json::from_str(reply).unwrap();
                let signature = value["result"]["transaction"]["signatures"][0]
                    .as_str()
                    .unwrap()
                    .to_owned();
                (signature, reply)
            })
            .collect();
        let (base, seen) = serve_with(move |request| {
            let request = String::from_utf8_lossy(request);
            if request.contains("getSignaturesForAddress") {
                return ok_json(&signatures);
            }
            let reply = transactions
                .iter()
                .find(|(signature, _)| request.contains(signature.as_str()))
                .map_or(r#"{"jsonrpc":"2.0","id":1,"result":null}"#, |(_, reply)| {
                    reply
                });
            ok_json(reply)
        })
        .await;
        (RpcClient::plain(0, &base), seen)
    }

    fn last_request(seen: &std::sync::Mutex<Vec<u8>>) -> String {
        String::from_utf8(seen.lock().unwrap().clone()).unwrap()
    }

    #[tokio::test]
    async fn polls_recorded_signatures_with_a_cursor() {
        let (rpc, seen) = provider(SIGNATURES.to_owned()).await;
        let now = at(NEWEST) + Duration::hours(1);
        let mut discovery = Discovery::new();
        let stats = discovery.poll_with(&rpc, now).await.unwrap();
        // 5 failed transactions are skipped unread; of the 15 read, two are
        // fresh migrations and one a no-op repeat.
        assert_eq!(
            stats,
            PollStats {
                signatures: 20,
                fetched: 15,
                found: 2,
                skipped: 18,
                gap: false,
            }
        );
        let newest = "53CYjoQmW2UVw8eZpFyae4RZvuaxycqNjFmEBmLTxLSoVVTrvyNjdtMV71JvcYAYW8Ka5PBv71dc6an8WsYaiHXh";
        assert_eq!(discovery.until.as_deref(), Some(newest));
        let due: Vec<(&str, &str)> = discovery
            .due(now)
            .iter()
            .map(|m| (m.mint.as_str(), m.pool.as_str()))
            .collect();
        assert_eq!(due, [MIGRATED_V2, MIGRATED]);

        // A provider that answers the same page again adds nothing.
        let stats = discovery.poll_with(&rpc, now).await.unwrap();
        assert_eq!(stats.found, 0);
        let request = last_request(&seen);
        assert!(request.contains(r#""method":"getTransaction""#));
        assert!(request.contains(r#""maxSupportedTransactionVersion":1"#));
        assert_eq!(discovery.due(now).len(), 2);
    }

    fn page(signatures: impl Iterator<Item = String>) -> String {
        let entries: Vec<Value> = signatures
            .map(|signature| serde_json::json!({"signature": signature, "blockTime": NEWEST, "err": null}))
            .collect();
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": entries}).to_string()
    }

    fn signature(i: usize) -> String {
        format!("{i:0>88}").replace('0', "z")
    }

    #[tokio::test]
    async fn caps_reads_per_poll_and_resumes_from_the_cursor() {
        // Newest first, like the RPC: 0 is the oldest.
        let (rpc, _) = provider(page((0..MAX_SIGNATURES_PER_POLL).rev().map(signature))).await;
        let mut discovery = Discovery::new();
        let now = at(NEWEST);
        let stats = discovery.poll_with(&rpc, now).await.unwrap();
        assert_eq!(
            (stats.fetched, stats.gap),
            (MAX_TRANSACTIONS_PER_POLL, false)
        );
        let cursor = signature(MAX_TRANSACTIONS_PER_POLL - 1);
        assert_eq!(discovery.until.as_deref(), Some(cursor.as_str()));
        // A full page after a cursor may have skipped signatures.
        assert!(discovery.poll_with(&rpc, now).await.unwrap().gap);

        // Polls after the first stop before the cursor.
        let (rpc, seen) = provider(page(std::iter::empty())).await;
        discovery.poll_with(&rpc, now).await.unwrap();
        assert!(last_request(&seen).contains(r#""until":"#));

        // Signatures older than the window are skipped unread.
        let (rpc, _) = provider(page((0..3).map(signature))).await;
        let stats = Discovery::new()
            .poll_with(&rpc, at(NEWEST) + MAX_AGE + Duration::minutes(1))
            .await
            .unwrap();
        assert_eq!((stats.fetched, stats.skipped), (0, 3));
    }

    #[tokio::test]
    async fn malformed_pages_fail_and_transient_errors_keep_the_cursor() {
        let now = at(NEWEST);
        for bad in [
            page(["not a signature".to_owned()].into_iter()),
            page((0..=MAX_SIGNATURES_PER_POLL).map(signature)),
            r#"{"jsonrpc":"2.0","id":1,"result":[{"signature":5}]}"#.to_owned(),
        ] {
            let (rpc, _) = provider(bad).await;
            let mut discovery = Discovery::new();
            assert!(discovery.poll_with(&rpc, now).await.is_err());
            assert_eq!(discovery.until, None);
        }

        // A rate-limited `getTransaction` ends the poll; the cursor stays on
        // the last signature handled (the two oldest are failed ones).
        let (base, _) = serve_with(|request| {
            if String::from_utf8_lossy(request).contains("getSignaturesForAddress") {
                ok_json(SIGNATURES)
            } else {
                response("429 Too Many Requests", "retry-after: 1\r\n", "")
            }
        })
        .await;
        let mut discovery = Discovery::new();
        let result = discovery.poll_with(&RpcClient::plain(0, &base), now).await;
        assert!(matches!(result, Err(MarketDataError::RateLimited { .. })));
        assert_eq!(
            discovery.until.as_deref(),
            Some("X49WFowxhpKqFqj1uStTPEiaZnLZHQHeNv1tFQHD2hWutTNpELfv4e7cJqXMEDEZfPADDY4mt8Y2ZrVgd4TZ5sY")
        );
    }
}
