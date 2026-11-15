BEGIN TRANSACTION;
-- Read-only crypto wallets: public addresses the owner added and what Core
-- last read for them. Never a key, a seed or a transaction. Single owner.
-- Amounts and prices are decimal strings, never floats (like 0015). Only
-- assets on Core's allowlist are stored, under the allowlist's own key.
-- Additive: only new, empty tables, so defining their indexes here is safe
-- (see `apply_baseline_schema`).

-- One row per address. One EVM address covers the EVM chains it lists.
-- Core normalises addresses (Bitcoin bech32 lower case, EVM lower-case hex,
-- base58 as given) and caps the table at 20 rows; the patterns here are a
-- backstop for its checksum checks, not a replacement.
DEFINE TABLE IF NOT EXISTS crypto_wallets SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON crypto_wallets TYPE string ASSERT record::id($value) = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
DEFINE FIELD kind ON crypto_wallets TYPE string ASSERT $value IN ['bitcoin', 'evm', 'solana'];
DEFINE FIELD address ON crypto_wallets TYPE string ASSERT (kind = 'bitcoin' AND $value = /^(bc1[02-9ac-hj-np-z]{6,87}|[13][1-9A-HJ-NP-Za-km-z]{25,34})$/) OR (kind = 'evm' AND $value = /^0x[0-9a-f]{40}$/) OR (kind = 'solana' AND $value = /^[1-9A-HJ-NP-Za-km-z]{32,44}$/);
DEFINE FIELD chains ON crypto_wallets TYPE array<string, 5> ASSERT array::len(array::distinct($value)) = array::len($value) AND ((kind = 'bitcoin' AND $value = ['bitcoin']) OR (kind = 'solana' AND $value = ['solana']) OR (kind = 'evm' AND array::len($value) >= 1 AND $value ALLINSIDE ['ethereum', 'base', 'arbitrum', 'optimism', 'polygon']));
DEFINE FIELD label ON crypto_wallets TYPE option<string> ASSERT $value = NONE OR (string::len($value) >= 1 AND string::len($value) <= 40);
DEFINE FIELD created_at ON crypto_wallets TYPE datetime;
-- The last refresh that read every chain of the wallet, and the code of the
-- last failed one (NONE after a good one). Solana tokens off the allowlist
-- are kept only as a count.
DEFINE FIELD last_ok_at ON crypto_wallets TYPE option<datetime>;
DEFINE FIELD error ON crypto_wallets TYPE option<string> ASSERT $value = NONE OR $value IN ['unavailable', 'rate_limited', 'invalid_response', 'too_many_tokens'];
DEFINE FIELD other_tokens ON crypto_wallets TYPE option<int> ASSERT $value = NONE OR (kind = 'solana' AND $value >= 0 AND $value <= 100000);
DEFINE INDEX IF NOT EXISTS crypto_wallets_id ON crypto_wallets FIELDS id UNIQUE;
DEFINE INDEX IF NOT EXISTS crypto_wallets_address ON crypto_wallets FIELDS kind, address UNIQUE;

-- One row, `crypto_wallet_guards:create`, that every wallet create writes
-- first in its transaction (like `scheduled_job_guards`): two creates at the
-- same time write the same key and only one commits, so both cannot pass the
-- 20-wallet check on the same count.
DEFINE TABLE IF NOT EXISTS crypto_wallet_guards SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD IF NOT EXISTS touched_at ON crypto_wallet_guards TYPE datetime;

-- Balances of the last good refresh of a wallet, one row per chain and
-- allowlisted asset, replaced as a whole with the wallet's refresh. The row
-- id is "{wallet_id}:{chain}:{asset}".
DEFINE TABLE IF NOT EXISTS crypto_balances SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON crypto_balances TYPE string;
DEFINE FIELD wallet_id ON crypto_balances TYPE string ASSERT $value = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
DEFINE FIELD chain ON crypto_balances TYPE string ASSERT $value IN ['bitcoin', 'ethereum', 'base', 'arbitrum', 'optimism', 'polygon', 'solana'];
DEFINE FIELD asset ON crypto_balances TYPE string ASSERT $value = /^[a-z0-9]{2,12}$/;
DEFINE FIELD amount ON crypto_balances TYPE string ASSERT $value = /^[0-9]{1,20}(\.[0-9]{1,28})?$/;
DEFINE FIELD as_of ON crypto_balances TYPE datetime;
DEFINE INDEX IF NOT EXISTS crypto_balances_id ON crypto_balances FIELDS id UNIQUE;
DEFINE INDEX IF NOT EXISTS crypto_balances_wallet ON crypto_balances FIELDS wallet_id;

-- EUR price per allowlisted asset. The row id is the asset key; `as_of` is
-- the price's own time, not the time Core read it.
DEFINE TABLE IF NOT EXISTS crypto_prices SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON crypto_prices TYPE string ASSERT record::id($value) = /^[a-z0-9]{2,12}$/;
DEFINE FIELD eur ON crypto_prices TYPE string ASSERT $value = /^[0-9]{1,20}(\.[0-9]{1,28})?$/ AND $value = /[1-9]/;
DEFINE FIELD as_of ON crypto_prices TYPE datetime;
DEFINE INDEX IF NOT EXISTS crypto_prices_id ON crypto_prices FIELDS id UNIQUE;

UPDATE schema_version:baseline SET version = 23, applied_at = time::now();
COMMIT TRANSACTION;
