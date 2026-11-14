//! Read-only crypto wallets: the asset allowlist and public-address
//! validation.
//!
//! Only public addresses are accepted. Anything that looks like key material
//! (a recovery phrase, a private key, an extended private key) is refused
//! *before* any other check, with a fixed message: the input is never stored,
//! logged or echoed, and no error here carries it.

use sha2::{Digest, Sha256};
use tiny_keccak::{Hasher, Keccak};
use zeroize::Zeroizing;

/// What kind of address a wallet holds. One EVM address covers every EVM chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalletKind {
    Bitcoin,
    Evm,
    Solana,
}

/// The closed set of supported chains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chain {
    Bitcoin,
    Ethereum,
    Base,
    Arbitrum,
    Optimism,
    Polygon,
    Solana,
}

/// An asset Jarvis counts, whatever chain it is on.
#[derive(Debug)]
pub struct Asset {
    /// Stable lower-case key, also the database key.
    pub key: &'static str,
    /// Display symbol; the only name ever shown (never a chain-provided one).
    pub symbol: &'static str,
    /// Other words for the asset, lower case (privacy gate).
    pub aliases: &'static [&'static str],
    /// CoinGecko API id for the EUR price.
    pub coingecko_id: &'static str,
}

pub const ASSETS: &[Asset] = &[
    asset("btc", "BTC", &["bitcoin"], "bitcoin"),
    asset("eth", "ETH", &["ether", "ethereum"], "ethereum"),
    asset(
        "pol",
        "POL",
        &["polygon", "matic"],
        "polygon-ecosystem-token",
    ),
    asset("sol", "SOL", &["solana"], "solana"),
    asset("usdc", "USDC", &["usd coin"], "usd-coin"),
    asset("usdt", "USDT", &["tether"], "tether"),
    asset("eurc", "EURC", &["euro coin"], "euro-coin"),
    asset("wbtc", "WBTC", &["wrapped bitcoin"], "wrapped-bitcoin"),
    asset("weth", "WETH", &["wrapped ether"], "weth"),
    asset("dai", "DAI", &[], "dai"),
];

const fn asset(
    key: &'static str,
    symbol: &'static str,
    aliases: &'static [&'static str],
    coingecko_id: &'static str,
) -> Asset {
    Asset {
        key,
        symbol,
        aliases,
        coingecko_id,
    }
}

/// One allowlisted asset on one chain. Only these are ever counted.
#[derive(Debug)]
pub struct Token {
    pub chain: Chain,
    /// [`Asset::key`].
    pub asset: &'static str,
    /// ERC-20 contract (EIP-55) or SPL mint; `None` is the chain's native coin.
    pub contract: Option<&'static str>,
    pub decimals: u32,
}

