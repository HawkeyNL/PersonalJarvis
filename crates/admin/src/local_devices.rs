use anyhow::{bail, Context, Result};
use clap::Subcommand;
use jarvis_privileged::{
    local_devices::{DeviceRequest, RESPONSE_LIMIT, SOCKET_PATH},
    trusted_keys::{self, TRUSTED_DEVICE_KEYS_PATH},
};
use serde::Deserialize;
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{FileTypeExt, MetadataExt},
            net::UnixStream,
        },
    },
    path::Path,
    time::Duration,
};
use uuid::Uuid;

#[derive(Debug, Subcommand)]
pub enum DeviceCommand {
    List,
    Pending,
    Approve {
        request_id: Uuid,
        #[arg(long)]
        fingerprint: String,
    },
    Deny {
        request_id: Uuid,
    },
    /// Also removes the device from the trusted device key file.
    Revoke {
        device_id: Uuid,
    },
    /// Pin active device keys for privileged broker changes.
    Trust {
        #[arg(required_unless_present = "all_active", conflicts_with = "all_active")]
        device_id: Option<Uuid>,
        #[arg(long)]
        all_active: bool,
        /// Skip the confirmation after reviewing the fingerprints.
        #[arg(long)]
        yes: bool,
    },
}

pub fn run(command: DeviceCommand, json: bool) -> Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        bail!("local device administration requires the host owner root path");
    }
    if json && !matches!(command, DeviceCommand::List | DeviceCommand::Pending) {
        bail!("JSON mode is read-only");
    }
    match command {
        DeviceCommand::List => print(&exchange(&DeviceRequest::List {})?),
        DeviceCommand::Pending => print(&exchange(&DeviceRequest::Pending {})?),
        DeviceCommand::Approve {
            request_id,
            fingerprint,
        } => {
            let data = exchange(&DeviceRequest::Approve {
                request_id,
                fingerprint: fingerprint.clone(),
            })?;
            print(&data)?;
            trust_approved(&data, &fingerprint)
                .context("device approved but not trusted for privileged changes; run `sudo jarvis devices trust <device-id>`")
        }
        DeviceCommand::Deny { request_id } => {
            print(&exchange(&DeviceRequest::Deny { request_id })?)
        }
        DeviceCommand::Revoke { device_id } => {
            // Independent of Core: a refused or failed revocation must not
            // leave the device trusted for privileged changes.
            let untrusted = untrust(device_id).context("the trusted key entry was not removed");
            let revoked =
                exchange(&DeviceRequest::Revoke { device_id }).and_then(|data| print(&data));
            if let (Err(_), Err(error)) = (&revoked, &untrusted) {
                eprintln!("{error:#}");
            }
            revoked?;
            untrusted
        }
        DeviceCommand::Trust {
            device_id,
            all_active: _,
            yes,
        } => trust(device_id, yes),
    }
}

fn print(data: &serde_json::Value) -> Result<()> {
    // JSON escaping also makes control characters in remote device names safe
    // in ordinary terminal output. Never initialize a transient TUI here.
    println!("{}", serde_json::to_string_pretty(data)?);
    Ok(())
}

#[derive(Deserialize)]
struct ListedDevices {
    devices: Vec<ListedDevice>,
}

#[derive(Deserialize)]
struct ListedDevice {
    id: Uuid,
    name: String,
    platform: String,
    public_key: Option<String>,
}

#[derive(Deserialize)]
struct ApprovedDevice {
    device_id: Uuid,
    public_key: String,
}

fn decode_key(public_key: &str) -> Result<Vec<u8>> {
    match hex::decode(public_key) {
        Ok(key) if key.len() == 32 => Ok(key),
        _ => bail!("invalid device public key from Core"),
    }
}

fn trust_approved(data: &serde_json::Value, fingerprint: &str) -> Result<()> {
    let (device_id, key) = approved_key(data, fingerprint)?;
    update_trusted(|keys| keys.insert(device_id, &key))
}

/// Core only matched the owner-supplied fingerprint; pin exactly that key.
fn approved_key(data: &serde_json::Value, fingerprint: &str) -> Result<(Uuid, Vec<u8>)> {
    let approved =
        ApprovedDevice::deserialize(data).context("Core did not report the approved key")?;
    let key = decode_key(&approved.public_key)?;
    if trusted_keys::fingerprint(&key) != fingerprint {
        bail!("approved key does not match the checked fingerprint");
    }
    Ok((approved.device_id, key))
}

