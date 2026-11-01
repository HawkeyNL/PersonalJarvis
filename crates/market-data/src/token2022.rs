//! Parse an SPL Token or Token-2022 mint from raw account data.
//!
//! Layout (spl-token / spl-token-2022 interface):
//!
//! - bytes `0..82`: the base mint. `COption<Pubkey>` mint authority (u32 LE
//!   tag + 32 bytes), u64 LE supply, u8 decimals, bool initialized,
//!   `COption<Pubkey>` freeze authority;
//! - Token-2022 only, when there are extensions: zero padding up to byte 165,
//!   the account type byte at 165 (`1` = mint), then TLV entries of u16 LE
//!   type, u16 LE length and the value. Type `0` ends the list.
//!
//! Every read is bounds-checked; any layout surprise is an error, never a
//! panic and never a guess.

use jarvis_onchain::types::{Extension, ExtensionKind};

pub const SPL_TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
pub const TOKEN_2022_PROGRAM: &str = "TokenzQdBNbLqP5VEhdkAjS5EQ8i3MBVZ1zRSThdFEsd";

const MINT_LEN: usize = 82;
const ACCOUNT_TYPE_OFFSET: usize = 165;
const MULTISIG_LEN: usize = 355;
const ACCOUNT_TYPE_MINT: u8 = 1;

/// Why a mint could not be parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid mint account data: {0}")]
pub struct MintParseError(pub &'static str);

/// Which token program owns the mint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenProgram {
    Spl,
    Token2022,
}

impl TokenProgram {
    /// The program for an account `owner`, or `None` if it is no token program.
    pub fn from_owner(owner: &str) -> Option<Self> {
        match owner {
            SPL_TOKEN_PROGRAM => Some(Self::Spl),
            TOKEN_2022_PROGRAM => Some(Self::Token2022),
            _ => None,
        }
    }
}

/// A parsed mint. Authorities are raw 32-byte public keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintAccount {
    pub program: TokenProgram,
    pub supply: u64,
    pub decimals: u8,
    pub mint_authority: Option<[u8; 32]>,
    pub freeze_authority: Option<[u8; 32]>,
    pub extensions: Vec<Extension>,
}

/// Parse `data` as a mint owned by `program`.
pub fn parse_mint(program: TokenProgram, data: &[u8]) -> Result<MintAccount, MintParseError> {
    let base = data
        .get(..MINT_LEN)
        .ok_or(MintParseError("shorter than a mint"))?;
    let mint_authority = coption_pubkey(&base[0..36])?;
    let supply = u64::from_le_bytes(take::<8>(&base[36..44])?);
    let decimals = base[44];
    if base[45] != 1 {
        return Err(MintParseError("mint is not initialized"));
    }
    let freeze_authority = coption_pubkey(&base[46..82])?;

    let extensions = match (program, data.len()) {
        (_, MINT_LEN) => Vec::new(),
        (TokenProgram::Spl, _) => return Err(MintParseError("SPL mint has a wrong length")),
        (TokenProgram::Token2022, len) if len <= ACCOUNT_TYPE_OFFSET || len == MULTISIG_LEN => {
            return Err(MintParseError("Token-2022 mint has a wrong length"))
        }
        (TokenProgram::Token2022, _) => {
            if data[MINT_LEN..ACCOUNT_TYPE_OFFSET].iter().any(|b| *b != 0) {
                return Err(MintParseError("padding is not zero"));
            }
            if data[ACCOUNT_TYPE_OFFSET] != ACCOUNT_TYPE_MINT {
                return Err(MintParseError("account type is not a mint"));
            }
            parse_extensions(&data[ACCOUNT_TYPE_OFFSET + 1..], freeze_authority.is_none())?
        }
    };
    Ok(MintAccount {
        program,
        supply,
        decimals,
        mint_authority,
        freeze_authority,
        extensions,
    })
}

fn parse_extensions(
    mut tlv: &[u8],
    freeze_revoked: bool,
) -> Result<Vec<Extension>, MintParseError> {
    let mut seen: Vec<u16> = Vec::new();
    let mut out = Vec::new();
    while !tlv.is_empty() {
        let Some(header) = tlv.get(..4) else {
            if tlv.iter().all(|b| *b == 0) {
                break;
            }
            return Err(MintParseError("truncated extension header"));
        };
        let kind = u16::from_le_bytes([header[0], header[1]]);
        if kind == 0 {
            break;
        }
        let len = usize::from(u16::from_le_bytes([header[2], header[3]]));
        let value = tlv
            .get(4..4 + len)
            .ok_or(MintParseError("extension value runs past the account"))?;
        if seen.contains(&kind) {
            return Err(MintParseError("duplicate extension"));
        }
        seen.push(kind);
        out.push(extension(kind, value, freeze_revoked)?);
        tlv = &tlv[4 + len..];
    }
    Ok(out)
}