/// Natives; USDC, USDT and EURC only where the issuer issues them natively;
/// WETH, WBTC and DAI as the chain's canonical (bridged) token. Left out: USDT
/// on L2s (USDT0 or PoS-bridged, not Tether-issued) and DAI on Base
/// (provenance unconfirmed).
#[rustfmt::skip]
pub const TOKENS: &[Token] = &[
    native(Chain::Bitcoin, "btc", 8),
    native(Chain::Ethereum, "eth", 18),
    native(Chain::Base, "eth", 18),
    native(Chain::Arbitrum, "eth", 18),
    native(Chain::Optimism, "eth", 18),
    native(Chain::Polygon, "pol", 18),
    native(Chain::Solana, "sol", 9),
    // Ethereum.
    token(Chain::Ethereum, "usdc", "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", 6),
    token(Chain::Ethereum, "usdt", "0xdAC17F958D2ee523a2206206994597C13D831ec7", 6),
    token(Chain::Ethereum, "eurc", "0x1aBaEA1f7C830bD89Acc67eC4af516284b1bC33c", 6),
    token(Chain::Ethereum, "wbtc", "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599", 8),
    token(Chain::Ethereum, "weth", "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2", 18),
    token(Chain::Ethereum, "dai", "0x6B175474E89094C44Da98b954EedeAC495271d0F", 18),
    // Base.
    token(Chain::Base, "usdc", "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913", 6),
    token(Chain::Base, "eurc", "0x60a3E35Cc302bFA44Cb288Bc5a4F316Fdb1adb42", 6),
    token(Chain::Base, "weth", "0x4200000000000000000000000000000000000006", 18),
    // Arbitrum One.
    token(Chain::Arbitrum, "usdc", "0xaf88d065e77c8cC2239327C5EDb3A432268e5831", 6),
    token(Chain::Arbitrum, "wbtc", "0x2f2a2543B76A4166549F7aaB2e75Bef0aefC5B0f", 8),
    token(Chain::Arbitrum, "weth", "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1", 18),
    token(Chain::Arbitrum, "dai", "0xDA10009cBd5D07dd0CeCc66161FC93D7c9000da1", 18),
    // OP Mainnet.
    token(Chain::Optimism, "usdc", "0x0b2C639c533813f4Aa9D7837CAf62653d097Ff85", 6),
    token(Chain::Optimism, "wbtc", "0x68f180fcCe6836688e9084f035309E29Bf0A2095", 8),
    token(Chain::Optimism, "weth", "0x4200000000000000000000000000000000000006", 18),
    token(Chain::Optimism, "dai", "0xDA10009cBd5D07dd0CeCc66161FC93D7c9000da1", 18),
    // Polygon PoS.
    token(Chain::Polygon, "usdc", "0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359", 6),
    token(Chain::Polygon, "wbtc", "0x1BFD67037B42Cf73acF2047067bd4F2C47D9BfD6", 8),
    token(Chain::Polygon, "weth", "0x7ceB23fD6bC0adD59E62ac25578270cFf1b9f619", 18),
    token(Chain::Polygon, "dai", "0x8f3Cf7ad23Cd3CaDbD9735AFf958023239c6A063", 18),
    // Solana.
    token(Chain::Solana, "usdc", "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", 6),
    token(Chain::Solana, "usdt", "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB", 6),
    token(Chain::Solana, "eurc", "HzwqbKZw8HxMN6bF2yFZNrht3c2iXXzpKcFu7uBEDKtr", 6),
];

const fn native(chain: Chain, asset: &'static str, decimals: u32) -> Token {
    Token {
        chain,
        asset,
        contract: None,
        decimals,
    }
}

const fn token(chain: Chain, asset: &'static str, contract: &'static str, decimals: u32) -> Token {
    Token {
        chain,
        asset,
        contract: Some(contract),
        decimals,
    }
}

/// Why an address was refused. Every message is fixed and carries no input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AddressError {
    #[error(
        "this looks like a private key or recovery phrase; it was not stored. \
         Never enter one: add a public address only"
    )]
    KeyMaterial,
    #[error("extended public keys (xpub) are not supported; add single addresses")]
    ExtendedPublicKey,
    #[error("only mainnet addresses are supported")]
    Testnet,
    #[error("not a valid address for this wallet kind")]
    Invalid,
}

/// A BIP-39 phrase has at least this many words.
const MIN_MNEMONIC_WORDS: usize = 12;
const PRIVATE_KEY_PREFIXES: [&str; 8] = [
    "xprv", "yprv", "zprv", "tprv", "uprv", "vprv", "Yprv", "Zprv",
];
const PUBLIC_KEY_PREFIXES: [&str; 8] = [
    "xpub", "ypub", "zpub", "tpub", "upub", "vpub", "Ypub", "Zpub",
];

/// Check `input` as a public address of `kind` and return its stored form:
/// Bitcoin bech32 lower case, base58 as given; EVM `0x` + 40 lower-case hex
/// (see [`evm_checksum`] for display); Solana as given. Mainnet only.
pub fn parse_address(kind: WalletKind, input: &str) -> Result<String, AddressError> {
    if is_key_material(input) {
        return Err(AddressError::KeyMaterial);
    }
    let address = input.trim();
    match kind {
        WalletKind::Bitcoin => bitcoin(address),
        WalletKind::Evm => evm(address),
        WalletKind::Solana => solana(address),
    }
}

/// Whether `input` holds anything that looks like key material: a phrase of
/// 12+ words (split by spaces, commas, semicolons or dashes), a PEM block, a
/// JSON byte array (Solana keypair file), or a token that is an extended
/// private key, 64 or 128 hex digits, a WIF key
/// (base58, version 0x80/0xef; checksum not needed, a mistyped key is still
/// a key) or base58 decoding to 64 bytes (Solana secret key).
pub fn is_key_material(input: &str) -> bool {
    input.contains("-----BEGIN")
        || input
            .split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '-'))
            .filter(|word| !word.is_empty())
            .count()
            >= MIN_MNEMONIC_WORDS
        || input
            .trim_start()
            .strip_prefix('[')
            .is_some_and(|rest| rest.trim_start().starts_with(|c: char| c.is_ascii_digit()))
        || input
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(is_key_token)
}

