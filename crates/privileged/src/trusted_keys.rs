//! Root-owned allowlist of owner device keys for privileged brokers.
//!
//! Defense in depth: a broker accepts an approval only when the device's
//! current database key also equals the key pinned for that device in a file
//! that the Core service identity cannot write. Only the root `jarvis` CLI
//! changes the file.
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
};

use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const TRUSTED_DEVICE_KEYS_PATH: &str = "/etc/jarvis/trusted-device-keys";
pub const MAX_BYTES: u64 = 64 * 1024;
/// `root:jarvis 0640`: the non-root Codex broker runs with group `jarvis`.
pub const MODE: u32 = 0o640;
const KEY_BYTES: usize = 32;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TrustError {
    #[error("invalid owner device signature")]
    Signature,
    #[error(
        "device not trusted for privileged changes; run `sudo jarvis devices trust <device-id>`"
    )]
    NotTrusted,
    #[error(
        "pinned key differs from database key; possible compromise; do not re-trust unless this device was re-paired"
    )]
    KeyChanged,
    #[error("trusted device key file rejected: {0}")]
    Invalid(String),
}

fn invalid(reason: impl Into<String>) -> TrustError {
    TrustError::Invalid(reason.into())
}

/// One `<device-id> <public-key>` per line: a lowercase hyphenated UUID and
/// the lowercase hex Ed25519 key exactly as Core stores it. Blank lines and
/// `#` comments are allowed; anything else rejects the whole file.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TrustedDeviceKeys(BTreeMap<Uuid, [u8; KEY_BYTES]>);

impl TrustedDeviceKeys {
    pub fn parse(raw: &[u8]) -> Result<Self, TrustError> {
        if raw.len() as u64 > MAX_BYTES {
            return Err(invalid("exceeds size limit"));
        }
        let text = std::str::from_utf8(raw).map_err(|_| invalid("not UTF-8"))?;
        let mut keys = BTreeMap::new();
        // Not `lines()`: a CR is malformed, not a line ending.
        for (index, line) in text.split('\n').enumerate() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let entry = parse_line(line).ok_or_else(|| {
                invalid(format!(
                    "line {}: expected `<device-id> <public-key>`",
                    index + 1
                ))
            })?;
            if keys.insert(entry.0, entry.1).is_some() {
                return Err(invalid(format!("line {}: duplicate device", index + 1)));
            }
        }
        Ok(Self(keys))
    }

    pub fn render(&self) -> String {
        let mut text = String::from(
            "# Owner device keys trusted for privileged changes: <device-id> <public-key>.\n\
             # Managed by `sudo jarvis devices trust`; manual comments are not kept.\n",
        );
        for (device_id, key) in &self.0 {
            text.push_str(&format!("{device_id} {}\n", hex::encode(key)));
        }
        text
    }

    pub fn insert(&mut self, device_id: Uuid, public_key: &[u8]) -> Result<(), TrustError> {
        let key = public_key
            .try_into()
            .map_err(|_| invalid("public key must be 32 bytes"))?;
        self.0.insert(device_id, key);
        Ok(())
    }

    pub fn remove(&mut self, device_id: Uuid) -> bool {
        self.0.remove(&device_id).is_some()
    }

    pub fn get(&self, device_id: Uuid) -> Option<&[u8; KEY_BYTES]> {
        self.0.get(&device_id)
    }

    pub fn check(&self, device_id: Uuid, public_key: &[u8]) -> Result<(), TrustError> {
        match self.0.get(&device_id) {
            Some(key) if key.as_slice() == public_key => Ok(()),
            Some(_) => Err(TrustError::KeyChanged),
            None => Err(TrustError::NotTrusted),
        }
    }
}

fn parse_line(line: &str) -> Option<(Uuid, [u8; KEY_BYTES])> {
    let (id, key) = line.split_once(' ')?;
    let device_id = Uuid::parse_str(id).ok()?;
    // Only the canonical spelling, so one device cannot hide behind another.
    if device_id.hyphenated().to_string() != id
        || key.len() != KEY_BYTES * 2
        || !key.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return None;
    }
    let mut bytes = [0; KEY_BYTES];
    hex::decode_to_slice(key, &mut bytes).ok()?;
    Some((device_id, bytes))
}