/// Map one extension and decide whether its authority is gone. Where an
/// extension carries more than one lever (fee config + withdraw authority,
/// hook authority + hook program) every lever must be gone.
fn extension(kind: u16, value: &[u8], freeze_revoked: bool) -> Result<Extension, MintParseError> {
    use ExtensionKind::*;
    let none_at = |offset: usize| optional_pubkey_is_none(value, offset);
    let (kind, authority_revoked) = match kind {
        // Authorities gone is not enough: a fee frozen above zero still
        // takes a cut of every sell. Older and newer fee bps at 88 and 106.
        1 => {
            let bps_zero = |at: usize| value.get(at..at + 2) == Some(&[0, 0][..]);
            let fee_zero = value.len() >= 108 && bps_zero(88) && bps_zero(106);
            (TransferFeeConfig, none_at(0)? && none_at(32)? && fee_zero)
        }
        3 => (MintCloseAuthority, none_at(0)?),
        // Confidential balances are invisible to `getTokenLargestAccounts`,
        // so the holder share could be understated: never revoked.
        4 => (ConfidentialTransfer, false),
        6 => {
            // No own authority: the freeze authority changes the default
            // state, and a frozen default blocks every new holder.
            let state = *value.first().ok_or(MintParseError("truncated extension"))?;
            (DefaultAccountState, freeze_revoked && state == 1)
        }
        // Can never be sold.
        9 => (NonTransferable, false),
        10 => (InterestBearing, none_at(0)?),
        12 => (PermanentDelegate, none_at(0)?),
        14 => (TransferHook, none_at(0)? && none_at(32)?),
        18 => (MetadataPointer, none_at(0)?),
        19 => (TokenMetadata, none_at(0)?),
        // A paused mint with a revoked authority can never be unpaused.
        26 => (Pausable, none_at(0)? && value.get(32) == Some(&0)),
        other => (Unknown(other), false),
    };
    Ok(Extension {
        kind,
        authority_revoked,
    })
}

/// `OptionalNonZeroPubkey` at `offset`: all zeroes means none.
fn optional_pubkey_is_none(value: &[u8], offset: usize) -> Result<bool, MintParseError> {
    let key = value
        .get(offset..offset + 32)
        .ok_or(MintParseError("truncated extension"))?;
    Ok(key.iter().all(|b| *b == 0))
}

fn coption_pubkey(bytes: &[u8]) -> Result<Option<[u8; 32]>, MintParseError> {
    match u32::from_le_bytes(take::<4>(&bytes[..4])?) {
        0 => Ok(None),
        1 => Ok(Some(take::<32>(&bytes[4..36])?)),
        _ => Err(MintParseError("invalid option tag")),
    }
}

