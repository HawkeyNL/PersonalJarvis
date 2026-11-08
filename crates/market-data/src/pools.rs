//! On-chain LP status of the pool a DexScreener pair names.
//!
//! The pair's pool address is untrusted. The pool account is read through
//! [`CrossCheckedRpc`], its owner must be one of three allowlisted programs,
//! and its two mints must be the pair's base and quote mints. Only then are
//! its vaults trusted and its LP mint read:
//!
//! - LP mint supply `0`: every LP token is burned, [`LpStatus::Burned`];
//! - otherwise the largest LP token accounts are read, and the LP counts as
//!   burned only when at most 1% of the supply sits outside accounts owned by
//!   the incinerator, the pool itself or the program's pool authority.
//!
//! Anything else stays [`LpStatus::Unknown`]: other programs (CLMM, DLMM and
//! every other venue), a layout surprise, mints that do not match, LP held by
//! a wallet or a third-party locker. Read errors, including the two providers
//! disagreeing, are returned as errors; the caller maps them to `Unknown`.
//!
//! Layouts and program ids, checked 2026-11-08 against:
//!
//! - PumpSwap `Pool`: pump_amm IDL,
//!   <https://github.com/pump-fun/pump-public-docs/blob/main/idl/pump_amm.json>;
//! - Raydium AMM v4 `AmmInfo` (`#[repr(C, packed)]`, 752 bytes, no
//!   discriminator): <https://github.com/raydium-io/raydium-amm/blob/master/program/src/state.rs>;
//! - Raydium CPMM `PoolState`:
//!   <https://github.com/raydium-io/raydium-cp-swap/blob/master/programs/cp-swap/src/states/pool.rs>;
//! - Raydium pool authorities: `AMM_V4` / `CREATE_CPMM_POOL_AUTH` in
//!   <https://github.com/raydium-io/raydium-sdk-V2/blob/master/src/common/programId.ts>,
//!   and the owner of recorded mainnet pool vaults.
//!
//! Anchor discriminators are `sha256("account:<Name>")[..8]`.
// ponytail: the 1% is of the LP mint supply, not of the pool's own LP total,
// so a burned pool where a third party later added more than 1% of the
// remaining LP stays `Unknown`. Weigh against the pool's LP field if that
// rejects too many good pools.

use std::ops::RangeInclusive;

use jarvis_onchain::types::LpStatus;

use crate::dexscreener::Pair;
use crate::rpc::{CrossChecked, CrossCheckedRpc, RawAccount};
use crate::token2022::TokenProgram;
use crate::MarketDataError;

pub const PUMPSWAP_PROGRAM: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";
pub const RAYDIUM_AMM_V4_PROGRAM: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";
pub const RAYDIUM_CPMM_PROGRAM: &str = "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C";

/// Tokens sent here can never move again: nobody holds its key.
const INCINERATOR: &str = "1nc1nerator11111111111111111111111111111111";

/// LP token accounts read per pool, largest first. A burn not proven within
/// these stays `Unknown`.
const MAX_HOLDER_READS: usize = 4;

/// SPL Token account (also the start of a Token-2022 one): mint at 0, owner
/// at 32, amount (u64 LE) at 64, `COption` delegate tag (u32 LE) at 72.
const TOKEN_ACCOUNT_LEN: usize = 165;

/// Where one program keeps the fields we read. Offsets include the Anchor
/// discriminator.
struct Layout {
    program: &'static str,
    discriminator: Option<[u8; 8]>,
    len: RangeInclusive<usize>,
    mints: [usize; 2],
    lp_mint: usize,
    vaults: [usize; 2],
    /// The program-wide PDA that owns the vaults. PumpSwap vaults are owned
    /// by the pool account itself.
    authority: Option<&'static str>,
}

