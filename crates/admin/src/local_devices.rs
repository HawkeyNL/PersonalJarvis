use anyhow::{bail, Context, Result};
use clap::Subcommand;
use jarvis_privileged::{
    local_devices::{DeviceRequest, RESPONSE_LIMIT, SOCKET_PATH},
    trusted_keys::{self, TrustError, TrustedDeviceKeys, TRUSTED_DEVICE_KEYS_PATH},
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
    /// Also pins the approved key for privileged changes.
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
        /// Pin every active, not yet pinned device. Never replaces a key.
        #[arg(long)]
        all_active: bool,
        /// Replace a pinned key that differs from Core's key. Only for a
        /// re-paired device; always asks for confirmation.
        #[arg(long, conflicts_with = "all_active")]
        replace: bool,
        /// Skip the confirmation for one new device after reviewing it.
        #[arg(long, conflicts_with_all = ["all_active", "replace"])]
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
        } => approve(request_id, fingerprint),
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
            replace,
            yes,
        } => trust(device_id, replace, yes),
    }
}

pub(crate) fn print(data: &serde_json::Value) -> Result<()> {
    // JSON escaping neutralizes ASCII control characters in remote device
    // names, not Unicode bidi/format characters; `trust` shows names next to
    // fingerprints with `shown`. Never initialize a transient TUI here.
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

fn approve(request_id: Uuid, fingerprint: String) -> Result<()> {
    // Validate the pin file and hold the lock before Core activates the
    // device, so a bad file or a concurrent change cannot strand it unpinned.
    let _lock = crate::mutation_lock(crate::CONFIG_LOCK)?;
    let path = Path::new(TRUSTED_DEVICE_KEYS_PATH);
    trusted_keys::load(path)?;
    let gid = jarvis_gid()?;
    let data = exchange(&DeviceRequest::Approve {
        request_id,
        fingerprint: fingerprint.clone(),
    })?;
    print(&data)?;
    let pinned = approved_key(&data, &fingerprint).and_then(|(device_id, key)| {
        trusted_keys::update(path, 0, gid, |keys| match keys.get(device_id) {
            Some(old) if old.as_slice() != key => Err(TrustError::KeyChanged),
            _ => keys.insert(device_id, &key),
        })
        .map_err(anyhow::Error::from)
    });
    pinned.context("device approved but not trusted for privileged changes; run `sudo jarvis devices trust <device-id>`")?;
    Ok(())
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

struct Candidate {
    id: Uuid,
    name: String,
    platform: String,
    key: Vec<u8>,
    pinned: Option<[u8; 32]>,
}

/// Active devices from Core, optionally one, with their current pin.
fn candidates(
    listed: ListedDevices,
    pins: &TrustedDeviceKeys,
    device_id: Option<Uuid>,
) -> Result<Vec<Candidate>> {
    let mut seen = std::collections::HashSet::new();
    let mut selected = Vec::new();
    for device in listed.devices {
        if !seen.insert(device.id) {
            bail!("Core listed device {} twice; refusing to trust", device.id);
        }
        if device_id.is_some_and(|id| id != device.id) {
            continue;
        }
        let Some(key) = device.public_key.as_deref() else {
            eprintln!("Skipping {}: no active signing key.", device.id);
            continue;
        };
        selected.push(Candidate {
            id: device.id,
            key: decode_key(key)?,
            pinned: pins.get(device.id).copied(),
            name: device.name,
            platform: device.platform,
        });
    }
    if selected.is_empty() {
        bail!("no matching active device with a signing key");
    }
    Ok(selected)
}

/// Remote device names are capped and debug-escaped (control and bidi
/// characters) so they cannot disguise a fingerprint.
pub(crate) fn shown(value: &str) -> String {
    let capped: String = value.chars().take(64).collect();
    format!("{capped:?}")
}

fn trust(device_id: Option<Uuid>, replace: bool, yes: bool) -> Result<()> {
    let _lock = crate::mutation_lock(crate::CONFIG_LOCK)?;
    let path = Path::new(TRUSTED_DEVICE_KEYS_PATH);
    let pins = trusted_keys::load(path)?.unwrap_or_default();
    let gid = jarvis_gid()?;
    let listed = ListedDevices::deserialize(exchange(&DeviceRequest::List {})?)
        .context("invalid local device response")?;
    let mut changes = Vec::new();
    let mut replacing = false;
    for device in candidates(listed, &pins, device_id)? {
        let offered = trusted_keys::fingerprint(&device.key);
        println!(
            "{}  {} ({})\n    fingerprint {offered}",
            device.id,
            shown(&device.name),
            shown(&device.platform)
        );
        match device.pinned {
            Some(old) if old.as_slice() == device.key => {
                println!("    already trusted");
                continue;
            }
            Some(old) => {
                println!(
                    "    KEY CHANGED: pinned {} -> offered {offered}",
                    trusted_keys::fingerprint(&old)
                );
                if !replace {
                    if device_id.is_some() {
                        bail!(
                            "{}; pass --replace only if this device was re-paired",
                            TrustError::KeyChanged
                        );
                    }
                    println!("    skipped: --all-active never replaces a pinned key");
                    continue;
                }
                replacing = true;
            }
            None => {}
        }
        changes.push((device.id, device.key));
    }
    if changes.is_empty() {
        println!("Nothing to change.");
        return Ok(());
    }
    if replacing {
        crate::confirm("Replace the pinned key? Only do this for a re-paired device.")?;
    } else if !yes {
        crate::confirm(
            "Check each fingerprint against the device. Trust these keys for privileged changes?",
        )?;
    }
    trusted_keys::update(path, 0, gid, |keys| {
        changes
            .iter()
            .try_for_each(|(id, key)| keys.insert(*id, key))
    })?;
    println!(
        "Trusted {} device key(s) in {TRUSTED_DEVICE_KEYS_PATH}.",
        changes.len()
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

/// The brokers read the file as root or with group `jarvis`.
fn jarvis_gid() -> Result<u32> {
    let group = unsafe { libc::getgrnam(c"jarvis".as_ptr()) };
    if group.is_null() {
        bail!("Jarvis service group unavailable");
    }
    Ok(unsafe { (*group).gr_gid })
}

/// One request over the root-peer local socket; returns the reply's `data`.
pub(crate) fn exchange(request: &DeviceRequest) -> Result<serde_json::Value> {
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

    fn listed(devices: &[(Uuid, Option<[u8; 32]>)]) -> ListedDevices {
        ListedDevices {
            devices: devices
                .iter()
                .map(|(id, key)| ListedDevice {
                    id: *id,
                    name: "phone".into(),
                    platform: "ios".into(),
                    public_key: key.map(hex::encode),
                })
                .collect(),
        }
    }

    #[test]
    fn candidates_refuse_duplicates_and_report_current_pins() {
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let mut pins = TrustedDeviceKeys::default();
        pins.insert(a, &[1; 32]).unwrap();
        assert!(candidates(
            listed(&[(a, Some([1; 32])), (a, Some([2; 32]))]),
            &pins,
            None
        )
        .is_err());
        let all = candidates(
            listed(&[(a, Some([9; 32])), (b, Some([2; 32]))]),
            &pins,
            None,
        )
        .unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].pinned, Some([1; 32]));
        assert_eq!(all[1].pinned, None);
        let one = candidates(
            listed(&[(a, Some([1; 32])), (b, Some([2; 32]))]),
            &pins,
            Some(b),
        )
        .unwrap();
        assert_eq!(one.iter().map(|c| c.id).collect::<Vec<_>>(), [b]);
        assert!(candidates(listed(&[(a, None)]), &pins, Some(a)).is_err());
        assert!(candidates(listed(&[(a, Some([1; 32]))]), &pins, Some(b)).is_err());
    }

    #[test]
    fn shown_names_are_capped_and_escape_bidi_controls() {
        assert_eq!(shown("phone"), "\"phone\"");
        assert!(shown("a\u{202e}b").contains("\\u{202e}"));
        assert!(shown("x\u{1b}[2J").contains("\\u{1b}"));
        assert_eq!(shown(&"n".repeat(100)).len(), 66);
    }
}