/// Same format as the pairing fingerprint: SHA-256 of the key, lowercase hex.
pub fn fingerprint(public_key: &[u8]) -> String {
    hex::encode(Sha256::digest(public_key))
}

/// Where a broker reads the pinned keys and which owner the file and its
/// directory must have. Production is always [`PinnedKeys::production`].
#[derive(Debug, Clone, Copy)]
pub struct PinnedKeys<'a> {
    pub path: &'a Path,
    pub owner_uid: u32,
}

impl PinnedKeys<'static> {
    pub fn production() -> Self {
        Self {
            path: Path::new(TRUSTED_DEVICE_KEYS_PATH),
            owner_uid: 0,
        }
    }
}

/// Verify an owner approval for a privileged broker. `public_key` must be the
/// device's current database key (`jarvis_identity::active_device_public_key`).
/// With `pinned` set, that key must also be pinned for the device in the
/// protected file; `None` is the documented rollback setting.
pub fn verify_approval(
    pinned: Option<PinnedKeys<'_>>,
    device_id: Uuid,
    public_key: &[u8],
    message: &[u8],
    signature: &[u8],
) -> Result<(), TrustError> {
    jarvis_identity::verify_signature(public_key, message, signature)
        .map_err(|_| TrustError::Signature)?;
    let Some(pinned) = pinned else {
        return Ok(());
    };
    read(pinned.path, pinned.owner_uid)?
        .ok_or(TrustError::NotTrusted)?
        .check(device_id, public_key)
}

/// Read the root-owned file. A missing file is `None`, never an error.
pub fn load(path: &Path) -> Result<Option<TrustedDeviceKeys>, TrustError> {
    read(path, 0)
}

fn read(path: &Path, owner: u32) -> Result<Option<TrustedDeviceKeys>, TrustError> {
    check_directory(path, owner)?;
    let link = match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        result => result.map_err(|_| invalid("unreadable"))?,
    };
    if !link.file_type().is_file() {
        return Err(invalid("not a regular file"));
    }
    let file = fs::File::open(path).map_err(|_| invalid("unreadable"))?;
    let meta = file.metadata().map_err(|_| invalid("unreadable"))?;
    // Validate and read the same inode that lstat saw (no symlink swap).
    if (meta.dev(), meta.ino()) != (link.dev(), link.ino())
        || meta.uid() != owner
        || meta.mode() & 0o022 != 0
    {
        return Err(invalid(
            "must be a root-owned file without group/other write",
        ));
    }
    if meta.len() > MAX_BYTES {
        return Err(invalid("exceeds size limit"));
    }
    let mut raw = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut raw)
        .map_err(|_| invalid("unreadable"))?;
    TrustedDeviceKeys::parse(&raw).map(Some)
}

fn check_directory(path: &Path, owner: u32) -> Result<(), TrustError> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("no parent directory"))?;
    let meta = fs::symlink_metadata(parent).map_err(|_| invalid("directory unreadable"))?;
    if !meta.is_dir() || meta.uid() != owner || meta.mode() & 0o022 != 0 {
        return Err(invalid("unsafe directory"));
    }
    Ok(())
}

/// Read-modify-write the file atomically as `uid:gid` [`MODE`]. Callers
/// serialize concurrent updates. An unsafe or malformed existing file is
/// never overwritten. Returns the stored set.
pub fn update(
    path: &Path,
    uid: u32,
    gid: u32,
    change: impl FnOnce(&mut TrustedDeviceKeys) -> Result<(), TrustError>,
) -> Result<TrustedDeviceKeys, TrustError> {
    let mut keys = read(path, uid)?.unwrap_or_default();
    change(&mut keys)?;
    let content = keys.render();
    if content.len() as u64 > MAX_BYTES {
        return Err(invalid("exceeds size limit"));
    }
    write_atomic(path, content.as_bytes(), uid, gid)
        .map_err(|error| invalid(format!("write failed: {error}")))?;
    Ok(keys)
}

