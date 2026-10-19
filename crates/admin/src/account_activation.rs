use crate::local_devices::{exchange, print, shown};
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use jarvis_config::activation::{ActivationPolicy, ACTIVATION_FILE};
use jarvis_privileged::local_devices::DeviceRequest;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{IsTerminal, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, PermissionsExt},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;
use zeroize::Zeroizing;

// 32 equally likely, readable symbols: 12 characters retain 60 bits of entropy.
// The ten-minute expiry, LAN restriction, rate limit and bootstrap latch remain.
const CODE_ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const CODE_LENGTH: usize = 12;

fn encode_code(bytes: &[u8; CODE_LENGTH]) -> Zeroizing<String> {
    Zeroizing::new(
        bytes
            .iter()
            .map(|byte| CODE_ALPHABET[(byte & 31) as usize] as char)
            .collect(),
    )
}

#[derive(Debug, Subcommand)]
pub enum AccountCommand {
    /// Generate a ten-minute first-device code for an explicitly allowed LAN.
    ActivationCode {
        #[arg(long, required = true)]
        allow_cidr: Vec<String>,
    },
    /// Confirm a forgotten-password reset signed on an approver device.
    Recovery {
        #[command(subcommand)]
        command: RecoveryCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum RecoveryCommand {
    /// Pending password resets; shows no secrets.
    Status,
    /// Set the new password, end every session and cancel pending requests.
    Confirm {
        request_id: Uuid,
        /// Skip the confirmation after checking the request on the device.
        #[arg(long)]
        yes: bool,
    },
    /// Refuse a pending reset; nothing changes.
    Deny { request_id: Uuid },
}

pub fn run(command: AccountCommand, json: bool) -> Result<()> {
    let allow_cidr = match command {
        AccountCommand::ActivationCode { allow_cidr } => allow_cidr,
        AccountCommand::Recovery { command } => return recovery(command, json),
    };
    if json || !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!("activation codes require an interactive owner terminal; JSON/redirection refused");
    }
    for parent in ["/etc", "/etc/jarvis"] {
        let metadata = fs::symlink_metadata(parent).context("activation directory unavailable")?;
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            bail!("activation directory must be root-owned and not writable by other users");
        }
    }
    match fs::symlink_metadata(ACTIVATION_FILE) {
        Ok(info)
            if !info.is_file()
                || info.uid() != 0
                || info.nlink() != 1
                || info.mode() & 0o027 != 0 =>
        {
            bail!("unsafe existing activation policy");
        }
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    // Fixed service group; neither its name nor the destination is user input.
    let group = unsafe { libc::getgrnam(c"jarvis".as_ptr()) };
    if group.is_null() {
        bail!("Jarvis service group is unavailable");
    }
    let gid = unsafe { (*group).gr_gid };
    let mut raw = Zeroizing::new([0_u8; CODE_LENGTH]);
    rand::RngCore::try_fill_bytes(&mut rand::rngs::OsRng, &mut *raw)
        .map_err(|_| anyhow::anyhow!("secure randomness unavailable"))?;
    let code = encode_code(&raw);
    let issued_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let policy = ActivationPolicy {
        schema_version: 1,
        secret_sha256: hex::encode(Sha256::digest(code.as_bytes())),
        allowed_cidrs: allow_cidr,
        issued_at,
        expires_at: issued_at.checked_add(600).context("clock overflow")?,
    };
    policy.validate(issued_at).map_err(anyhow::Error::msg)?;
    let mut staged = tempfile::NamedTempFile::new_in("/etc/jarvis")?;
    serde_json::to_writer(staged.as_file_mut(), &policy)?;
    if unsafe { libc::fchown(staged.as_file().as_raw_fd(), 0, gid) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    staged
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o640))?;
    staged.as_file().sync_all()?;
    staged
        .persist(ACTIVATION_FILE)
        .map_err(|_| anyhow::anyhow!("could not activate local enrollment policy"))?;
    fs::File::open("/etc/jarvis")?.sync_all()?;
    // Controlling TTY only. No code in argv, JSON, files, or captured logs.
    let mut tty = fs::OpenOptions::new().write(true).open("/dev/tty")?;
    writeln!(
        tty,
        "One-time Jarvis activation code (valid for 10 minutes):\n{}",
        code.as_str()
    )?;
    writeln!(tty, "Use it with your new account password on the first device. Existing enrollment cannot be reset with this code.")?;
    Ok(())
}

