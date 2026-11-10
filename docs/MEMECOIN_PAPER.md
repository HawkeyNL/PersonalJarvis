# Memecoin paper trader

Core can run the on-chain memecoin strategies as a **paper** simulation: it
reads public market data, applies the hard filters and several typed
strategies from `jarvis-onchain` side by side, and simulates fills, fees and
failed attempts. It has no
wallet, no signer and no execution path. When the profile allows it, Claude
decides the paper entries and exits and proposes strategy versions (below),
only inside the paper book and only past the same code checks. Results are
stored in schemas 0014 and 0017 and shown through `/v1/trading/meme/*`.

It is **off by default** (`JARVIS_MEMECOIN_PAPER_ENABLED=false`). Any
configuration problem at startup leaves it off and logs why; Core keeps
running.

## Enabling it (owner)

1. Write the paper profile (below) to `/etc/jarvis/memecoin-paper.json`,
   owned by root, with the group of `/etc/jarvis` and mode `0640`. Production
   refuses any other owner, mode or a symlink. No API writes this file.
2. Put the market-data secrets in `/etc/jarvis/secrets/market-data.env`, with
   the same owner and mode as the other provider files there. The Core unit
   loads it as an optional `EnvironmentFile`, like the LLM provider keys:
   - `JARVIS_MARKET_SOLANA_RPC_A_URL` and `JARVIS_MARKET_SOLANA_RPC_B_URL`:
     two HTTPS Solana RPC endpoints on **different** hosts from the
     market-data allowlist (`api.mainnet-beta.solana.com`,
     `mainnet.helius-rpc.com`, `solana-mainnet.g.alchemy.com`). Every read is
     made on both and must agree. The URLs usually contain a provider key;
     Core redacts them in its logs.
   - `JARVIS_MARKET_JUPITER_API_KEY` (optional): without it, quotes are
     paced to one per 2.1 s.
3. Set `JARVIS_MEMECOIN_PAPER_ENABLED=true` in `/etc/jarvis/core.env` and
   restart Core. The log shows `memecoin PAPER trader running` or the reason
   it stayed off; the overview then reports `running` or a `last_error`.

`jarvis-credentials` does not manage `market-data.env` yet; a provider that
reads these values from the TTY and probes them read-only is a follow-up.
Until then the owner writes the file by hand.

## Paper profile

A JSON object; unknown fields are refused. Amounts are decimal strings, ratios
in basis points, durations in seconds (at most 30 days). The repository ships
no values on purpose.

- `watchlist`: 1–20 distinct base58 mints to observe.
- `discovery` (optional, default `true`): also observe mints from pump.fun
  migrations (below). `false` limits the trader to the watchlist.
- `goal_multiple`: target multiple of the stake (above 1), for progress only.
- `stake_usd`, `max_position_usd`, `max_open_positions` (1–10): simulation
  inputs only, never limits for live trading. The stake must cover
  `max_position_usd × max_open_positions`.
- `filter`: `min_liquidity_usd`, `min_pool_age_secs`, `max_top_holders_share`
  (fraction), `max_sell_impact_bps`, `allowed_extensions` (snake_case
  Token-2022 extension names), `max_snapshot_age_secs`,
  `min_lp_lock_remaining_secs`.
- `strategy` (optional, **deprecated**): `version` and the `StrategyParams`
  fields of `jarvis-onchain` (`*_secs` for durations); validated against the
  code-fixed bounds, with `version` below 1 000 000. This single strategy of
  older profiles no longer enters. It is still read so the positions it
  opened keep exiting by its rules; drop it once none are held.
- `costs`: `dex_fee_bps`, `network_fee_usd`, `priority_fee_usd`,
  `failure_rate` (fraction, below 1).
- `agent` (optional): Claude decides entries and exits and reviews the
  strategies daily (below). Absent: off, and each strategy enters on its own
  signal. Fields, all optional: `max_calls_per_hour` (1–4, default 4) and
  `max_calls_per_day` (1–48, default 48), which can only lower the code caps,
  and `new_strategies_enabled` (default `true`).