fn is_key_token(token: &str) -> bool {
    let hex = token
        .strip_prefix("0x")
        .or_else(|| token.strip_prefix("0X"))
        .unwrap_or(token);
    // A Solana address may start with a prefix by chance (or by vanity).
    (PRIVATE_KEY_PREFIXES
        .iter()
        .any(|prefix| token.starts_with(prefix))
        && solana(token).is_err())
        || (matches!(hex.len(), 64 | 128) && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        || (matches!(token.len(), 51 | 52)
            && base58(token).is_some_and(|bytes| {
                matches!(bytes.len(), 37 | 38) && matches!(bytes[0], 0x80 | 0xef)
            }))
        || ((64..=88).contains(&token.len()) && base58(token).is_some_and(|b| b.len() == 64))
}

/// Decoded bytes are wiped on drop: they may be a private key.
fn base58(text: &str) -> Option<Zeroizing<Vec<u8>>> {
    bs58::decode(text).into_vec().ok().map(Zeroizing::new)
}

fn bitcoin(address: &str) -> Result<String, AddressError> {
    if PUBLIC_KEY_PREFIXES
        .iter()
        .any(|prefix| address.starts_with(prefix))
    {
        return Err(AddressError::ExtendedPublicKey);
    }
    let lower = address.to_ascii_lowercase();
    if ["bc1", "tb1", "bcrt1"]
        .iter()
        .any(|hrp| lower.starts_with(hrp))
    {
        segwit(address)
    } else {
        base58check(address)
    }
}

/// Native segwit, mainnet: v0 (P2WPKH 20 bytes, P2WSH 32 bytes, bech32) or
/// v1 (taproot 32 bytes, bech32m). Other versions are valid per BIP-350 but
/// not used for holding funds today, so refused.
fn segwit(address: &str) -> Result<String, AddressError> {
    let (hrp, data, variant) = bech32_decode(address).ok_or(AddressError::Invalid)?;
    let (&version, program) = data.split_first().ok_or(AddressError::Invalid)?;
    let program = five_to_eight_bits(program).ok_or(AddressError::Invalid)?;
    let valid = match (version, variant) {
        (0, Variant::Bech32) => matches!(program.len(), 20 | 32),
        (1, Variant::Bech32m) => program.len() == 32,
        _ => false,
    };
    match hrp.as_str() {
        "bc" if valid => Ok(address.to_ascii_lowercase()),
        "tb" | "bcrt" if valid => Err(AddressError::Testnet),
        _ => Err(AddressError::Invalid),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Variant {
    Bech32,
    Bech32m,
}

const BECH32_CHARSET: &[u8; 32] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";

/// BIP-173/BIP-350 decoding: the lower-case HRP, the 5-bit data without the
/// checksum, and which checksum matched. Mixed case is refused.
fn bech32_decode(text: &str) -> Option<(String, Vec<u8>, Variant)> {
    if text.len() > 90 || !text.bytes().all(|b| (33..=126).contains(&b)) {
        return None;
    }
    if text.bytes().any(|b| b.is_ascii_lowercase()) && text.bytes().any(|b| b.is_ascii_uppercase())
    {
        return None;
    }
    let text = text.to_ascii_lowercase();
    let (hrp, data) = text.rsplit_once('1')?;
    if hrp.is_empty() || data.len() < 6 {
        return None;
    }
    let mut data = data
        .bytes()
        .map(|b| BECH32_CHARSET.iter().position(|&c| c == b).map(|p| p as u8))
        .collect::<Option<Vec<u8>>>()?;
    let variant = match bech32_polymod(hrp, &data) {
        1 => Variant::Bech32,
        0x2bc8_30a3 => Variant::Bech32m,
        _ => return None,
    };
    data.truncate(data.len() - 6);
    Some((hrp.to_owned(), data, variant))
}

fn bech32_polymod(hrp: &str, data: &[u8]) -> u32 {
    const GENERATOR: [u32; 5] = [
        0x3b6a_57b2,
        0x2650_8e6d,
        0x1ea1_19fa,
        0x3d42_33dd,
        0x2a14_62b3,
    ];
    let values = hrp
        .bytes()
        .map(|b| b >> 5)
        .chain([0])
        .chain(hrp.bytes().map(|b| b & 31))
        .chain(data.iter().copied());
    let mut checksum = 1u32;
    for value in values {
        let top = checksum >> 25;
        checksum = ((checksum & 0x1ff_ffff) << 5) ^ u32::from(value);
        for (bit, generator) in GENERATOR.iter().enumerate() {
            if (top >> bit) & 1 == 1 {
                checksum ^= generator;
            }
        }
    }
    checksum
}

/// 5-bit groups to bytes without padding: at most 4 leftover bits, all zero.
fn five_to_eight_bits(data: &[u8]) -> Option<Vec<u8>> {
    let (mut acc, mut bits, mut out) = (0u32, 0u32, Vec::new());
    for &value in data {
        acc = ((acc << 5) | u32::from(value)) & 0xfff;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    (bits < 5 && acc & ((1 << bits) - 1) == 0).then_some(out)
}

/// Legacy P2PKH (version 0x00) or P2SH (0x05) with a valid double-SHA-256
/// checksum.
fn base58check(address: &str) -> Result<String, AddressError> {
    if !(26..=35).contains(&address.len()) {
        return Err(AddressError::Invalid);
    }
    let bytes = bs58::decode(address)
        .into_vec()
        .map_err(|_| AddressError::Invalid)?;
    if bytes.len() != 25 || Sha256::digest(Sha256::digest(&bytes[..21]))[..4] != bytes[21..] {
        return Err(AddressError::Invalid);
    }
    match bytes[0] {
        0x00 | 0x05 => Ok(address.to_owned()),
        0x6f | 0xc4 => Err(AddressError::Testnet),
        _ => Err(AddressError::Invalid),
    }
}

/// `0x` + 40 hex digits. All lower or all upper case is accepted as is;
/// mixed case must be the EIP-55 checksum.
fn evm(address: &str) -> Result<String, AddressError> {
    let hex = address.strip_prefix("0x").ok_or(AddressError::Invalid)?;
    if hex.len() != 40 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(AddressError::Invalid);
    }
    let lower = format!("0x{}", hex.to_ascii_lowercase());
    let mixed =
        hex.bytes().any(|b| b.is_ascii_lowercase()) && hex.bytes().any(|b| b.is_ascii_uppercase());
    if mixed && evm_checksum(&lower) != address {
        return Err(AddressError::Invalid);
    }
    Ok(lower)
}

/// EIP-55 display form of an address in the stored form (`0x` + 40 lower-case
/// hex digits).
pub fn evm_checksum(address: &str) -> String {
    let hex = address.strip_prefix("0x").unwrap_or(address);
    let hex = hex.to_ascii_lowercase();
    let mut hash = [0u8; 32];
    let mut keccak = Keccak::v256();
    keccak.update(hex.as_bytes());
    keccak.finalize(&mut hash);
    let nibbles = hash.iter().flat_map(|byte| [byte >> 4, byte & 0xf]);
    let checksummed: String = hex
        .chars()
        .zip(nibbles)
        .map(|(c, nibble)| {
            if nibble >= 8 {
                c.to_ascii_uppercase()
            } else {
                c
            }
        })
        .collect();
    format!("0x{checksummed}")
}

/// Base58 decoding to exactly 32 bytes (on or off the curve; program-derived
/// addresses can hold tokens too). A 32-byte Ed25519 seed in base58 looks the
/// same and cannot be told apart.
fn solana(address: &str) -> Result<String, AddressError> {
    let decodes_to_32 = || base58(address).is_some_and(|bytes| bytes.len() == 32);
    if (32..=44).contains(&address.len()) && decodes_to_32() {
        Ok(address.to_owned())
    } else {
        Err(AddressError::Invalid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P2PKH: &str = "1BvBMSEYstWetqTFn5Au4m4GFg7xJaNVN2";
    const P2SH: &str = "3J98t1WpEZ73CNmQviecrnyiWrnqRhWNLy";
    // Random throwaway test data, never funded or used: a compressed and an
    // uncompressed mainnet WIF, a testnet WIF and a 64-byte Solana secret.
    const WIF_COMPRESSED: &str = "L2fo4Kvj1k79zBLw3RTRacgsVgWPLNQMQbGNNc8LnsLnDt662SF7";
    const WIF_UNCOMPRESSED: &str = "5KFbZ6ALEBaeHdRc4dz6rir2AELThVnmE8oMFvUF7SpT8npAgbu";
    const WIF_TESTNET: &str = "cRJuYTqc3oV92biruZreVwveD4D6oGZvBRu7TCJ2DWbZBWVtkMPi";
    const SOLANA_SECRET: &str =
        "Ri71QgUgULUbKeM5426JRND3BB68jDLaVn2bdxwTxWse67F9HvGgEGxR7Gp4m5vE6eDnBd2QiX72cSQvTffKwuv";

    fn key_material() -> Vec<String> {
        let mnemonic12 = format!("{}about", "abandon ".repeat(11));
        let mnemonic24 = format!("{}art", "abandon ".repeat(23));
        let hex64 = "4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318";
        let solana_array = format!("[{}]", vec!["174"; 64].join(","));
        vec![
            mnemonic12.clone(),
            mnemonic24,
            mnemonic12.replace(' ', "\n"),
            mnemonic12.replace(' ', ","),
            mnemonic12.replace(' ', "-"),
            WIF_COMPRESSED.to_owned(),
            WIF_UNCOMPRESSED.to_owned(),
            WIF_TESTNET.to_owned(),
            format!("\"{WIF_COMPRESSED}\""),
            "xprv9s21ZrQH143K3QTDL4LXw2F7HEK3wJUD2nW2nRk4stbPy6cq3jPPqjiChkVvvNKmPGJxWUtg6LnF5kejMRNNU3TGtRBeJgk33yuGBxrMPHi".to_owned(),
            "zprvAWgYBBk7JR8Gj".to_owned(),
            "tprv8ZgxMBicQKsPd".to_owned(),
            hex64.to_owned(),
            format!("0x{hex64}"),
            hex64.to_uppercase(),
            hex64.repeat(2),
            SOLANA_SECRET.to_owned(),
            solana_array.clone(),
            format!("  {}", solana_array.replace(',', ", ")),
            "-----BEGIN EC PRIVATE KEY-----\nMHcCAQEE\n-----END EC PRIVATE KEY-----".to_owned(),
        ]
    }

    #[test]
    fn key_material_is_refused_first_without_echo() {
        for input in key_material() {
            for kind in [WalletKind::Bitcoin, WalletKind::Evm, WalletKind::Solana] {
                let err = parse_address(kind, &input).unwrap_err();
                assert_eq!(err, AddressError::KeyMaterial, "{kind:?} {input}");
                for shown in [err.to_string(), format!("{err:?}")] {
                    for word in input.split(|c: char| !c.is_ascii_alphanumeric()) {
                        assert!(
                            word.len() < 6 || !shown.contains(word),
                            "error echoes input: {shown}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn public_addresses_are_not_key_material() {
        for address in [
            P2PKH,
            P2SH,
            "bc1p0xlxvlhemja6c4dqv22uapctqupfhlxm9h8z3k2e72q4k9hcz7vqzk5jj0",
            "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed",
            "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            "So11111111111111111111111111111111111111112",
            "xpub661MyMwAqRbcFtXgS5sYJABqqG9YLmC4Q1Rdap9gSE8NqtwybGhePY2gZ29ESFjqJoCu1Rupje8YtGqsefD265TMg7usUDFdp6W1EGMcet8",
            "eleven words are not a phrase so this text passes fine",
        ] {
            assert!(!is_key_material(address), "{address}");
        }
    }

    #[test]
    fn extended_public_keys_are_unsupported() {
        for xpub in [
            "xpub661MyMwAqRbcFtXgS5sYJABqqG9YLmC4Q1Rdap9gSE8NqtwybGhePY2gZ29ESFjqJoCu1Rupje8YtGqsefD265TMg7usUDFdp6W1EGMcet8",
            "zpub6rFR7y4Q2AijBEqTUquhVz398htDFrtymD9xYYfG1m4wAcvPhXNfE3EfH1r1ADqtfSdVCToUG868RvUUkgDKf31mGDtKsAYz2oz2AGutZYs",
            " tpubD6NzVbkrYhZ4X",
        ] {
            assert_eq!(
                parse_address(WalletKind::Bitcoin, xpub),
                Err(AddressError::ExtendedPublicKey)
            );
            for kind in [WalletKind::Evm, WalletKind::Solana] {
                assert_eq!(parse_address(kind, xpub), Err(AddressError::Invalid));
            }
        }
    }

    #[test]
    fn bech32_spec_vectors() {
        // BIP-173 and BIP-350 valid strings, checksum level.
        for (text, variant) in [
            ("A12UEL5L", Variant::Bech32),
            ("a12uel5l", Variant::Bech32),
            ("an83characterlonghumanreadablepartthatcontainsthenumber1andtheexcludedcharactersbio1tt5tgs", Variant::Bech32),
            ("abcdef1qpzry9x8gf2tvdw0s3jn54khce6mua7lmqqqxw", Variant::Bech32),
            ("11qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqc8247j", Variant::Bech32),
            ("split1checkupstagehandshakeupstreamerranterredcaperred2y9e3w", Variant::Bech32),
            ("?1ezyfcl", Variant::Bech32),
            ("A1LQFN3A", Variant::Bech32m),
            ("a1lqfn3a", Variant::Bech32m),
            ("an83characterlonghumanreadablepartthatcontainsthetheexcludedcharactersbioandnumber11sg7hg6", Variant::Bech32m),
            ("abcdef1l7aum6echk45nj3s0wdvt2fg8x9yrzpqzd3ryx", Variant::Bech32m),
            ("11llllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllludsr8", Variant::Bech32m),
            ("split1checkupstagehandshakeupstreamerranterredcaperredlc445v", Variant::Bech32m),
            ("?1v759aa", Variant::Bech32m),
        ] {
            assert_eq!(bech32_decode(text).map(|(_, _, v)| v), Some(variant), "{text}");
        }
        for text in [
            "\u{20}1nwldj5",
            "\u{7f}1axkwrx",
            "\u{80}1eym55h",
            "an84characterslonghumanreadablepartthatcontainsthenumber1andtheexcludedcharactersbio1569pvx",
            "pzry9x0s0muk",
            "1pzry9x0s0muk",
            "x1b4n0q5v",
            "li1dgmt3",
            "de1lg7wt\u{ff}",
            "A1G7SGD8",
            "10a06t8",
            "1qzzfhee",
            "\u{20}1xj0phk",
            "\u{7f}1g6xzxy",
            "\u{80}1vctc34",
            "an84characterslonghumanreadablepartthatcontainsthetheexcludedcharactersbioandnumber11d6pts4",
            "qyrz8wqd2c9m",
            "1qyrz8wqd2c9m",
            "y1b0jsk6g",
            "lt1igcx5c0",
            "in1muywd",
            "mm1crxm3i",
            "au1s5cgom",
            "M1VUXWEZ",
            "16plkw9",
            "1p2gdwpf",
        ] {
            assert_eq!(bech32_decode(text), None, "{text:?}");
        }
    }

    #[test]
    fn bitcoin_addresses() {
        let btc = |text: &str| parse_address(WalletKind::Bitcoin, text);
        for (text, stored) in [
            (
                "BC1QW508D6QEJXTDG4Y5R3ZARVARY0C5XW7KV8F3T4",
                "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4",
            ),
            (
                "bc1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3qccfmv3",
                "bc1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3qccfmv3",
            ),
            (
                "bc1p0xlxvlhemja6c4dqv22uapctqupfhlxm9h8z3k2e72q4k9hcz7vqzk5jj0",
                "bc1p0xlxvlhemja6c4dqv22uapctqupfhlxm9h8z3k2e72q4k9hcz7vqzk5jj0",
            ),
            (P2PKH, P2PKH),
            (P2SH, P2SH),
            (
                " 1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa\n",
                "1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa",
            ),
        ] {
            assert_eq!(btc(text).as_deref(), Ok(stored), "{text}");
        }
        for text in [
            "tb1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3q0sl5k7",
            "tb1pqqqqp399et2xygdj5xreqhjjvcmzhxw4aywxecjdzew6hylgvsesf3hn0c",
            "mrS8eVKXguwufwvsVe9GtgGb7fif9UQeAu",
            "2N9hLwkSqr1cPQAPxbrGVUjxyjD11G2e1he",
        ] {
            assert_eq!(btc(text), Err(AddressError::Testnet), "{text}");
        }
        for text in [
            // BIP-173 and BIP-350 invalid segwit addresses.
            "tc1qw508d6qejxtdg4y5r3zarvary0c5xw7kg3g4ty",
            "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t5",
            "BC13W508D6QEJXTDG4Y5R3ZARVARY0C5XW7KN40WF2",
            "bc1rw5uspcuh",
            "bc10w508d6qejxtdg4y5r3zarvary0c5xw7kw508d6qejxtdg4y5r3zarvary0c5xw7kw5rljs90",
            "BC1QR508D6QEJXTDG4Y5R3ZARVARYV98GJ9P",
            "tb1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3q0sL5k7",
            "bc1zw508d6qejxtdg4y5r3zarvaryvqyzf3du",
            "tb1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3pjxtptv",
            "bc1gmk9yu",
            "tc1p0xlxvlhemja6c4dqv22uapctqupfhlxm9h8z3k2e72q4k9hcz7vq5zuyut",
            "bc1p0xlxvlhemja6c4dqv22uapctqupfhlxm9h8z3k2e72q4k9hcz7vqh2y7hd",
            "tb1z0xlxvlhemja6c4dqv22uapctqupfhlxm9h8z3k2e72q4k9hcz7vqglt7rf",
            "BC1S0XLXVLHEMJA6C4DQV22UAPCTQUPFHLXM9H8Z3K2E72Q4K9HCZ7VQ54WELL",
            "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kemeawh",
            "tb1q0xlxvlhemja6c4dqv22uapctqupfhlxm9h8z3k2e72q4k9hcz7vq24jc47",
            "bc1p38j9r5y49hruaue7wxjce0updqjuyyx0kh56v8s25huc6995vvpql3jow4",
            "BC130XLXVLHEMJA6C4DQV22UAPCTQUPFHLXM9H8Z3K2E72Q4K9HCZ7VQ7ZWS8R",
            "bc1pw5dgrnzv",
            "bc1p0xlxvlhemja6c4dqv22uapctqupfhlxm9h8z3k2e72q4k9hcz7v8n0nx0muaewav253zgeav",
            "tb1p0xlxvlhemja6c4dqv22uapctqupfhlxm9h8z3k2e72q4k9hcz7vq47Zagq",
            "bc1p0xlxvlhemja6c4dqv22uapctqupfhlxm9h8z3k2e72q4k9hcz7v07qwwzcrf",
            "tb1p0xlxvlhemja6c4dqv22uapctqupfhlxm9h8z3k2e72q4k9hcz7vpggkg4j",
            // BIP-173 era v1+ vectors (bech32 instead of bech32m).
            "bc1pw508d6qejxtdg4y5r3zarvary0c5xw7kw508d6qejxtdg4y5r3zarvary0c5xw7k7grplx",
            "BC1SW50QA3JX3S",
            "bc1zw508d6qejxtdg4y5r3zarvaryvg6kdaj",
            // Valid BIP-350 v1 (40 bytes), v16 and v2 programs: not used for funds.
            "bc1pw508d6qejxtdg4y5r3zarvary0c5xw7kw508d6qejxtdg4y5r3zarvary0c5xw7kt5nd6y",
            "BC1SW50QGDZ25J",
            "bc1zw508d6qejxtdg4y5r3zarvaryvaxxpcs",
            // Base58check: bad checksum, wrong length, bad characters.
            "1BvBMSEYstWetqTFn5Au4m4GFg7xJaNVN3",
            "3J98t1WpEZ73CNmQviecrnyiWrnqRhWNLz",
            "1BvBMSEYstWetqTFn5Au4m4GFg7xJaNVN",
            "1BvBMSEYstWetqTFn5Au4m4GFg7xJaNVN0",
            "",
            "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed",
            "So11111111111111111111111111111111111111112",
        ] {
            assert_eq!(btc(text), Err(AddressError::Invalid), "{text}");
        }
    }

    #[test]
    fn evm_addresses() {
        let evm = |text: &str| parse_address(WalletKind::Evm, text);
        // EIP-55 spec vectors: all caps, all lower, mixed.
        for address in [
            "0x52908400098527886E0F7030069857D2E4169EE7",
            "0x8617E340B3D01FA5F11F306F4090FD50E238070D",
            "0xde709f2102306220921060314715629080e2fb77",
            "0x27b1fdb04752bbc536007a920d24acb045561c26",
            "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed",
            "0xfB6916095ca1df60bB79Ce92cE3Ea74c37c5d359",
            "0xdbF03B407c01E7cD3CBea99509d93f8DDDC8C6FB",
            "0xD1220A0cf47c7B9Be7A2E6BA89F429762e7b9aDb",
        ] {
            let stored = evm(address).unwrap();
            assert_eq!(stored, address.to_ascii_lowercase());
            let mixed = address[2..].bytes().any(|b| b.is_ascii_lowercase())
                && address[2..].bytes().any(|b| b.is_ascii_uppercase());
            if mixed {
                assert_eq!(evm_checksum(&stored), address);
            }
        }
        for address in [
            "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAeD",
            "0x5AAeb6053F3E94C9b9A09f33669435E7Ef1BeAed",
            "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeA",
            "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed00",
            "5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed",
            "0X5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed",
            "0xgaaeb6053f3e94c9b9a09f33669435e7ef1beaed",
            P2PKH,
        ] {
            assert_eq!(evm(address), Err(AddressError::Invalid), "{address}");
        }
    }

    #[test]
    fn solana_addresses() {
        let sol = |text: &str| parse_address(WalletKind::Solana, text);
        for address in [
            "So11111111111111111111111111111111111111112",
            "11111111111111111111111111111111",
            "CEyDYzCCrgfDDiiQTxrgGdLSEBrKeSCyuVrSUbyTjWxh",
            // Starts with a key prefix by chance.
            "xprvJChwnKLytsecV5vbr8jyFKQLBPGBxRn9YZFNkif",
        ] {
            assert_eq!(sol(address).as_deref(), Ok(address));
        }
        assert_eq!(sol(SOLANA_SECRET), Err(AddressError::KeyMaterial));
        for address in [
            "So1111111111111111111111111111111111111111",
            "0OIl0OIl0OIl0OIl0OIl0OIl0OIl0OIl",
            "1111111111111111111111111111111",
            "So11111111111111111111111111111111111111112So",
            P2PKH,
        ] {
            assert_eq!(sol(address), Err(AddressError::Invalid), "{address}");
        }
    }

    #[test]
    fn allowlist_is_consistent() {
        for chain in [
            Chain::Bitcoin,
            Chain::Ethereum,
            Chain::Base,
            Chain::Arbitrum,
            Chain::Optimism,
            Chain::Polygon,
            Chain::Solana,
        ] {
            let natives = TOKENS
                .iter()
                .filter(|t| t.chain == chain && t.contract.is_none())
                .count();
            assert_eq!(natives, 1, "{chain:?}");
        }
        for asset in ASSETS {
            assert!(
                (2..=12).contains(&asset.key.len())
                    && asset
                        .key
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()),
                "{}",
                asset.key
            );
            assert!(asset.aliases.iter().all(|a| *a == a.to_lowercase()));
            assert!(TOKENS.iter().any(|t| t.asset == asset.key), "{}", asset.key);
        }
        for (i, token) in TOKENS.iter().enumerate() {
            assert!(ASSETS.iter().any(|a| a.key == token.asset), "{token:?}");
            assert!(
                TOKENS[..i]
                    .iter()
                    .all(|t| (t.chain, t.asset) != (token.chain, token.asset)),
                "duplicate {token:?}"
            );
            assert!(
                TOKENS
                    .iter()
                    .filter(|t| t.asset == token.asset)
                    .all(|t| t.decimals == token.decimals),
                "decimals differ for {}",
                token.asset
            );
            match (token.chain, token.contract) {
                (_, None) | (Chain::Bitcoin, Some(_)) => {}
                (Chain::Solana, Some(mint)) => {
                    assert_eq!(solana(mint).as_deref(), Ok(mint));
                }
                (_, Some(contract)) => {
                    // The stored EIP-55 form proves the address has no typo.
                    let stored = evm(contract).unwrap();
                    assert_eq!(evm_checksum(&stored), contract);
                }
            }
        }
        assert!(TOKENS
            .iter()
            .all(|t| t.chain != Chain::Bitcoin || t.contract.is_none()));
    }
}