const LAYOUTS: [Layout; 3] = [
    // pool_bump u8, index u16, creator, base_mint, quote_mint, lp_mint,
    // pool_base_token_account, pool_quote_token_account, lp_supply u64, ...
    // Newer fields follow, so only a minimum length is fixed.
    Layout {
        program: PUMPSWAP_PROGRAM,
        discriminator: Some([241, 154, 109, 4, 17, 177, 109, 188]),
        len: 211..=usize::MAX,
        mints: [43, 75],
        lp_mint: 107,
        vaults: [139, 171],
        authority: None,
    },
    // 16 u64 params, Fees (8 u64), StateData (64 + 80 bytes), coin_vault,
    // pc_vault, coin_vault_mint, pc_vault_mint, lp_mint, ...
    Layout {
        program: RAYDIUM_AMM_V4_PROGRAM,
        discriminator: None,
        len: 752..=752,
        mints: [400, 432],
        lp_mint: 464,
        vaults: [336, 368],
        authority: Some("5Q544fKrFoe6tsEbD7S8EmxGTJYAKtTVhAW5Q5pge4j1"),
    },
    // amm_config, pool_creator, token_0_vault, token_1_vault, lp_mint,
    // token_0_mint, token_1_mint, ...
    Layout {
        program: RAYDIUM_CPMM_PROGRAM,
        discriminator: Some([247, 237, 227, 245, 215, 195, 222, 70]),
        len: 637..=usize::MAX,
        mints: [168, 200],
        lp_mint: 136,
        vaults: [72, 104],
        authority: Some("GpMZbSM2GgvTKHJirzeGfMFoaZ8UR2X7F4v8vHTvxFbL"),
    },
];

/// A decoded pool of an allowlisted program. Addresses are base58.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pool {
    pub address: String,
    pub mints: [String; 2],
    pub lp_mint: String,
    pub vaults: [String; 2],
    authority: Option<&'static str>,
}

impl Pool {
    /// The pool holds exactly these two mints, in either order.
    fn holds(&self, a: &str, b: &str) -> bool {
        let [x, y] = &self.mints;
        (x == a && y == b) || (x == b && y == a)
    }

    /// Owners whose LP tokens can never be withdrawn by anyone.
    fn burn_owners(&self) -> Vec<&str> {
        let mut owners = vec![INCINERATOR, self.address.as_str()];
        owners.extend(self.authority);
        owners
    }
}

/// What [`read`] found about a pair's pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolLp {
    pub status: LpStatus,
    /// The pool's two token vaults, read from the verified pool account; the
    /// token's top-holder share must leave them out. Empty when the pool
    /// could not be verified.
    pub vaults: Vec<String>,
}

/// Decode `account` (at `address`) if its owner is an allowlisted program
/// and the data has that program's pool layout.
pub fn decode(address: &str, account: &RawAccount) -> Option<Pool> {
    let layout = LAYOUTS.iter().find(|l| l.program == account.owner)?;
    let data = &account.data;
    if !layout.len.contains(&data.len())
        || layout
            .discriminator
            .is_some_and(|d| data.get(..8) != Some(&d[..]))
    {
        return None;
    }
    let key = |offset: usize| base58(&data[offset..offset + 32]);
    Some(Pool {
        address: address.to_owned(),
        mints: layout.mints.map(key),
        lp_mint: key(layout.lp_mint),
        vaults: layout.vaults.map(key),
        authority: layout.authority,
    })
}

/// Read the LP status of `pair`'s pool. The result is bound to the pair's
/// base mint.
pub async fn read(
    rpc: &CrossCheckedRpc,
    pair: &Pair,
) -> Result<CrossChecked<PoolLp>, MarketDataError> {
    let account = rpc.account(&pair.pair_address, "pool account").await?;
    let pool = decode(&pair.pair_address, &account)
        .filter(|pool| pool.holds(&pair.base_token.address, &pair.quote_token.address));
    let value = match pool {
        None => PoolLp {
            status: LpStatus::Unknown,
            vaults: Vec::new(),
        },
        Some(pool) => PoolLp {
            status: lp_status(rpc, &pool).await?,
            vaults: pool.vaults.to_vec(),
        },
    };
    Ok(CrossChecked {
        mint: pair.base_token.address.clone(),
        value,
    })
}