While the profile has a `strategy`, everything except `watchlist`,
`discovery` and `goal_multiple` is recorded with its version. Changing any of
it without raising `strategy.version` keeps the trader off; that includes
rewriting `"25"` as `"25.0"`, since decimals keep their scale. The filter
rules, costs and sizes apply to every strategy and are not versioned with the
typed strategies.

## Strategies

At startup Core records the seed strategies `a-survivor-momentum`,
`b-base-reclaim` and `c-established-trend` (families A, B and C of
`jarvis-onchain::config`) as versions 1 000 001 to 1 000 003, author `seed`,
and enables each one it creates. Versions are unique across strategies;
typed versions start above 1 000 000 and profile versions stay below, so
they never collide. Seeding is idempotent: an existing strategy keeps its
active version and its enabled or owner-disabled state. A seed whose filter
overrides would loosen the profile's filter rules, or whose version is
already recorded with other content, is left out and logged; the others
still run.

Each tick, every **enabled** strategy's active version is evaluated, in id
order:

- The hard filter runs with the profile's rules tightened by the version's
  overrides. A candidate row is written per mint and strategy version, when
  that verdict changes.
- The entry conditions read signals computed from our own recorded
  observations: every price and liquidity seen in the last 25 hours plus the
  daily ones before, at most 2000 points, ending with the snapshot an entry
  would fill at.
- The entry size is the version's share of `max_position_usd`, capped by its
  share of pool liquidity.
- Limits: the profile's `max_open_positions` and one position per mint
  across all strategies; per strategy an equal share of `max_open_positions`
  among the enabled strategies (at least one), its `max_entries_per_day` per
  UTC day, and its `token_cooldown_secs` since it last left that mint.
- The first strategy whose limits and signal allow it makes the one entry
  attempt for that mint this tick; its trip and any failed-attempt cost carry
  that strategy's version. With `agent` set it does not enter: the mint
  becomes an eligible candidate for Claude's next decision instead.

A **disabled** strategy makes no new entries; its open positions still exit.
Every position exits by the rules of the version that opened it: liquidity
drop, stop loss, trailing stop (the version's `trailing_stop_bps` below the peak executable value, once
that peak was above cost; the peak is stored with the position, so a restart
keeps it), take profit and time stop. Positions of a legacy profile version
exit by the parameters recorded for that version. A version that no longer
fits the profile's filter rules only exits. A position whose version has no
usable rules recorded gets only the time stop, after 30 days, and is logged
every tick until then.

The overview reports the whole paper book (every strategy). The hourly gate
snapshot is written per strategy, for its active version; one that fails is
logged and skipped until the next hour. Typed versions are only recorded
above 1 000 000, with parameters that name the same strategy, version,
parent and family as their row. Trades, net result
(after failed-attempt fees and AI cost), expectancy, win rate and maximum
drawdown per strategy, over all its versions, are shown per strategy (API
below).

## Claude decisions

With `agent` set, Claude decides which eligible candidates to enter, how
much, and which open positions to exit early. Code exits (stop loss, trailing
stop, take profit, liquidity drop, time stop) always run first, every tick,
whatever Claude does or whether it answers at all.

- **Input: numbers only.** Candidates are `c0`…`c9` (at most 10, newest
  first) with their strategy, liquidity, sell impact and every signal value
  (pool age is `pool_age_minutes`); open positions are `p0`…`p9` with net
  P&L, peak P&L and hold time; strategies are `s0`… with their open
  positions, limits and paper results relative to the stake; plus the free
  slots and free stake as shares. No mint, token name, strategy name or other
  text is sent, and no rationale Claude wrote is ever sent back. A test
  checks that an input holds only fixed keys, enum names, opaque ids and
  numbers.
