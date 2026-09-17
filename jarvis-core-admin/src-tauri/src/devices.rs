//! Local host management only. Mutations never use the retained broker grant:
//! every action starts a fresh, non-cached PolicyKit authentication process.
use crate::{
    admin,
    session::{BrokerRequest, SessionManager},
};
use serde::{Deserialize, Serialize};
use std::{fs, os::unix::fs::MetadataExt, time::Duration};

const POLICY: &str = "/usr/share/polkit-1/actions/com.hawkeynl.jarvis.devices.policy";
const POLICY_BYTES: &[u8] = include_bytes!("../../packaging/com.hawkeynl.jarvis.devices.policy");

#[derive(Deserialize, Serialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub status: String,
}
#[derive(Deserialize, Serialize)]
pub struct Pending {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub fingerprint: String,
    pub expires_at: i64,
}
#[derive(Serialize)]
pub struct Overview {
    devices: Vec<Device>,
    requests: Vec<Pending>,
}
#[derive(Deserialize)]
struct Devices {
    devices: Vec<Device>,
}
#[derive(Deserialize)]
struct Requests {
    requests: Vec<Pending>,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeviceAction {
    Approve {
        request_id: String,
        fingerprint: String,
    },
    Deny {
        request_id: String,
    },
    Revoke {
        device_id: String,
    },
}

impl DeviceAction {
    fn arguments(&self) -> Result<Vec<&str>, String> {
        let (id, mut args) = match self {
            Self::Approve {
                request_id,
                fingerprint,
            } => {
                if fingerprint.len() != 64 || !fingerprint.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err("invalid device fingerprint".into());
                }
                (
                    request_id,
                    vec![
                        "approve",
                        request_id.as_str(),
                        "--fingerprint",
                        fingerprint.as_str(),
                    ],
                )
            }
            Self::Deny { request_id } => (request_id, vec!["deny", request_id.as_str()]),
            Self::Revoke { device_id } => (device_id, vec!["revoke", device_id.as_str()]),
        };
        if id.len() != 36
            || !id.bytes().enumerate().all(|(i, b)| {
                if [8, 13, 18, 23].contains(&i) {
                    b == b'-'
                } else {
                    b.is_ascii_hexdigit()
                }
            })
        {
            return Err("invalid device/request ID".into());
        }
        args.splice(0..0, ["/usr/local/sbin/jarvis", "devices"]);
        Ok(args)
    }
}

pub fn overview(session: &SessionManager) -> Result<Overview, String> {
    let devices: Devices = serde_json::from_str(
        &session
            .run(BrokerRequest::Devices { pending: false })?
            .stdout,
    )
    .map_err(|_| "invalid device response")?;
    let requests: Requests = serde_json::from_str(
        &session
            .run(BrokerRequest::Devices { pending: true })?
            .stdout,
    )
    .map_err(|_| "invalid pending device response")?;
    Ok(Overview {
        devices: devices.devices,
        requests: requests.requests,
    })
}

pub fn mutate(session: &SessionManager, request: DeviceAction) -> Result<(), String> {
    admin::root_guard()?;
    session.require_active()?;
    let args = request.arguments()?;
    admin::verify_root_executable("/usr/local/sbin/jarvis")?;
    let info = fs::symlink_metadata(POLICY)
        .map_err(|_| "install the matching Core Admin package before managing devices")?;
    if !info.is_file()
        || info.uid() != 0
        || info.mode() & 0o022 != 0
        || info.len() != POLICY_BYTES.len() as u64
        || fs::read(POLICY).map_err(|_| "device authorization policy unavailable")? != POLICY_BYTES
    {
        return Err("unsafe or mismatched device authorization policy".into());
    }
    // No password parameter/stdin; the desktop system authentication agent owns
    // the prompt. auth_admin (not auth_admin_keep) requires fresh authorization.
    admin::run_checked_command("/usr/bin/pkexec", &args, Duration::from_secs(120))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_mutations_have_fixed_executable_and_typed_arguments() {
        let id = "00000000-0000-0000-0000-000000000001".to_string();
        let action = DeviceAction::Revoke {
            device_id: id.clone(),
        };
        assert_eq!(
            action.arguments().unwrap(),
            vec!["/usr/local/sbin/jarvis", "devices", "revoke", &id]
        );
        assert!(DeviceAction::Revoke {
            device_id: "--help".into()
        }
        .arguments()
        .is_err());
        assert!(serde_json::from_str::<DeviceAction>(
            r#"{"action":"revoke","device_id":"x","password":"x"}"#
        )
        .is_err());
        let policy = std::str::from_utf8(POLICY_BYTES).unwrap();
        assert!(policy.contains("<allow_active>auth_admin</allow_active>"));
        assert!(!policy.contains("auth_admin_keep"));
    }
}
