//! Bounded, backend-only password hashing for account onboarding.
//!
//! No session or device can be authorized by this module. HTTP admission/rate
//! limits and signed-device checks must be enforced by the caller. Keep one
//! service per Core process: its shared semaphore bounds expensive work even
//! when an HTTP caller disconnects while a blocking hash is still running.

use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Algorithm, Argon2, Params, Version,
};
use std::{fmt, sync::Arc};
use tokio::sync::Semaphore;
use zeroize::Zeroizing;

const MEMORY_KIB: u32 = 64 * 1024;
const ITERATIONS: u32 = 3;
const LANES: u32 = 1;
const OUTPUT_BYTES: usize = 32;
const SALT_BYTES: usize = 16;
const MAX_PHC_BYTES: usize = 256;
pub const MIN_PASSWORD_CHARACTERS: usize = 15;
pub const MAX_PASSWORD_BYTES: usize = 1024;

/// Owned password bytes are wiped on drop, including on admission failure.
/// Intentionally not Clone/Serialize/Display. No normalization or trimming:
/// spaces, Unicode and case are part of the exact password.
pub struct AccountPassword(Zeroizing<String>);

impl<'de> serde::Deserialize<'de> for AccountPassword {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = <String as serde::Deserialize>::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl fmt::Debug for AccountPassword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccountPassword([REDACTED])")
    }
}

impl AccountPassword {
    pub fn new(value: String) -> Result<Self, PasswordError> {
        let value = Zeroizing::new(value);
        if value.len() > MAX_PASSWORD_BYTES
            || value.chars().count() < MIN_PASSWORD_CHARACTERS
            || value.chars().any(char::is_control)
        {
            return Err(PasswordError::InvalidInput);
        }
        Ok(Self(value))
    }
}

/// PHC verifier, private to backend storage. Not serializable to client DTOs.
pub struct StoredPassword(Zeroizing<String>);

impl fmt::Debug for StoredPassword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StoredPassword([REDACTED])")
    }
}

impl StoredPassword {
    /// Reject unexpected/expensive parameters before doing any Argon2 work.
    /// Future policy versions need an explicit bounded migration, not arbitrary
    /// parameters supplied in a database string or client request.
    pub fn from_storage(value: String) -> Result<Self, PasswordError> {
        let value = Zeroizing::new(value);
        if value.len() > MAX_PHC_BYTES {
            return Err(PasswordError::InvalidVerifier);
        }
        let hash = PasswordHash::new(&value).map_err(|_| PasswordError::InvalidVerifier)?;
        let params = Params::try_from(&hash).map_err(|_| PasswordError::InvalidVerifier)?;
        let mut salt = [0_u8; 64];
        let salt_len = hash
            .salt
            .ok_or(PasswordError::InvalidVerifier)?
            .decode_b64(&mut salt)
            .map_err(|_| PasswordError::InvalidVerifier)?
            .len();
        if hash.algorithm.as_str() != "argon2id"
            || hash.version != Some(19)
            || params.m_cost() != MEMORY_KIB
            || params.t_cost() != ITERATIONS
            || params.p_cost() != LANES
            || hash.params.iter().count() != 3
            || salt_len != SALT_BYTES
            || hash.hash.map(|h| h.len()) != Some(OUTPUT_BYTES)
        {
            return Err(PasswordError::InvalidVerifier);
        }
        Ok(Self(value))
    }

    /// Only use for parameter-bound database writes; never for public responses.
    pub fn as_storage_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PasswordError {
    #[error("password does not satisfy length or character requirements")]
    InvalidInput,
    #[error("account password verifier is unavailable")]
    InvalidVerifier,
    #[error("authentication failed")]
    AuthenticationFailed,
    #[error("password service busy; retry later")]
    Busy,
    #[error("password service unavailable")]
    Unavailable,
}

#[derive(Clone)]
pub struct PasswordService {
    slots: Arc<Semaphore>,
}

impl Default for PasswordService {
    fn default() -> Self {
        Self {
            slots: Arc::new(Semaphore::new(2)),
        }
    }
}

fn engine() -> Result<Argon2<'static>, PasswordError> {
    let params = Params::new(MEMORY_KIB, ITERATIONS, LANES, Some(OUTPUT_BYTES))
        .map_err(|_| PasswordError::Unavailable)?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

impl PasswordService {
    /// One admission pool shared by all account routes in this process.
    pub fn shared() -> &'static Self {
        static SERVICE: std::sync::OnceLock<PasswordService> = std::sync::OnceLock::new();
        SERVICE.get_or_init(Self::default)
    }

    async fn work<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> Result<T, PasswordError> + Send + 'static,
    ) -> Result<T, PasswordError> {
        // No unbounded queue of password-bearing requests waiting for memory.
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| PasswordError::Busy)?;
        tokio::task::spawn_blocking(move || {
            // Moving the permit into the closure prevents cancellation of its
            // awaiter from admitting more hashing jobs while this one runs.
            let _permit = permit;
            work()
        })
        .await
        .map_err(|_| PasswordError::Unavailable)?
    }

    pub async fn hash(&self, password: AccountPassword) -> Result<StoredPassword, PasswordError> {
        self.work(move || {
            let mut salt_bytes = [0_u8; SALT_BYTES];
            rand::RngCore::try_fill_bytes(&mut rand::rngs::OsRng, &mut salt_bytes)
                .map_err(|_| PasswordError::Unavailable)?;
            let salt =
                SaltString::encode_b64(&salt_bytes).map_err(|_| PasswordError::Unavailable)?;
            let hash = engine()?
                .hash_password(password.0.as_bytes(), &salt)
                .map_err(|_| PasswordError::Unavailable)?
                .to_string();
            StoredPassword::from_storage(hash)
        })
        .await
    }

    pub async fn verify(
        &self,
        password: AccountPassword,
        stored: StoredPassword,
    ) -> Result<(), PasswordError> {
        self.work(move || {
            let hash = PasswordHash::new(stored.as_storage_str())
                .map_err(|_| PasswordError::InvalidVerifier)?;
            engine()?
                .verify_password(password.0.as_bytes(), &hash)
                .map_err(|_| PasswordError::AuthenticationFailed)
        })
        .await
    }
}

#[cfg(test)]
mod tests;