fn recovery(command: RecoveryCommand, json: bool) -> Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        bail!("account recovery requires the host owner root path");
    }
    if json && !matches!(command, RecoveryCommand::Status) {
        bail!("JSON mode is read-only");
    }
    match command {
        RecoveryCommand::Status if json => print(&exchange(&DeviceRequest::RecoveryStatus {})?),
        RecoveryCommand::Status => {
            let pending =
                PendingRecoveries::deserialize(exchange(&DeviceRequest::RecoveryStatus {})?)
                    .context("invalid recovery status from Core")?;
            if pending.requests.is_empty() {
                println!("No pending password reset.");
            }
            for request in &pending.requests {
                println!("{}", describe(request));
            }
            Ok(())
        }
        RecoveryCommand::Confirm { request_id, yes } => {
            let pending = exchange(&DeviceRequest::RecoveryStatus {})?;
            let request = pending_request(&pending, request_id)?;
            println!("{}", describe(&request));
            if !yes {
                crate::confirm("Only confirm a reset you started on this device. Set the new password and sign out every device?")?;
            }
            print(&exchange(&DeviceRequest::RecoveryConfirm { request_id })?)
        }
        RecoveryCommand::Deny { request_id } => {
            print(&exchange(&DeviceRequest::RecoveryDeny { request_id })?)
        }
    }
}

#[derive(Deserialize)]
struct PendingRecoveries {
    requests: Vec<PendingRecovery>,
}

#[derive(Deserialize)]
struct PendingRecovery {
    id: Uuid,
    device_name: String,
    platform: String,
    created_at: String,
    expires_at: String,
}

/// Device names come from remote clients: `shown` escapes bidi and controls.
fn describe(request: &PendingRecovery) -> String {
    format!(
        "{}  from {} ({}), created {}, expires {}",
        request.id,
        shown(&request.device_name),
        shown(&request.platform),
        shown(&request.created_at),
        shown(&request.expires_at)
    )
}

fn pending_request(data: &serde_json::Value, request_id: Uuid) -> Result<PendingRecovery> {
    PendingRecoveries::deserialize(data)
        .context("invalid recovery status from Core")?
        .requests
        .into_iter()
        .find(|request| request.id == request_id)
        .context("no pending recovery with this id; run `sudo jarvis account recovery status`")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_has_twelve_readable_symbols_and_uniform_mapping() {
        let mut counts = [0; 32];
        for byte in 0..=255_u8 {
            let code = encode_code(&[byte; CODE_LENGTH]);
            assert_eq!(code.len(), CODE_LENGTH);
            assert!(code.bytes().all(|symbol| CODE_ALPHABET.contains(&symbol)));
            let index = CODE_ALPHABET
                .iter()
                .position(|symbol| *symbol == code.as_bytes()[0])
                .unwrap();
            counts[index] += 1;
        }
        assert_eq!(counts, [8; 32]);
    }

    #[test]
    fn recovery_confirm_needs_a_listed_pending_request() {
        let id = Uuid::from_u128(7);
        let data = serde_json::json!({"requests":[{"id":id,"device_id":Uuid::nil(),
            "device_name":"phone","platform":"ios","created_at":"2026-10-15T10:00:00Z",
            "expires_at":"2026-10-15T10:15:00Z"}]});
        let request = pending_request(&data, id).unwrap();
        assert_eq!(request.device_name, "phone");
        assert!(describe(&request).starts_with(&format!("{id}  from \"phone\" (\"ios\")")));
        assert!(pending_request(&data, Uuid::from_u128(8)).is_err());
        assert!(pending_request(&serde_json::json!({}), id).is_err());
    }

    #[test]
    fn short_code_uses_existing_hash_only_policy() {
        let code = encode_code(&[0; CODE_LENGTH]);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let policy = ActivationPolicy {
            schema_version: 1,
            secret_sha256: hex::encode(Sha256::digest(code.as_bytes())),
            allowed_cidrs: vec!["10.23.45.10/32".into()],
            issued_at: now,
            expires_at: now + 600,
        };
        let verifier = policy.validate(now).unwrap();
        assert!(verifier.verifies(&code));
        assert!(!verifier.verifies("BBBBBBBBBBBB"));
        assert!(!verifier.verifies("aaaaaaaaaaaa"));
        assert!(!serde_json::to_string(&policy)
            .unwrap()
            .contains(code.as_str()));
        assert!(policy.validate(now + 600).is_err());
    }
}