- **Output: validated whole.** At most 16 KiB of one JSON object with unknown
  fields denied: `enter` (candidate id, `size_bps` 1–10 000, a snake_case
  `reason_code` of at most 32 characters), `exit` (position id) or a lone
  `hold`, and an optional `rationale` (control characters stripped, at most
  300 characters). Ids must come from the input and appear once, and the
  entries' `size_bps` sum to at most 10 000. Anything else drops the whole
  reply, recorded as `invalid_output` with the reason.
- **Checked again.** An entry still needs an intact stored ledger (the
  limits are counted from it; otherwise `book_unavailable`) and, on a fresh
  observation: its strategy enabled at the same version, no position in the mint, a free slot,
  the strategy's open, daily and cooldown limits, and the hard filter. Its
  size is `size_bps` of what the strategy would enter (its share of
  `max_position_usd`, capped by its liquidity share), within the free paper
  stake. A refused action is recorded as `skip` with the reason (for example
  `strategy_limit`, `no_slot`, `liquidity_too_low`). Exits sell at the tick's
  price with exit reason `agent_exit`.
- **Recorded.** Every call starts with a `skip` row with reason
  `call_started`, written before the call is made; if it cannot be written,
  the call is not made. Then every action and failure is a decision row:
  `enter`, `skip`, `exit`, `hold`, `worker_unavailable` or
  `invalid_output`, with the model. The call's token counts are on its first
  row after the reply only, so rows add up.

**Budget.** A decision call runs only when there are eligible candidates or
open positions. Decision and review calls together run at most every 15
minutes, 4 per hour and 48 per day (rolling; `agent` can only lower the last
two). A decision Claude answered is carried out early in the next tick,
before new candidates are observed; each fresh observation for one of its
entries is cut off after 5 seconds, and entries still left after 20 seconds
are skipped (`apply_deadline`). The budget is counted from the decision
rows, including the `call_started` row of a call still running or cut off
by a restart, so a restart does not reset it. A busy or absent worker (it shares one
background slot with research) skips that cycle without an alarm. A plan
limit backs every call off for 1, 2, then 6 hours. Three invalid replies in a
row pause decisions for 6 hours; the overview shows `paused_until` and
`backoff_until`. A pause by the owner stops new calls and Claude's entries;
exits continue. Claude runs on the subscription worker only (never the
router or a paid API), so its cost is counted as zero in the paper results.

## Strategy review

Once a day (an unanswered review is retried after an hour) Claude's hard
model reviews the strategies: their active configs, with the strategy id
replaced by `s0`…, enabled as 0 or 1, and their paper results, again numbers
and enum names only. It may return at most 3 proposals: a changed config for
an existing strategy, or a config for a new one (`strategy_id` `"new"`).
Each one is checked before it is recorded:

- `StrategyConfig::validate` against the profile's filter rules: anything
  outside the code and family bounds, or a filter override that loosens the
  profile, is refused (`out_of_bounds`, `loosening`);
- the parent (the `version` Claude kept) must be a version of the same
  strategy (`parent_mismatch`); the family cannot change
  (`family_changed`); an unchanged config is refused (`unchanged`);
- at most 2 new versions per rolling day (`version_cap`) and at most 6
  strategies (`strategy_cap`).

An accepted proposal is backtested on the stored observations of recently
migrated mints (at most 50; with fewer than 5 mints or 500 points it is
marked `unbacktested`), recorded as a new version with author `claude`, its
parent and its rationale (at most 600 characters, display only), and becomes
the strategy's active version. A strategy the owner disabled stays disabled:
Claude can never enable a strategy. A new strategy is named
`claude-<version>` and starts enabled unless `new_strategies_enabled` is
false. Each proposal is recorded as a `propose` row with `accepted` or the
reason it was refused.

## Owner controls

