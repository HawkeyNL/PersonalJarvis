use anyhow::{bail, Context, Result};
use clap::Subcommand;
use jarvis_config::activation::{ActivationPolicy, ACTIVATION_FILE};
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
}

pub fn run(command: AccountCommand, json: bool) -> Result<()> {
    if json || !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!("activation codes require an interactive owner terminal; JSON/redirection refused");
    }
    let AccountCommand::ActivationCode { allow_cidr } = command;
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
