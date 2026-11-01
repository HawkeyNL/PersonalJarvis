//! Local read-only IBKR broker: the only process that holds the IBKR OAuth
//! credentials. It answers a fixed allowlist of typed read requests over a
//! Unix socket, and only for peers whose kernel-reported uid is allowlisted.
//! There is no generic path proxy and no order, reply, what-if, cancel or
//! account-switch request.
//!
//! Framing mirrors the other Jarvis brokers: one JSON request line of at most
//! [`MAX_REQUEST_BYTES`], answered by one JSON reply line of at most
//! [`MAX_REPLY_BYTES`], then the broker closes the connection.
//!
//! [`BrokerClient`] is the typed client Core uses; [`server`] is the broker.

use std::{ffi::CString, path::PathBuf, time::Duration};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

use crate::{
    oauth::{Account, History, Ledger, Position, SecdefMatch, Snapshot},
    IbkrError,
};

pub mod server;

#[cfg(test)]
mod tests;

/// systemd `RuntimeDirectory=jarvis-ibkr-broker`.
pub const DEFAULT_SOCKET: &str = "/run/jarvis-ibkr-broker/broker.sock";
/// The largest valid request (50 snapshot conids) is well below this.
pub const MAX_REQUEST_BYTES: usize = 4 * 1024;
pub const MAX_REPLY_BYTES: usize = 8 * 1024 * 1024;
/// Upper bound for one upstream read inside the broker, including IBKR
/// pacing and a session mint.
pub const UPSTREAM_DEADLINE: Duration = Duration::from_secs(60);
const CLIENT_DEADLINE: Duration = Duration::from_secs(75);

/// The complete set of requests the broker serves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BrokerRequest {
    Accounts {},
    Positions {
        account_id: String,
    },
    Ledger {
        account_id: String,
    },
    SecdefSearch {
        symbol: String,
    },
    Snapshot {
        conids: Vec<i64>,
    },
    History {
        conid: i64,
        period: String,
        bar: String,
    },
}

impl BrokerRequest {
    /// Fixed name for audit logs.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Accounts {} => "accounts",
            Self::Positions { .. } => "positions",
            Self::Ledger { .. } => "ledger",
            Self::SecdefSearch { .. } => "secdef_search",
            Self::Snapshot { .. } => "snapshot",
            Self::History { .. } => "history",
        }
    }

    fn account_id(&self) -> Option<&str> {
        match self {
            Self::Positions { account_id } | Self::Ledger { account_id } => Some(account_id),
            _ => None,
        }
    }
}

/// Reply line: `{"ok": <data>}` or `{"error": {"code": ...}}`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Reply<T> {
    Ok(T),
    Error(BrokerError),
}

/// Errors on the broker socket. They never carry a URL, account id or any
/// credential material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum BrokerError {
    #[error("invalid IBKR broker request")]
    InvalidRequest,
    #[error("invalid IBKR request parameter: {field}")]
    InvalidInput { field: String },
    #[error("IBKR broker is busy")]
    Busy,
    #[error("IBKR is rate limiting; retry in {retry_after_secs} s")]
    RetryLater { retry_after_secs: u64 },
    #[error("IBKR authentication failed")]
    UpstreamAuth,
    #[error("IBKR request failed")]
    Upstream,
    #[error("IBKR reply too large")]
    TooLarge,
    #[error("IBKR broker request timed out")]
    Timeout,
    /// Client side only: the socket could not be reached or answered badly.
    #[error("IBKR broker unavailable")]
    Unavailable,
}

impl From<&IbkrError> for BrokerError {
    fn from(error: &IbkrError) -> Self {
        match error {
            IbkrError::InvalidInput(field) => Self::InvalidInput {
                field: (*field).to_string(),
            },
            IbkrError::RateLimited { retry_after_secs }
            | IbkrError::MintCooldown { retry_after_secs } => Self::RetryLater {
                retry_after_secs: *retry_after_secs,
            },
            IbkrError::Busy => Self::Busy,
            IbkrError::TooLarge => Self::TooLarge,
            IbkrError::Unauthorized
            | IbkrError::NotAuthenticated
            | IbkrError::Credential(_)
            | IbkrError::Handshake(_) => Self::UpstreamAuth,
            IbkrError::Http(_) | IbkrError::Decode(_) => Self::Upstream,
        }
    }
}