async fn lp_status(rpc: &CrossCheckedRpc, pool: &Pool) -> Result<LpStatus, MarketDataError> {
    let supply = rpc.mint(&pool.lp_mint).await?.value.supply;
    if supply == 0 {
        return Ok(LpStatus::Burned);
    }
    let largest = rpc.largest_accounts(&pool.lp_mint).await?.value;
    let mut seen: Vec<&str> = Vec::new();
    let mut holders = Vec::new();
    for balance in largest.iter().filter(|b| b.amount > 0) {
        if seen.len() == MAX_HOLDER_READS {
            break;
        }
        // A duplicate entry must not count twice.
        if seen.contains(&balance.address.as_str()) {
            continue;
        }
        seen.push(&balance.address);
        let account = rpc.account(&balance.address, "LP token account").await?;
        let owner = token_account_owner(&account, &pool.lp_mint, balance.amount);
        holders.push((balance.amount, owner));
    }
    Ok(burned_or_unknown(supply, &holders, &pool.burn_owners()))
}

/// The owner of a token account of `mint` holding exactly `amount` with no
/// delegate (who could move the tokens without the owner), or `None` if the
/// account is anything else.
fn token_account_owner(account: &RawAccount, mint: &str, amount: u64) -> Option<String> {
    TokenProgram::from_owner(&account.owner)?;
    let data = account.data.get(..TOKEN_ACCOUNT_LEN)?;
    let held = u64::from_le_bytes(data[64..72].try_into().ok()?);
    let no_delegate = data[72..76] == [0; 4];
    (base58(&data[..32]) == mint && held == amount && no_delegate).then(|| base58(&data[32..64]))
}

/// `Burned` when at most 1% of `supply` is outside `holders` owned by one of
/// `burn_owners`. Holder amounts above the supply (reads from different
/// moments) are a surprise and stay `Unknown`.
fn burned_or_unknown(
    supply: u64,
    holders: &[(u64, Option<String>)],
    burn_owners: &[&str],
) -> LpStatus {
    let burned: u128 = holders
        .iter()
        .filter(|(_, owner)| owner.as_deref().is_some_and(|o| burn_owners.contains(&o)))
        .map(|(amount, _)| u128::from(*amount))
        .sum();
    match u128::from(supply).checked_sub(burned) {
        Some(outside) if outside * 100 <= u128::from(supply) => LpStatus::Burned,
        _ => LpStatus::Unknown,
    }
}