fn write_atomic(path: &Path, content: &[u8], uid: u32, gid: u32) -> std::io::Result<()> {
    let dir = path.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
    let temp = dir.join(format!(".trusted-device-keys.{}.tmp", Uuid::now_v7()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(MODE)
        .open(&temp)?;
    let result = (|| {
        std::os::unix::fs::fchown(&file, Some(uid), Some(gid))?;
        file.set_permissions(fs::Permissions::from_mode(MODE))?;
        file.write_all(content)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        fs::File::open(dir)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    const DEVICE: Uuid = Uuid::from_u128(0x0192_abcd_0000_7000_8000_0000_0000_00ef);
    const OTHER: Uuid = Uuid::from_u128(0x0192_0000_0000_7000_8000_0000_0000_0002);

    fn line(device: Uuid, key: &[u8]) -> String {
        format!("{device} {}\n", hex::encode(key))
    }

    fn temp_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("jarvis-trusted-keys-{}", Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o750)).unwrap();
        dir
    }

    #[test]
    fn parses_entries_comments_and_blank_lines() {
        let raw = format!(
            "# owner phone\n\n{}# laptop\n{}",
            line(DEVICE, &[1; 32]),
            line(OTHER, &[2; 32]).trim_end()
        );
        let keys = TrustedDeviceKeys::parse(raw.as_bytes()).unwrap();
        assert!(keys.check(DEVICE, &[1; 32]).is_ok());
        assert!(keys.check(OTHER, &[2; 32]).is_ok());
        assert_eq!(TrustedDeviceKeys::parse(keys.render().as_bytes()), Ok(keys));
        assert_eq!(
            TrustedDeviceKeys::parse(b""),
            Ok(TrustedDeviceKeys::default())
        );
    }

    #[test]
    fn any_malformed_line_rejects_the_whole_file() {
        let key = hex::encode([0xab; 32]);
        for bad in [
            format!("{DEVICE}  {key}"),
            format!("{DEVICE} {key} extra"),
            format!(" {DEVICE} {key}"),
            format!("{DEVICE} {key} "),
            format!("{DEVICE}\t{key}"),
            format!("{DEVICE} {key}\r"),
            format!("{} {key}", DEVICE.simple()),
            format!("{} {key}", DEVICE.to_string().to_uppercase()),
            format!("{DEVICE} {}", key.to_uppercase()),
            format!("{DEVICE} {}", &key[2..]),
            format!("{DEVICE} {key}00"),
            format!("{DEVICE} {}", "zz".repeat(32)),
            format!("not-a-uuid {key}"),
            DEVICE.to_string(),
            " # indented comment".into(),
        ] {
            let raw = format!("{}{bad}\n", line(OTHER, &[2; 32]));
            assert!(
                matches!(
                    TrustedDeviceKeys::parse(raw.as_bytes()),
                    Err(TrustError::Invalid(_))
                ),
                "{bad:?}"
            );
        }
        assert!(TrustedDeviceKeys::parse(&[0xff, b'\n']).is_err());
    }

    #[test]
    fn duplicate_devices_and_oversize_files_are_rejected() {
        let raw = format!("{}{}", line(DEVICE, &[1; 32]), line(DEVICE, &[1; 32]));
        assert_eq!(
            TrustedDeviceKeys::parse(raw.as_bytes()),
            Err(invalid("line 2: duplicate device"))
        );
        let oversize = vec![b'#'; MAX_BYTES as usize + 1];
        assert!(TrustedDeviceKeys::parse(&oversize).is_err());
    }

    #[test]
    fn approval_requires_signature_and_the_exact_pinned_key() {
        let signing = SigningKey::from_bytes(&[7; 32]);
        let public_key = signing.verifying_key().to_bytes();
        let message = b"privileged approval";
        let signature = signing.sign(message).to_bytes();
        let mut keys = TrustedDeviceKeys::default();
        keys.insert(DEVICE, &public_key).unwrap();
        keys.insert(OTHER, &[3; 32]).unwrap();
        // Listed with the matching key.
        assert!(keys.check(DEVICE, &public_key).is_ok());
        // Listed with a different key (database key replaced).
        assert_eq!(keys.check(OTHER, &public_key), Err(TrustError::KeyChanged));
        // Not listed.
        assert_eq!(
            keys.check(Uuid::nil(), &public_key),
            Err(TrustError::NotTrusted)
        );
        // Missing file (its parent, `/`, is root-owned).
        let missing = Path::new("/").join(format!("jarvis-missing-{}", Uuid::new_v4()));
        let missing = Some(PinnedKeys {
            path: &missing,
            owner_uid: 0,
        });
        assert_eq!(
            verify_approval(missing, DEVICE, &public_key, message, &signature),
            Err(TrustError::NotTrusted)
        );
        // A pinned file: match, changed key, unlisted.
        let dir = temp_dir();
        let path = dir.join("trusted-device-keys");
        let owner = fs::metadata(&dir).unwrap();
        update(&path, owner.uid(), owner.gid(), |pins| {
            pins.insert(DEVICE, &public_key)?;
            pins.insert(OTHER, &[3; 32])
        })
        .unwrap();
        let file = Some(PinnedKeys {
            path: &path,
            owner_uid: owner.uid(),
        });
        assert!(verify_approval(file, DEVICE, &public_key, message, &signature).is_ok());
        let other = SigningKey::from_bytes(&[9; 32]);
        let other_key = other.verifying_key().to_bytes();
        let other_signature = other.sign(message).to_bytes();
        assert_eq!(
            verify_approval(file, OTHER, &other_key, message, &other_signature),
            Err(TrustError::KeyChanged)
        );
        assert_eq!(
            verify_approval(file, Uuid::nil(), &other_key, message, &other_signature),
            Err(TrustError::NotTrusted)
        );
        fs::remove_dir_all(dir).unwrap();
        // Disabled check: the signature alone decides.
        assert!(verify_approval(None, DEVICE, &public_key, message, &signature).is_ok());
        assert_eq!(
            verify_approval(None, DEVICE, &public_key, b"other", &signature),
            Err(TrustError::Signature)
        );
        assert_eq!(
            verify_approval(missing, DEVICE, &public_key, b"other", &signature),
            Err(TrustError::Signature)
        );
    }

    #[test]
    fn update_writes_atomically_and_refuses_unsafe_files() {
        let dir = temp_dir();
        let meta = fs::metadata(&dir).unwrap();
        let (uid, gid) = (meta.uid(), meta.gid());
        let path = dir.join("trusted-device-keys");
        // Created when missing.
        update(&path, uid, gid, |keys| keys.insert(DEVICE, &[1; 32])).unwrap();
        let written = fs::metadata(&path).unwrap();
        assert_eq!((written.uid(), written.gid()), (uid, gid));
        assert_eq!(written.mode() & 0o777, MODE);
        // Added, replaced, removed.
        update(&path, uid, gid, |keys| keys.insert(OTHER, &[2; 32])).unwrap();
        update(&path, uid, gid, |keys| keys.insert(DEVICE, &[4; 32])).unwrap();
        let keys = read(&path, uid).unwrap().unwrap();
        assert!(keys.check(DEVICE, &[4; 32]).is_ok());
        assert!(keys.check(OTHER, &[2; 32]).is_ok());
        let keys = update(&path, uid, gid, |keys| {
            assert!(keys.remove(OTHER));
            assert!(!keys.remove(OTHER));
            Ok(())
        })
        .unwrap();
        assert_eq!(read(&path, uid).unwrap(), Some(keys));
        // No staging files remain.
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        // Malformed or writable files are never overwritten.
        fs::write(&path, b"garbage\n").unwrap();
        assert!(update(&path, uid, gid, |_| Ok(())).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"garbage\n");
        fs::write(&path, line(DEVICE, &[1; 32])).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o662)).unwrap();
        assert!(read(&path, uid).is_err());
        assert!(update(&path, uid, gid, |_| Ok(())).is_err());
        // Symlinks are refused, not followed.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(read(&link, uid).is_err());
        assert!(update(&link, uid, gid, |_| Ok(())).is_err());
        // A different owner is refused (root reads in production).
        assert!(read(&path, uid.wrapping_add(1)).is_err());
        if uid != 0 {
            assert!(load(&path).is_err());
        }
        // A group-writable directory is refused.
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o770)).unwrap();
        assert!(read(&path, uid).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