- **Disable** is immediate and needs only the owner's session: entries of
  that strategy stop within the running tick (also when the database cannot
  be reached; a `503` then means the disable holds only until the next
  restart, so retry), and it holds as the owner's disable. Its positions keep exiting. Audited as
  `memecoin.strategy.disable`.
- What a signature enables is the **strategy**, not one version: while it
  stays enabled, later versions Claude writes for it become active without a
  new signature, each within `StrategyConfig::validate` against the
  profile's rules and the review's caps. To stop that, disable the strategy.
- **Enable** needs a device signature, from a device that can approve
  (iOS, Android or macOS): ask for a challenge, then sign
  `meme_strategy_enable_message` (`jarvis-client-core`, domain
  `jarvis-meme-strategy-enable-v1`) over the strategy id, the active version,
  the SHA-256 of that version's parameter JSON, the challenge nonce and the
  device id. A challenge lives 5 minutes in memory, one per device, and is
  used up by the next enable call whatever its outcome. If the strategy's
  active version changed meanwhile, the enable is refused. Audited as
  `memecoin.strategy.enable_challenge` and `memecoin.strategy.enable`.
- Every strategy change shows in its version list with Core's per-field diff
  to the parent, the rationale and the backtest.

## What a tick does

Every 60 s: value held positions and simulate exits (also while paused);
then, if the stored ledger replays exactly, a strategy is enabled and the
trader is not paused, poll discovery (at most every 2 minutes), observe up to
5 mints (watchlist first, then discovered mints; each at most every 15
minutes, through the per-provider pacing of `jarvis-market-data`), run the
filter and the strategies, and simulate entries; finally report from the
stored ledger. The price and liquidity of every held, watchlist and observed
mint are recorded each tick for the signals. Discovered mints are only seen
when they are observed as candidates, so their series are sparser than the
watchlist's. A
write failure stops the tick before anything is simulated on it.

- An exit that fails or cannot be priced is retried on later ticks; after 10
  such attempts the position is marked `stuck` (shown in the overview), is
  no longer retried and keeps its slot. Failed-attempt fees are summed per
  day, so they cannot grow the ledger without bound.
- If the stored ledger no longer replays exactly or exceeds 100 000 rows,
  entries and the report stop until the owner investigates; exits continue.
  The overview shows a fixed reason as `last_error` (details are in the
  log, reported once).
- A ledger dated more than five minutes ahead of the clock stops the whole
  tick, so events are never stamped into the future.
- If the pause state cannot be read, that tick makes no entries.

A candidate row is written only when a mint's verdict changes. Daily
housekeeping prunes, in bounded batches:

- candidate rows and the hourly gate snapshots after 30 days;
- strategy-agent decisions after 90 days;
- price and liquidity observations after 72 hours, except the first one of
  each mint and UTC day, which is kept for 60 days;
- universe entries (recently migrated tokens) 7 days after the migration.

Trips, costs, strategies and strategy versions are kept for the paper gate.

**LP status:** the LP of the main pool (the deepest DexScreener pair; other
pools of the token are not checked) is read on chain, only for PumpSwap,
Raydium AMM v4 and Raydium CPMM pools. The pool account must be owned by one
of those programs and hold the pair's two mints. LP counts as burned when the
LP mint supply is zero, or when at most 1% of it sits outside accounts owned
by the incinerator or, for PumpSwap, the pool itself. Everything else (other
venues such as CLMM or DLMM, LP in a wallet, a Raydium pool authority or a
third-party locker, unreadable data, the two RPC providers disagreeing) is
rejected with `lp_not_secured`; time locks are never recognised. The verified
pool vaults are left out of the top-holder share.

## Discovery

