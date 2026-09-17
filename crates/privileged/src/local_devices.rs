//! Deliberately separate from the remote device-signed operation protocol.
//! These requests have no authority on their own: the server must establish a
//! root Unix peer using kernel credentials before reading or executing them.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const SOCKET_PATH: &str = "/run/jarvis-core-admin/devices.sock";
pub const REQUEST_LIMIT: usize = 2048;
pub const RESPONSE_LIMIT: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeviceRequest {
    List {},
    Pending {},
    /// Both values come from the displayed pending request, not a caller-
    /// supplied public key. The backend must recheck expiry and consume once.
    Approve {
        request_id: Uuid,
        fingerprint: String,
    },
    Deny {
        request_id: Uuid,
    },
    Revoke {
        device_id: Uuid,
    },
}

impl DeviceRequest {
    pub fn parse(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > REQUEST_LIMIT {
            return Err("local device request too large");
        }
        let request: Self =
            serde_json::from_slice(bytes).map_err(|_| "invalid local device request")?;
        if let Self::Approve { fingerprint, .. } = &request {
            if fingerprint.len() != 64 || !fingerprint.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("invalid device fingerprint");
            }
        }
        Ok(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_fixed_typed_operations_are_accepted() {
        for operation in [
            DeviceRequest::List {},
            DeviceRequest::Pending {},
            DeviceRequest::Approve {
                request_id: Uuid::nil(),
                fingerprint: "ab".repeat(32),
            },
            DeviceRequest::Deny {
                request_id: Uuid::nil(),
            },
            DeviceRequest::Revoke {
                device_id: Uuid::nil(),
            },
        ] {
            assert_eq!(
                DeviceRequest::parse(&serde_json::to_vec(&operation).unwrap()),
                Ok(operation)
            );
        }
        for input in [
            r#"{"operation":"shell","command":"id"}"#,
            r#"{"operation":"list","approved":true}"#,
            r#"{"operation":"list","uid":0}"#,
            r#"{"operation":"revoke","device_id":"../../etc/passwd"}"#,
        ] {
            assert!(DeviceRequest::parse(input.as_bytes()).is_err());
        }
        assert!(DeviceRequest::parse(&vec![b' '; REQUEST_LIMIT + 1]).is_err());
    }

    #[test]
    fn approval_cannot_substitute_an_arbitrary_key_or_fingerprint() {
        for fingerprint in [
            "".to_owned(),
            "aa".repeat(31),
            "g".repeat(64),
            "\n".repeat(64),
        ] {
            let request = DeviceRequest::Approve {
                request_id: Uuid::nil(),
                fingerprint,
            };
            assert!(DeviceRequest::parse(&serde_json::to_vec(&request).unwrap()).is_err());
        }
    }
}