/// Base58 (Bitcoin alphabet) text of a public key.
fn base58(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    // Little-endian base-58 digits of the big-endian number in `bytes`.
    let mut digits: Vec<u8> = Vec::with_capacity(44);
    for &byte in bytes {
        let mut carry = u32::from(byte);
        for digit in &mut digits {
            carry += u32::from(*digit) << 8;
            *digit = (carry % 58) as u8;
            carry /= 58;
        }
        while carry > 0 {
            digits.push((carry % 58) as u8);
            carry /= 58;
        }
    }
    let zeros = bytes.iter().take_while(|b| **b == 0).count();
    std::iter::repeat_n(b'1', zeros)
        .chain(digits.iter().rev().map(|d| ALPHABET[usize::from(*d)]))
        .map(char::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::RpcClient;
    use crate::testutil::{ok_json, serve_with};
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use serde_json::{json, Value};

    // Verbatim `getAccountInfo` replies from mainnet, recorded 2026-11-08
    // (slots 466969107..466969138). The public RPC refuses
    // `getTokenLargestAccounts`, so holder lists are built in the tests from
    // the recorded token accounts.
    const PUMP_POOL: &str = include_str!("../fixtures/pools/pumpswap_pool.json");
    const PUMP_LP_MINT: &str = include_str!("../fixtures/pools/pumpswap_lp_mint.json");
    const PUMP_BASE_VAULT: &str = include_str!("../fixtures/pools/pumpswap_base_vault.json");
    const V4_POOL: &str = include_str!("../fixtures/pools/raydium_amm_v4_pool.json");
    const V4_LP_MINT: &str = include_str!("../fixtures/pools/raydium_amm_v4_lp_mint.json");
    const CPMM_POOL: &str = include_str!("../fixtures/pools/raydium_cpmm_pool.json");
    const CPMM_LP_MINT: &str = include_str!("../fixtures/pools/raydium_cpmm_lp_mint.json");
    const LOCKED_POOL: &str = include_str!("../fixtures/pools/raydium_cpmm_locked_pool.json");
    const LOCKED_LP_MINT: &str = include_str!("../fixtures/pools/raydium_cpmm_locked_lp_mint.json");
    const LOCKED_LP_HOLDER: &str =
        include_str!("../fixtures/pools/raydium_cpmm_locked_lp_holder.json");
    const CLMM_POOL: &str = include_str!("../fixtures/pools/raydium_clmm_pool.json");

    const WSOL: &str = "So11111111111111111111111111111111111111112";
    const PUMP: (&str, &str) = (
        "1BwD1LXEkFfBbHKSPiGGGXioch6cGLNLRsDhey8f1zJ",
        "BfQV5EJTiwgq1ApuVXig3mfLpBsMBQBwz4m8LEzNpump",
    );
    const V4: (&str, &str) = (
        "FbRgvpenqz3BsJZ8CEFuVQ28ADSMgRiSWH1Y8aFrNjBn",
        "9Pf9XycyK9uf2oRcEKWbztKmRJRLo5iA2g5Q98xipump",
    );
    const CPMM: (&str, &str) = (
        "GNjtt7ZhQayuFWRwqhsCjMEVTninHXGWuSShKv8qixM4",
        "9cRCn9rGT8V2imeM2BaKs13yhMEais3ruM3rPvTGpump",
    );
    const LOCKED: (&str, &str) = (
        "AZjrDh8XLf5CnVkUiJipEsvbxxudnVBpHgcz3ddhRTy9",
        "VjKCt9XXKQ5zjR2xf8hNyXc6VRJHMqrGaK7p7NtPLUS",
    );
    const CLMM: &str = "4pCZCVEiYyT4efNdXUdL2tJF8VGMgiMXrZWq6FiNXhRw";

    fn raw(reply: &str) -> RawAccount {
        let reply: Value = serde_json::from_str(reply).unwrap();
        let value = &reply["result"]["value"];
        RawAccount {
            owner: value["owner"].as_str().unwrap().to_owned(),
            data: STANDARD.decode(value["data"][0].as_str().unwrap()).unwrap(),
        }
    }

    /// `reply` with its owner and data replaced.
    fn edited(reply: &str, owner: &str, data: &[u8]) -> String {
        let mut reply: Value = serde_json::from_str(reply).unwrap();
        reply["result"]["value"]["owner"] = json!(owner);
        reply["result"]["value"]["data"][0] = json!(STANDARD.encode(data));
        reply.to_string()
    }

    fn pair(pool: &str, base: &str, quote: &str) -> Pair {
        serde_json::from_value(json!({
            "chainId": "solana",
            "dexId": "test",
            "pairAddress": pool,
            "baseToken": { "address": base },
            "quoteToken": { "address": quote },
        }))
        .unwrap()
    }

    /// A provider answering `getAccountInfo` from `accounts` (address, reply)
    /// and `getTokenLargestAccounts` with `largest` (address, amount).
    async fn provider(index: u8, accounts: &[(&str, &str)], largest: &[(&str, u64)]) -> RpcClient {
        let accounts: Vec<(String, String)> = accounts
            .iter()
            .map(|(a, r)| ((*a).to_owned(), (*r).to_owned()))
            .collect();
        let largest = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "context": { "slot": 1 },
                "value": largest
                    .iter()
                    .map(|(a, n)| json!({ "address": a, "amount": n.to_string() }))
                    .collect::<Vec<_>>(),
            },
        })
        .to_string();
        let (base, _) = serve_with(move |request| {
            let request = String::from_utf8_lossy(request);
            if request.contains("getTokenLargestAccounts") {
                return ok_json(&largest);
            }
            let reply = accounts
                .iter()
                .find(|(address, _)| request.contains(address.as_str()))
                .map_or(
                    r#"{"jsonrpc":"2.0","id":1,"result":{"context":{"slot":1},"value":null}}"#,
                    |(_, reply)| reply.as_str(),
                );
            ok_json(reply)
        })
        .await;
        RpcClient::plain(index, &base)
    }

    async fn rpc(accounts: &[(&str, &str)], largest: &[(&str, u64)]) -> CrossCheckedRpc {
        CrossCheckedRpc::from_clients(
            provider(0, accounts, largest).await,
            provider(1, accounts, largest).await,
        )
    }

    fn pool(address: &str, reply: &str) -> Pool {
        decode(address, &raw(reply)).unwrap()
    }

    #[test]
    fn decodes_recorded_pools_of_each_program() {
        let p = pool(PUMP.0, PUMP_POOL);
        assert_eq!(p.mints, [PUMP.1.to_owned(), WSOL.to_owned()]);
        assert_eq!(p.lp_mint, "BSWSrAndCudBNqAfjsbmz7zzxbpmugvvM1NF4VhbevvK");
        // The base vault is a token account of the base mint owned by the pool.
        assert_eq!(p.vaults[0], "9XDhGDBFcYgvtVoEz36JF3LR2kNaJJqBpN8mdrBsAtT6");
        let vault = raw(PUMP_BASE_VAULT);
        assert_eq!(
            token_account_owner(&vault, PUMP.1, 99_027_066_920_642).as_deref(),
            Some(PUMP.0)
        );
        // PumpSwap LP mints are Token-2022 mints.
        assert_eq!(
            TokenProgram::from_owner(&raw(PUMP_LP_MINT).owner),
            Some(TokenProgram::Token2022)
        );

        // AMM v4 keeps WSOL as the coin side; the pair names the token first.
        let p = pool(V4.0, V4_POOL);
        assert_eq!(p.mints, [WSOL.to_owned(), V4.1.to_owned()]);
        assert_eq!(p.lp_mint, "A11oeQNT9rxHtf28q9LveDaZzuT8R3tQ3bECme7n7gjf");
        assert_eq!(
            p.vaults,
            [
                "8ZJd2DFbzwwHe3hUm4SfUKzqwjrkatit7ihweyBg3TUF".to_owned(),
                "EAFUEb3bZB3zjLG2SWh8ttVLNJWnZHSUhv6aj5dunshc".to_owned()
            ]
        );
        assert!(p.holds(V4.1, WSOL));

        let p = pool(CPMM.0, CPMM_POOL);
        assert_eq!(p.mints, [WSOL.to_owned(), CPMM.1.to_owned()]);
        assert_eq!(p.lp_mint, "JB63hxT4TSh288L7XtVDRGFeWd4UUqzCPZjQmkgik1Yw");
        assert_eq!(
            p.vaults,
            [
                "5DvvdQWnzE8AZAEoig4VGTUoU2iteB11c7Y9khs2gHUZ".to_owned(),
                "4mQMih5vCDcxcx2PbGHcGbTPHow5RVQ6dD6oJ98uxvGD".to_owned()
            ]
        );
    }

    #[test]
    fn other_owners_and_layout_surprises_do_not_decode() {
        // CLMM is a different program: never decoded.
        assert_eq!(decode(CLMM, &raw(CLMM_POOL)), None);
        let pump = raw(PUMP_POOL);
        for owner in [
            RAYDIUM_CPMM_PROGRAM,
            RAYDIUM_AMM_V4_PROGRAM,
            "11111111111111111111111111111111",
        ] {
            let moved = RawAccount {
                owner: owner.to_owned(),
                ..pump.clone()
            };
            assert_eq!(decode(PUMP.0, &moved), None, "{owner}");
        }
        let short = RawAccount {
            data: pump.data[..210].to_vec(),
            ..pump.clone()
        };
        assert_eq!(decode(PUMP.0, &short), None);
        let mut renamed = pump.clone();
        renamed.data[0] ^= 1;
        assert_eq!(decode(PUMP.0, &renamed), None);
        let v4 = raw(V4_POOL);
        let long = RawAccount {
            data: [v4.data.as_slice(), &[0]].concat(),
            ..v4
        };
        assert_eq!(decode(V4.0, &long), None);
    }

    #[tokio::test]
    async fn burned_pools_of_each_program_read_as_burned() {
        let cases = [
            (PUMP, PUMP_POOL, PUMP_LP_MINT, pair(PUMP.0, PUMP.1, WSOL)),
            (V4, V4_POOL, V4_LP_MINT, pair(V4.0, V4.1, WSOL)),
            (CPMM, CPMM_POOL, CPMM_LP_MINT, pair(CPMM.0, CPMM.1, WSOL)),
        ];
        for ((address, mint), pool_reply, lp_reply, pair) in cases {
            let decoded = pool(address, pool_reply);
            let client = rpc(&[(address, pool_reply), (&decoded.lp_mint, lp_reply)], &[]).await;
            let lp = read(&client, &pair).await.unwrap();
            assert_eq!(lp.mint(), mint);
            assert_eq!(lp.get().status, LpStatus::Burned, "{address}");
            assert_eq!(lp.get().vaults, decoded.vaults.to_vec());
        }
    }

    #[tokio::test]
    async fn unverified_pools_are_unknown_without_vaults() {
        let unknown = PoolLp {
            status: LpStatus::Unknown,
            vaults: Vec::new(),
        };
        let lp_mint = pool(PUMP.0, PUMP_POOL).lp_mint;
        let accounts = [(PUMP.0, PUMP_POOL), (lp_mint.as_str(), PUMP_LP_MINT)];

        // Mints that do not match the pair: another token, or another quote.
        let client = rpc(&accounts, &[]).await;
        for other in [pair(PUMP.0, V4.1, WSOL), pair(PUMP.0, PUMP.1, V4.1)] {
            assert_eq!(read(&client, &other).await.unwrap().get(), &unknown);
        }

        // The same bytes under another owner.
        let pump = raw(PUMP_POOL);
        let moved = edited(PUMP_POOL, RAYDIUM_CPMM_PROGRAM, &pump.data);
        let client = rpc(&[(PUMP.0, &moved)], &[]).await;
        assert_eq!(
            read(&client, &pair(PUMP.0, PUMP.1, WSOL))
                .await
                .unwrap()
                .get(),
            &unknown
        );

        let client = rpc(&[(CLMM, CLMM_POOL)], &[]).await;
        assert_eq!(
            read(&client, &pair(CLMM, PUMP.1, WSOL))
                .await
                .unwrap()
                .get(),
            &unknown
        );
    }

    #[tokio::test]
    async fn lp_held_by_a_locker_is_unknown() {
        // Recorded: all LP of this pool sits with Raydium's LP lock authority,
        // which is not in the burn set.
        let decoded = pool(LOCKED.0, LOCKED_POOL);
        let holder = "9iq9JvsYCnDqU1DmR4rW1LFw4bB9PJCrQKZvQ8CkHzCS";
        let client = rpc(
            &[
                (LOCKED.0, LOCKED_POOL),
                (&decoded.lp_mint, LOCKED_LP_MINT),
                (holder, LOCKED_LP_HOLDER),
            ],
            &[(holder, 4_193_626_126_116)],
        )
        .await;
        let lp = read(&client, &pair(LOCKED.0, LOCKED.1, WSOL))
            .await
            .unwrap();
        assert_eq!(lp.get().status, LpStatus::Unknown);
        // The pool itself is verified, so its vaults still count.
        assert_eq!(lp.get().vaults, decoded.vaults.to_vec());
    }

    #[tokio::test]
    async fn lp_held_by_the_pool_reads_as_burned() {
        // Synthetic: a PumpSwap LP token account owned by the pool, built
        // from the recorded base vault with the LP mint swapped in.
        let pump = raw(PUMP_POOL);
        let lp_mint = pool(PUMP.0, PUMP_POOL).lp_mint;
        let vault = raw(PUMP_BASE_VAULT);
        let mut holder = vault.data.clone();
        holder[..32].copy_from_slice(&pump.data[107..139]);
        let amount = 99_027_066_920_642_u64;
        let mut mint = raw(PUMP_LP_MINT).data;
        mint[36..44].copy_from_slice(&amount.to_le_bytes());
        let mint_reply = edited(PUMP_LP_MINT, crate::token2022::TOKEN_2022_PROGRAM, &mint);
        let holder_reply = edited(PUMP_BASE_VAULT, &vault.owner, &holder);
        let address = "Hzd6mEnXxbKH7ewBvxKbD9Lw3GeNK8f4y3ZSKwSWhXAi";
        let client = rpc(
            &[
                (PUMP.0, PUMP_POOL),
                (&lp_mint, &mint_reply),
                (address, &holder_reply),
            ],
            &[(address, amount)],
        )
        .await;
        let lp = read(&client, &pair(PUMP.0, PUMP.1, WSOL)).await.unwrap();
        assert_eq!(lp.get().status, LpStatus::Burned);

        // The same holder reporting another amount than the list is a surprise.
        let client = rpc(
            &[
                (PUMP.0, PUMP_POOL),
                (&lp_mint, &mint_reply),
                (address, &holder_reply),
            ],
            &[(address, amount - 1)],
        )
        .await;
        let lp = read(&client, &pair(PUMP.0, PUMP.1, WSOL)).await.unwrap();
        assert_eq!(lp.get().status, LpStatus::Unknown);
    }

    #[tokio::test]
    async fn provider_disagreement_is_an_error() {
        let decoded = pool(CPMM.0, CPMM_POOL);
        let mut data = raw(CPMM_POOL).data;
        data[136] ^= 1; // another LP mint
        let forged = edited(CPMM_POOL, RAYDIUM_CPMM_PROGRAM, &data);
        let honest = [
            (CPMM.0, CPMM_POOL),
            (decoded.lp_mint.as_str(), CPMM_LP_MINT),
        ];
        let client = CrossCheckedRpc::from_clients(
            provider(0, &honest, &[]).await,
            provider(1, &[(CPMM.0, &forged)], &[]).await,
        );
        assert!(matches!(
            read(&client, &pair(CPMM.0, CPMM.1, WSOL)).await,
            Err(MarketDataError::ProviderDisagreement("pool account"))
        ));
    }

    #[test]
    fn burned_needs_at_most_one_percent_outside_burn_owners() {
        let burn = [INCINERATOR, "Pool111111111111111111111111111111111111111"];
        let held = |owner: &str, amount: u64| (amount, Some(owner.to_owned()));
        let wallet = "Wa11et1111111111111111111111111111111111111";
        assert_eq!(burned_or_unknown(0, &[], &burn), LpStatus::Burned);
        assert_eq!(burned_or_unknown(1_000, &[], &burn), LpStatus::Unknown);
        assert_eq!(
            burned_or_unknown(1_000, &[held(INCINERATOR, 600), held(burn[1], 390)], &burn),
            LpStatus::Burned
        );
        assert_eq!(
            burned_or_unknown(1_000, &[held(INCINERATOR, 989), held(wallet, 11)], &burn),
            LpStatus::Unknown
        );
        // An unreadable holder counts as outside.
        assert_eq!(
            burned_or_unknown(1_000, &[(1_000, None)], &burn),
            LpStatus::Unknown
        );
        // Holders above the supply: reads from different moments.
        assert_eq!(
            burned_or_unknown(1_000, &[held(INCINERATOR, 1_001)], &burn),
            LpStatus::Unknown
        );
        assert_eq!(
            burned_or_unknown(u64::MAX, &[held(INCINERATOR, u64::MAX)], &burn),
            LpStatus::Burned
        );
    }

    #[test]
    fn only_plain_token_accounts_of_the_lp_mint_count() {
        let lp_mint = pool(LOCKED.0, LOCKED_POOL).lp_mint;
        let amount = 4_193_626_126_116;
        let holder = raw(LOCKED_LP_HOLDER);
        let lock_authority = "3f7GcQFG397GAaEnv51zR6tsTVihYRydnydDD1cXekxH";
        assert_eq!(
            token_account_owner(&holder, &lp_mint, amount).as_deref(),
            Some(lock_authority)
        );
        assert_eq!(token_account_owner(&holder, CPMM.1, amount), None);
        assert_eq!(token_account_owner(&holder, &lp_mint, amount + 1), None);
        let mut delegated = holder.clone();
        delegated.data[72] = 1;
        assert_eq!(token_account_owner(&delegated, &lp_mint, amount), None);
        let foreign = RawAccount {
            owner: RAYDIUM_CPMM_PROGRAM.to_owned(),
            ..holder
        };
        assert_eq!(token_account_owner(&foreign, &lp_mint, amount), None);
    }

    #[test]
    fn base58_matches_known_keys() {
        assert_eq!(base58(&[0; 32]), "11111111111111111111111111111111");
        let vault = raw(PUMP_BASE_VAULT);
        assert_eq!(base58(&vault.data[..32]), PUMP.1);
    }
}