When a pump.fun bonding curve completes, pump.fun migrates it to a PumpSwap
pool. Every migration writes to the pump.fun migration account
(`39azUYFWPz3VHgKCf3VChUwbpURdCHRxjWVowf5jUJjg`, the `withdraw_authority` of
pump.fun's Global account), so discovery polls `getSignaturesForAddress` on
that account and reads each new transaction with `getTransaction`. A
transaction counts only if a top-level pump.fun `migrate` or `migrate_v2`
instruction succeeded and called PumpSwap; a repeated migrate on an already
migrated curve is ignored. Its mint and pool are kept.

- At most 200 signatures per poll and 25 transactions read per poll; the
  rest is read on the next poll. A poll is cut off after 15 s.
- Failed transactions and signatures older than 72 hours are skipped
  unread, and a transaction that does not parse is skipped. A malformed
  signature list, a rate limit or a transport error ends the poll, which
  resumes at the same signature later.
- At most 2000 mints are kept, each once, for 72 hours after its migration
  (the oldest is dropped first). A mint is offered as a candidate from
  30 minutes after its migration on. With 5 observations per tick, shared
  with the watchlist, a busy day can leave some unobserved.
- Discovered mints are stored in the universe table after each poll, and
  those within the 72-hour window are loaded again at startup, so a restart
  keeps them. The poll position is not stored: after a restart discovery
  starts from the newest 200 signatures.

Discovery reads only the primary RPC provider. A discovered mint is just a
name to look at: it goes through the same DexScreener pair, cross-checked
RPC reads and hard filters as a watchlist mint, so a wrong answer can only
cost a candidate, never let one through.

## API (owner session, like every `/v1` route)

- `GET /v1/trading/meme/overview`: enabled, running, paused, stake, equity,
  P&L net of fees, failed attempts and AI cost, open positions (each with its
  strategy version), gate report and goal progress, for the whole paper
  book; with `agent` set, Claude's call budget (`agent`). The top-level `strategy_version` is **deprecated** and kept for
  older clients: it is the active version of the first strategy by id
  (normally `a-survivor-momentum`) and scopes nothing else in the response.
- `GET /v1/trading/meme/candidates?limit=1..100` (default 50).
- `GET /v1/trading/meme/trades?limit=1..100`: closed paper trades.
- `POST /v1/trading/meme/pause`: stops entries at once and persists the pause
  (security audit `memecoin.paper.pause`). Exits continue. A `503` means the
  pause holds only until the next restart; retry until it answers `200`.
- `GET /v1/trading/meme/strategies`: every strategy with its family, active
  version, enabled and owner-disabled state, last change and paper results.
- `GET /v1/trading/meme/strategies/{id}`: the same, plus the SHA-256 of the
  active parameters, the 20 newest versions (config, author, parent,
  per-field `diff` to the parent, rationale, backtest) and the latest hourly
  gate snapshot of the active version.
- `POST /v1/trading/meme/strategies/{id}/disable`: disable at once.
- `POST /v1/trading/meme/strategies/{id}/enable/challenge`: `version`,
  `config_sha256`, `config_json` (the exact bytes the hash covers), `nonce`
  (hex), `expires_at` and `message` (`meme-strategy-enable-v1`); `409` when
  the strategy is enabled, `403` from a device that cannot approve.
- `POST /v1/trading/meme/strategies/{id}/enable` with `{"signature": hex}`:
  `401` for a bad signature, `409` without an open challenge or when the
  strategy changed since.
- `GET /v1/trading/meme/decisions?limit=1..100` (default 50): the strategy
  agent's decision rows, newest first. Rationales are display only.

Strategy writes are limited to 30 per device and minute. Core sends the
realtime event `trading.meme.strategy_changed` (`strategy_id`, `version`,
`enabled`) when a strategy's active version or enabled state changed, and
`trading.meme.decision` (`id`) for each decision row the strategy agent
stored. Both are hints to reload the routes above and carry no market data
or rationale.

## Resuming

There is no resume over HTTP. To clear a pause, copy `paused_at` from the
overview into `JARVIS_MEMECOIN_PAPER_RESUME_PAUSED_AT` in `core.env` and
restart Core. Only the pause made at exactly that time is cleared, so the
setting can never undo a later pause; remove the line afterwards.