fn take<const N: usize>(bytes: &[u8]) -> Result<[u8; N], MintParseError> {
    bytes
        .try_into()
        .map_err(|_| MintParseError("truncated field"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn base_mint(mint_authority: Option<u8>, freeze_authority: Option<u8>) -> Vec<u8> {
        let mut data = Vec::with_capacity(MINT_LEN);
        let push_option = |data: &mut Vec<u8>, key: Option<u8>| match key {
            Some(byte) => {
                data.extend_from_slice(&1u32.to_le_bytes());
                data.extend_from_slice(&[byte; 32]);
            }
            None => data.extend_from_slice(&[0; 36]),
        };
        push_option(&mut data, mint_authority);
        data.extend_from_slice(&1_000_000_000_000u64.to_le_bytes());
        data.push(6);
        data.push(1);
        push_option(&mut data, freeze_authority);
        data
    }

    /// A Token-2022 mint with the given `(type, value)` extensions.
    pub fn mint_2022(extensions: &[(u16, Vec<u8>)]) -> Vec<u8> {
        let mut data = base_mint(None, None);
        data.resize(ACCOUNT_TYPE_OFFSET, 0);
        data.push(ACCOUNT_TYPE_MINT);
        for (kind, value) in extensions {
            data.extend_from_slice(&kind.to_le_bytes());
            data.extend_from_slice(&(value.len() as u16).to_le_bytes());
            data.extend_from_slice(value);
        }
        data
    }

    fn kinds(mint: &MintAccount) -> Vec<(ExtensionKind, bool)> {
        mint.extensions
            .iter()
            .map(|e| (e.kind, e.authority_revoked))
            .collect()
    }

    #[test]
    fn parses_classic_spl_mint() {
        let mint = parse_mint(TokenProgram::Spl, &base_mint(Some(7), None)).unwrap();
        assert_eq!(mint.mint_authority, Some([7; 32]));
        assert_eq!(mint.freeze_authority, None);
        assert_eq!(mint.supply, 1_000_000_000_000);
        assert_eq!(mint.decimals, 6);
        assert!(mint.extensions.is_empty());
        // An SPL mint never has trailing extension data.
        assert!(parse_mint(TokenProgram::Spl, &mint_2022(&[])).is_err());
    }

    #[test]
    fn parses_token_2022_extensions_and_authorities() {
        let key = vec![9u8; 32];
        let zero = vec![0u8; 32];
        let data = mint_2022(&[
            (18, [zero.clone(), key.clone()].concat()), // metadata pointer, authority revoked
            (19, [key.clone(), key.clone(), vec![0; 12]].concat()), // metadata, update authority set
            (14, [zero.clone(), key.clone()].concat()),             // hook program still set
            (12, key.clone()),                                      // permanent delegate
            (6, vec![1]),                                           // default state initialized
            (9, vec![]),                                            // non-transferable
            (20, vec![0; 64]),                                      // group pointer: not modelled
        ]);
        let mint = parse_mint(TokenProgram::Token2022, &data).unwrap();
        use ExtensionKind::*;
        assert_eq!(
            kinds(&mint),
            vec![
                (MetadataPointer, true),
                (TokenMetadata, false),
                (TransferHook, false),
                (PermanentDelegate, false),
                (DefaultAccountState, true),
                (NonTransferable, false),
                (Unknown(20), false),
            ]
        );
    }

    #[test]
    fn transfer_fee_counts_as_revoked_only_without_authorities_and_fee() {
        let revoked = |value: Vec<u8>| {
            parse_mint(TokenProgram::Token2022, &mint_2022(&[(1, value)]))
                .unwrap()
                .extensions[0]
                .authority_revoked
        };
        assert!(revoked(vec![0; 108]));
        let mut fee = vec![0; 108];
        fee[106] = 1;
        assert!(!revoked(fee));
        let mut authority = vec![0; 108];
        authority[40] = 1;
        assert!(!revoked(authority));
        assert!(!revoked(vec![0; 100]));
    }

    #[test]
    fn frozen_default_state_or_live_freeze_authority_is_not_revoked() {
        let frozen = parse_mint(TokenProgram::Token2022, &mint_2022(&[(6, vec![2])])).unwrap();
        assert_eq!(
            kinds(&frozen),
            vec![(ExtensionKind::DefaultAccountState, false)]
        );

        let mut live = mint_2022(&[(6, vec![1])]);
        live[46..82].copy_from_slice(&base_mint(None, Some(3))[46..82]);
        let live = parse_mint(TokenProgram::Token2022, &live).unwrap();
        assert_eq!(
            kinds(&live),
            vec![(ExtensionKind::DefaultAccountState, false)]
        );
    }

    #[test]
    fn rejects_bad_layouts() {
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("bad tag", {
                let mut d = base_mint(None, None);
                d[0] = 2;
                d
            }),
            ("uninitialized", {
                let mut d = base_mint(None, None);
                d[45] = 0;
                d
            }),
            ("padding", {
                let mut d = mint_2022(&[]);
                d[100] = 1;
                d
            }),
            ("account type", {
                let mut d = mint_2022(&[]);
                d[ACCOUNT_TYPE_OFFSET] = 2;
                d
            }),
            ("165 bytes", mint_2022(&[])[..ACCOUNT_TYPE_OFFSET].to_vec()),
            ("multisig length", {
                let mut d = mint_2022(&[]);
                d.resize(MULTISIG_LEN, 0);
                d
            }),
            ("duplicate", mint_2022(&[(9, vec![]), (9, vec![])])),
            ("short value", mint_2022(&[(12, vec![1; 31])])),
            ("overrun", {
                let mut d = mint_2022(&[(12, vec![1; 32])]);
                d.truncate(d.len() - 1);
                d
            }),
            ("truncated header", {
                let mut d = mint_2022(&[]);
                d.extend_from_slice(&[12, 0]);
                d
            }),
        ];
        for (name, data) in cases {
            assert!(
                parse_mint(TokenProgram::Token2022, &data).is_err(),
                "{name}"
            );
        }
    }

    #[test]
    fn truncated_and_garbage_data_never_panics() {
        let full = mint_2022(&[
            (18, vec![5; 64]),
            (19, vec![5; 80]),
            (1, vec![0; 108]),
            (26, vec![0; 33]),
        ]);
        for len in 0..=full.len() {
            let _ = parse_mint(TokenProgram::Token2022, &full[..len]);
            let _ = parse_mint(TokenProgram::Spl, &full[..len]);
        }
        let mut rng = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        for _ in 0..20_000 {
            let mut data = full.clone();
            for _ in 0..(next() % 8) {
                let at = (next() as usize) % data.len();
                data[at] = next() as u8;
            }
            data.truncate((next() as usize) % (data.len() + 1));
            let _ = parse_mint(TokenProgram::Token2022, &data);
            let random: Vec<u8> = (0..(next() % 600)).map(|_| next() as u8).collect();
            let _ = parse_mint(TokenProgram::Token2022, &random);
        }
    }
}
