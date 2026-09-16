//! Non-secret, action-bound account/device administration challenges.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AccountAction {
    PasswordSet,
    DeviceRevoke,
}

impl AccountAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PasswordSet => "password-set",
            Self::DeviceRevoke => "device-revoke",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountApproval {
    pub request_id: Uuid,
    pub user_id: Uuid,
    pub device_id: Uuid,
    pub action: AccountAction,
    pub target: Uuid,
    pub nonce: String,
    pub expires_at: i64,
}

/// Bind every authority-relevant field with a domain distinct from login,
/// pairing, unlock and agent approvals. Never sign arbitrary server bytes.
pub fn account_approval_message(request: &AccountApproval) -> Result<Vec<u8>, &'static str> {
    let nonce = hex::decode(&request.nonce).map_err(|_| "invalid account approval")?;
    if nonce.len() != 32 || request.expires_at <= 0 {
        return Err("invalid account approval");
    }
    let mut out = b"jarvis-account-approval-v1\0".to_vec();
    out.extend_from_slice(request.action.as_str().as_bytes());
    out.push(0);
    out.extend_from_slice(request.request_id.as_bytes());
    out.extend_from_slice(request.user_id.as_bytes());
    out.extend_from_slice(request.device_id.as_bytes());
    out.extend_from_slice(request.target.as_bytes());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&request.expires_at.to_be_bytes());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_authority_field_is_signature_bound() {
        let request = AccountApproval {
            request_id: Uuid::now_v7(),
            user_id: Uuid::now_v7(),
            device_id: Uuid::now_v7(),
            action: AccountAction::PasswordSet,
            target: Uuid::now_v7(),
            nonce: "ab".repeat(32),
            expires_at: 1900000000,
        };
        let expected = account_approval_message(&request).unwrap();
        for field in 0..7 {
            let mut changed = request.clone();
            match field {
                0 => changed.request_id = Uuid::now_v7(),
                1 => changed.user_id = Uuid::now_v7(),
                2 => changed.device_id = Uuid::now_v7(),
                3 => changed.target = Uuid::now_v7(),
                4 => changed.action = AccountAction::DeviceRevoke,
                5 => changed.nonce = "cd".repeat(32),
                _ => changed.expires_at += 1,
            }
            assert_ne!(expected, account_approval_message(&changed).unwrap());
        }
        let mut invalid = request;
        invalid.nonce = "ab".repeat(31);
        assert!(account_approval_message(&invalid).is_err());
    }
}
