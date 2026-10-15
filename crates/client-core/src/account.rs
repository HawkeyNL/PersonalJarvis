//! Non-secret, action-bound account/device administration challenges.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AccountAction {
    PasswordSet,
    DeviceRevoke,
    /// Forgotten-password recovery: carries no current password and only
    /// takes effect after local root confirmation on the Home Node.
    PasswordReset,
}

impl AccountAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PasswordSet => "password-set",
            Self::DeviceRevoke => "device-revoke",
            Self::PasswordReset => "password-reset",
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

    /// Fixed password-reset vector for the Swift and Kotlin clients. Only the
    /// action label differs from password-set, so the domains never collide.
    #[test]
    fn password_reset_approval_matches_golden_bytes() {
        let request = AccountApproval {
            request_id: Uuid::parse_str("00112233-4455-6677-8899-aabbccddeeff").unwrap(),
            user_id: Uuid::parse_str("10213243-5465-7687-98a9-bacbdcedfe0f").unwrap(),
            device_id: Uuid::parse_str("ffeeddcc-bbaa-9988-7766-554433221100").unwrap(),
            action: AccountAction::PasswordReset,
            target: Uuid::parse_str("10213243-5465-7687-98a9-bacbdcedfe0f").unwrap(),
            nonce: hex::encode((0_u8..32).collect::<Vec<_>>()),
            expires_at: 1_800_000_000,
        };
        let reset = account_approval_message(&request).unwrap();
        assert_eq!(
            hex::encode(&reset),
            concat!(
                "6a61727669732d6163636f756e742d617070726f76616c2d763100",
                "70617373776f72642d726573657400",
                "00112233445566778899aabbccddeeff",
                "102132435465768798a9bacbdcedfe0f",
                "ffeeddccbbaa99887766554433221100",
                "102132435465768798a9bacbdcedfe0f",
                "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
                "000000006b49d200"
            )
        );
        let set = AccountApproval {
            action: AccountAction::PasswordSet,
            ..request
        };
        assert_ne!(reset, account_approval_message(&set).unwrap());
        assert_eq!(
            serde_json::to_value(AccountAction::PasswordReset).unwrap(),
            "password-reset"
        );
    }

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
