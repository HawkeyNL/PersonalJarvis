//! In-memory health of the OAuth client, for the broker's `status` request.
//! It is kept under its own short synchronous lock and read without touching
//! the session, the brokerage lock, the pacer or the network.

use serde::{Deserialize, Serialize};

use super::LST_REFRESH_MARGIN;
use crate::{AuthStatus, IbkrError};

/// Fixed status code of an error, safe for logs and replies.
pub fn status_code(error: &IbkrError) -> &'static str {
    match error {
        IbkrError::Unauthorized | IbkrError::Credential(_) | IbkrError::Handshake(_) => {
            "auth_rejected"
        }
        IbkrError::NotAuthenticated => "not_authenticated",
        IbkrError::RateLimited { .. } | IbkrError::MintCooldown { .. } => "rate_limited",
        IbkrError::Http(_) | IbkrError::Decode(_) | IbkrError::TooLarge => "upstream",
        IbkrError::Busy => "busy",
        IbkrError::InvalidInput(_) => "invalid_input",
    }
}

/// Status of the OAuth client as served by the broker. Times are Unix
/// seconds. It carries no token, generation, credential, account id or URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrokerStatus {
    pub started_at: u64,
    pub lst: LstStatus,
    pub last_mint: MintStatus,
    /// Last successful signed IBKR request.
    pub last_ok_at: Option<u64>,
    pub last_error: Option<ErrorAt>,
    /// Only while a 429 back-off is still active.
    pub rate_limited_until: Option<u64>,
    pub brokerage: Brokerage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LstStatus {
    pub state: LstState,
    pub minted_at: Option<u64>,
    pub expires_at: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LstState {
    /// Not minted yet: normal after start, minting is lazy.
    None,
    Valid,
    /// Inside the refresh margin; the next request mints a new one.
    RefreshDue,
    Expired,
    /// IBKR answered 401 to a request signed with the current LST.
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MintStatus {
    pub at: Option<u64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorAt {
    pub at: u64,
    pub code: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Brokerage {
    #[default]
    Closed,
    Open,
    Competing,
}

impl From<&AuthStatus> for Brokerage {
    fn from(status: &AuthStatus) -> Self {
        if status.competing {
            Self::Competing
        } else if status.authenticated {
            Self::Open
        } else {
            Self::Closed
        }
    }
}

/// Mutable side of [`BrokerStatus`]; never held across an await.
#[derive(Default)]
pub(super) struct ClientStatus {
    pub(super) started_at: u64,
    pub(super) minted_at: Option<u64>,
    pub(super) expires_at: Option<u64>,
    /// Generation of the LST described above, to match a 401 to it.
    pub(super) generation: u64,
    pub(super) rejected: bool,
    pub(super) last_mint_at: Option<u64>,
    pub(super) last_mint_error: Option<&'static str>,
    pub(super) last_ok_at: Option<u64>,
    pub(super) last_error: Option<(u64, &'static str)>,
    pub(super) rate_limited_until: Option<u64>,
    pub(super) brokerage: Brokerage,
}

impl ClientStatus {
    pub(super) fn report(&self, now: u64) -> BrokerStatus {
        let state = match self.expires_at {
            None => LstState::None,
            Some(_) if self.rejected => LstState::Rejected,
            Some(expires) if now >= expires => LstState::Expired,
            Some(expires) if now + LST_REFRESH_MARGIN.as_secs() >= expires => LstState::RefreshDue,
            Some(_) => LstState::Valid,
        };
        BrokerStatus {
            started_at: self.started_at,
            lst: LstStatus {
                state,
                minted_at: self.minted_at,
                expires_at: self.expires_at,
            },
            last_mint: MintStatus {
                at: self.last_mint_at,
                error: self.last_mint_error.map(str::to_string),
            },
            last_ok_at: self.last_ok_at,
            last_error: self.last_error.map(|(at, code)| ErrorAt {
                at,
                code: code.to_string(),
            }),
            rate_limited_until: self.rate_limited_until.filter(|until| *until > now),
            brokerage: self.brokerage,
        }
    }
}