/// uid of a user name. Call once at startup: `getpwnam` is not reentrant.
pub fn lookup_uid(name: &str) -> Option<u32> {
    let name = CString::new(name).ok()?;
    let entry = unsafe { libc::getpwnam(name.as_ptr()) };
    (!entry.is_null()).then(|| unsafe { (*entry).pw_uid })
}

/// Typed client for the broker socket. Holds no credentials.
#[derive(Debug, Clone)]
pub struct BrokerClient {
    socket: PathBuf,
    broker_uid: u32,
}

impl BrokerClient {
    /// `broker_uid` is the broker's service uid (see [`lookup_uid`]): a
    /// socket served by any other process is refused before sending.
    pub fn new(socket: impl Into<PathBuf>, broker_uid: u32) -> Self {
        Self {
            socket: socket.into(),
            broker_uid,
        }
    }

    pub async fn accounts(&self) -> Result<Vec<Account>, BrokerError> {
        self.call(&BrokerRequest::Accounts {}).await
    }

    pub async fn positions(&self, account_id: &str) -> Result<Vec<Position>, BrokerError> {
        self.call(&BrokerRequest::Positions {
            account_id: account_id.to_string(),
        })
        .await
    }

    pub async fn ledger(&self, account_id: &str) -> Result<Ledger, BrokerError> {
        self.call(&BrokerRequest::Ledger {
            account_id: account_id.to_string(),
        })
        .await
    }

    pub async fn secdef_search(&self, symbol: &str) -> Result<Vec<SecdefMatch>, BrokerError> {
        self.call(&BrokerRequest::SecdefSearch {
            symbol: symbol.to_string(),
        })
        .await
    }

    pub async fn snapshot(&self, conids: &[i64]) -> Result<Vec<Snapshot>, BrokerError> {
        self.call(&BrokerRequest::Snapshot {
            conids: conids.to_vec(),
        })
        .await
    }

    pub async fn history(
        &self,
        conid: i64,
        period: &str,
        bar: &str,
    ) -> Result<History, BrokerError> {
        self.call(&BrokerRequest::History {
            conid,
            period: period.to_string(),
            bar: bar.to_string(),
        })
        .await
    }

    async fn call<T: DeserializeOwned>(&self, request: &BrokerRequest) -> Result<T, BrokerError> {
        let mut line = serde_json::to_vec(request).map_err(|_| BrokerError::InvalidRequest)?;
        line.push(b'\n');
        if line.len() > MAX_REQUEST_BYTES {
            return Err(BrokerError::InvalidRequest);
        }
        let reply = tokio::time::timeout(CLIENT_DEADLINE, async {
            let mut stream = UnixStream::connect(&self.socket).await.ok()?;
            if stream.peer_cred().ok()?.uid() != self.broker_uid {
                return None;
            }
            // A busy broker may answer and close before reading the request.
            let _ = stream.write_all(&line).await;
            let mut reply = Vec::new();
            BufReader::new(stream.take(MAX_REPLY_BYTES as u64 + 1))
                .read_until(b'\n', &mut reply)
                .await
                .ok()?;
            Some(reply)
        })
        .await
        .map_err(|_| BrokerError::Timeout)?
        .filter(|reply| reply.len() <= MAX_REPLY_BYTES && reply.last() == Some(&b'\n'))
        .ok_or(BrokerError::Unavailable)?;
        match serde_json::from_slice(&reply).map_err(|_| BrokerError::Unavailable)? {
            Reply::Ok(data) => Ok(data),
            Reply::Error(error) => Err(error),
        }
    }
}
