//! Single-use markers for owner-signed broker requests.
//!
//! Each broker keeps its markers in its own systemd `StateDirectory=`, owned
//! by the broker's service identity with mode 0700. The Core service identity
//! can neither write nor rename it, so a compromised Core cannot reset the
//! markers and replay an approval it relayed while that approval is still
//! within its TTL. Markers are never removed.
//!
//! Limitation: the Codex broker shares the `jarvis-codex` uid with the Codex
//! App Server and the Codex chat worker, so file permissions alone do not
//! separate them. The broker marks itself non-dumpable (blocking same-uid
//! ptrace and `/proc/<pid>/root`), and their units hide the broker's state
//! directory (`InaccessiblePaths=`) and run with `ProtectProc=invisible`
//! (the chat worker also with `PrivatePIDs=`). A dedicated broker uid is the
//! full fix and remains a follow-up.
use std::{
    fmt, fs, io,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

/// Why a request ID could not be recorded. Every variant denies the request.
#[derive(Debug)]
pub enum ReplayError {
    /// The request ID was already used.
    Replay,
    /// A directory is a symlink, has another owner or is group/other
    /// writable.
    UnsafeStore,
    /// The store could not be read or written.
    Io(io::Error),
}

impl fmt::Display for ReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Replay => f.write_str("approval replay rejected"),
            Self::UnsafeStore => f.write_str("unsafe broker replay directory"),
            Self::Io(error) => write!(f, "replay marker store unavailable: {error}"),
        }
    }
}

impl std::error::Error for ReplayError {}

impl From<io::Error> for ReplayError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// A broker's state directory and the identity that must own it. Production
/// owners are root (config broker) or the broker's service user (Codex).
#[derive(Debug, Clone, Copy)]
pub struct ReplayMarkers<'a> {
    pub state_dir: &'a Path,
    pub owner_uid: u32,
}

impl ReplayMarkers<'_> {
    /// Validate the state directory and its `replays` subdirectory, creating
    /// the latter 0700 when missing. Both must be real directories owned by
    /// `owner_uid` without group or other write.
    pub fn prepare(&self) -> Result<PathBuf, ReplayError> {
        check_dir(self.state_dir, self.owner_uid)?;
        let replays = self.state_dir.join("replays");
        match fs::DirBuilder::new().mode(0o700).create(&replays) {
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            result => result?,
        }
        check_dir(&replays, self.owner_uid)?;
        Ok(replays)
    }

    /// Record `request_id` as used, durably, before the caller acts on it.
    pub fn consume(&self, request_id: uuid::Uuid) -> Result<(), ReplayError> {
        let replays = self.prepare()?;
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(replays.join(request_id.to_string()))
            .map_err(|error| match error.kind() {
                io::ErrorKind::AlreadyExists => ReplayError::Replay,
                _ => ReplayError::Io(error),
            })?;
        // A crash must not forget a marker for an approval that was acted on.
        file.sync_all()?;
        fs::File::open(&replays)?.sync_all()?;
        Ok(())
    }
}

fn check_dir(path: &Path, owner_uid: u32) -> Result<(), ReplayError> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_dir() || meta.uid() != owner_uid || meta.mode() & 0o022 != 0 {
        return Err(ReplayError::UnsafeStore);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn state_dir() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("jarvis-broker-replay-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }

    fn markers(state_dir: &Path) -> ReplayMarkers<'_> {
        ReplayMarkers {
            state_dir,
            owner_uid: unsafe { libc::geteuid() },
        }
    }

    #[test]
    fn creates_a_private_replay_dir_and_consumes_each_request_once() {
        let dir = state_dir();
        let store = markers(&dir);
        let replays = store.prepare().unwrap();
        assert_eq!(replays, dir.join("replays"));
        assert_eq!(fs::metadata(&replays).unwrap().mode() & 0o777, 0o700);
        let id = uuid::Uuid::now_v7();
        store.consume(id).unwrap();
        assert!(matches!(store.consume(id), Err(ReplayError::Replay)));
        store.consume(uuid::Uuid::now_v7()).unwrap();
        let marker = fs::metadata(replays.join(id.to_string())).unwrap();
        assert_eq!(marker.mode() & 0o777, 0o600);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rejects_unsafe_directories_without_writing_a_marker() {
        let id = uuid::Uuid::now_v7();
        let unsafe_store = |dir: &Path| {
            let error = markers(dir).consume(id).unwrap_err();
            assert!(matches!(error, ReplayError::UnsafeStore), "{dir:?}");
        };
        // Missing state directory: systemd creates it, the broker never does.
        let root = state_dir();
        assert!(matches!(
            markers(&root.join("missing")).consume(id),
            Err(ReplayError::Io(_))
        ));
        assert!(!root.join("missing").exists());

        // Group- or other-writable state directory.
        for mode in [0o720, 0o702, 0o777] {
            let dir = root.join(format!("mode-{mode:o}"));
            fs::create_dir(&dir).unwrap();
            fs::set_permissions(&dir, fs::Permissions::from_mode(mode)).unwrap();
            unsafe_store(&dir);
            assert!(!dir.join("replays").exists());
        }

        // A symlinked state directory, even to a safe one.
        let real = root.join("real");
        fs::create_dir(&real).unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o700)).unwrap();
        let link = root.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        unsafe_store(&link);
        assert!(!real.join("replays").exists());

        // A replays entry that is a symlink, a file or group-writable.
        let swapped = root.join("swapped");
        fs::create_dir(&swapped).unwrap();
        fs::set_permissions(&swapped, fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(&real, swapped.join("replays")).unwrap();
        unsafe_store(&swapped);
        assert_eq!(fs::read_dir(&real).unwrap().count(), 0);
        fs::remove_file(swapped.join("replays")).unwrap();
        fs::write(swapped.join("replays"), b"").unwrap();
        unsafe_store(&swapped);
        fs::remove_file(swapped.join("replays")).unwrap();
        fs::create_dir(swapped.join("replays")).unwrap();
        fs::set_permissions(swapped.join("replays"), fs::Permissions::from_mode(0o770)).unwrap();
        unsafe_store(&swapped);
        assert_eq!(fs::read_dir(swapped.join("replays")).unwrap().count(), 0);

        // A directory owned by someone else (root, unless the test runs as root).
        if unsafe { libc::geteuid() } != 0 {
            let foreign = ReplayMarkers {
                state_dir: Path::new("/"),
                owner_uid: unsafe { libc::geteuid() },
            };
            assert!(matches!(foreign.consume(id), Err(ReplayError::UnsafeStore)));
        }
        let wrong_owner = ReplayMarkers {
            state_dir: &real,
            owner_uid: unsafe { libc::geteuid() }.wrapping_add(1),
        };
        assert!(matches!(
            wrong_owner.consume(id),
            Err(ReplayError::UnsafeStore)
        ));
        assert_eq!(fs::read_dir(&real).unwrap().count(), 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_planted_marker_symlink_is_not_followed() {
        let dir = state_dir();
        let store = markers(&dir);
        let replays = store.prepare().unwrap();
        let target = dir.join("target");
        let id = uuid::Uuid::now_v7();
        std::os::unix::fs::symlink(&target, replays.join(id.to_string())).unwrap();
        assert!(store.consume(id).is_err());
        assert!(!target.exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