fn trust(device_id: Option<Uuid>, yes: bool) -> Result<()> {
    let listed = ListedDevices::deserialize(exchange(&DeviceRequest::List {})?)
        .context("invalid local device response")?;
    let mut selected = Vec::new();
    for device in listed.devices {
        if device_id.is_some_and(|id| id != device.id) {
            continue;
        }
        let Some(key) = device.public_key.as_deref() else {
            eprintln!("Skipping {}: no active signing key.", device.id);
            continue;
        };
        let key = decode_key(key)?;
        // Fingerprint first and names debug-escaped (control and bidi
        // characters): a remote device name cannot disguise the fingerprint.
        println!(
            "{}  fingerprint {}  {:?} ({:?})",
            device.id,
            trusted_keys::fingerprint(&key),
            device.name,
            device.platform
        );
        selected.push((device.id, key));
    }
    if selected.is_empty() {
        bail!("no matching active device with a signing key");
    }
    if !yes {
        crate::confirm(
            "Check each fingerprint against the device. Trust these keys for privileged changes?",
        )?;
    }
    update_trusted(|keys| {
        selected
            .iter()
            .try_for_each(|(id, key)| keys.insert(*id, key))
    })?;
    println!(
        "Trusted {} device key(s) in {TRUSTED_DEVICE_KEYS_PATH}.",
        selected.len()
    );
    Ok(())
}

fn untrust(device_id: Uuid) -> Result<()> {
    let _lock = crate::mutation_lock(crate::CONFIG_LOCK)?;
    let path = Path::new(TRUSTED_DEVICE_KEYS_PATH);
    if trusted_keys::load(path)?.is_some() {
        trusted_keys::update(path, 0, jarvis_gid()?, |keys| {
            keys.remove(device_id);
            Ok(())
        })?;
    }
    Ok(())
}

fn update_trusted(
    change: impl FnOnce(&mut trusted_keys::TrustedDeviceKeys) -> Result<(), trusted_keys::TrustError>,
) -> Result<()> {
    let _lock = crate::mutation_lock(crate::CONFIG_LOCK)?;
    trusted_keys::update(
        Path::new(TRUSTED_DEVICE_KEYS_PATH),
        0,
        jarvis_gid()?,
        change,
    )?;
    Ok(())
}

/// The brokers read the file as root or with group `jarvis`.
fn jarvis_gid() -> Result<u32> {
    let group = unsafe { libc::getgrnam(c"jarvis".as_ptr()) };
    if group.is_null() {
        bail!("Jarvis service group unavailable");
    }
    Ok(unsafe { (*group).gr_gid })
}

/// One request over the root-peer local socket; returns the reply's `data`.
fn exchange(request: &DeviceRequest) -> Result<serde_json::Value> {
    let mut bytes = serde_json::to_vec(request)?;
    DeviceRequest::parse(&bytes).map_err(anyhow::Error::msg)?;
    let account = unsafe { libc::getpwnam(c"jarvis".as_ptr()) };
    if account.is_null() {
        bail!("Jarvis service account unavailable");
    }
    let uid = unsafe { (*account).pw_uid };
    let parent = fs::symlink_metadata("/run/jarvis-core-admin")
        .context("local device administration unavailable; check Core and release-matched units")?;
    let socket = fs::symlink_metadata(SOCKET_PATH)?;
    if !parent.is_dir()
        || parent.uid() != uid
        || parent.mode() & 0o077 != 0
        || !socket.file_type().is_socket()
        || socket.uid() != uid
        || socket.mode() & 0o177 != 0
    {
        bail!("unsafe local device administration socket");
    }
    let mut stream = UnixStream::connect(SOCKET_PATH)?;
    let mut peer: libc::ucred = unsafe { std::mem::zeroed() };
    let mut size = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut peer as *mut libc::ucred).cast(),
            &mut size,
        )
    } != 0
        || size as usize != std::mem::size_of::<libc::ucred>()
        || peer.uid != uid
    {
        bail!("unexpected local device administration peer");
    }
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    bytes.push(b'\n');
    stream.write_all(&bytes)?;
    let mut response = Vec::new();
    BufReader::new(stream.take((RESPONSE_LIMIT + 1) as u64)).read_until(b'\n', &mut response)?;
    if response.len() > RESPONSE_LIMIT {
        bail!("local device response too large");
    }
    let mut response: serde_json::Value = serde_json::from_slice(&response)?;
    if response.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        bail!(
            "device operation refused; refresh pending requests/devices and verify your selection"
        );
    }
    response
        .get_mut("data")
        .map(serde_json::Value::take)
        .context("invalid local device response")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_pins_only_the_key_behind_the_checked_fingerprint() {
        let key = [7_u8; 32];
        let fingerprint = trusted_keys::fingerprint(&key);
        let data = |key: &[u8]| serde_json::json!({"device_id": Uuid::nil(), "public_key": hex::encode(key)});
        assert_eq!(
            approved_key(&data(&key), &fingerprint).unwrap(),
            (Uuid::nil(), key.to_vec())
        );
        assert!(approved_key(&data(&[8; 32]), &fingerprint).is_err());
        assert!(approved_key(&data(&[7; 31]), &trusted_keys::fingerprint(&[7; 31])).is_err());
        assert!(
            approved_key(&serde_json::json!({"device_id": Uuid::nil()}), &fingerprint).is_err()
        );
    }
}
