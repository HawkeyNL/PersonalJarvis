//! Fixed-path, root-only model-policy layout migration primitive.
//!
//! The installer must stop policy writers before invoking this helper and keep
//! them stopped until binaries, configuration and units have switched together.
//! Direction is selected from verified release capabilities, never by comparing
//! mtimes or by selecting whichever policy happens to exist. In particular a
//! downgrade exports the CURRENT policy, not an old permissions snapshot.
//! Selection is idempotent: an old compatibility copy never wins over the
//! layout recorded by the installer. No model is enabled by migration.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::Path,
};

use anyhow::{bail, Context};
use jarvis_llm::ModelAccessPolicy;

const LIMIT: u64 = 8 * 1024 * 1024;

fn main() -> anyhow::Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        bail!("model policy migration requires the trusted root installation path");
    }
    let args: Vec<_> = std::env::args().skip(1).collect();
    // No environment override or caller-selected path is accepted.
    let root = Path::new("/etc/jarvis");
    match args.as_slice() {
        [command] if command == "layout" => {
            let dir = directory(root, 0)?;
            lock(&dir)?;
            println!("{}", layout(root, 0)?);
            Ok(())
        }
        [command] if command == "select-directory" => select(root, true, 0),
        [command] if command == "select-legacy" => select(root, false, 0),
        [command] if command == "check-directory" => {
            directory(root, 0)?;
            if layout(root, 0)? != "directory" { bail!("policy directory is not active"); }
            let directory = directory(&root.join("model-policy"), 0)?;
            let (_, group) = policy(&root.join("model-policy/policy.json"), 0)?;
            if directory.metadata()?.gid() != group || directory.metadata()?.mode() & 0o777 != 0o750 {
                bail!("unsafe policy directory reader permissions");
            }
            Ok(())
        }
        [command] if command == "initialize" => match fs::symlink_metadata("/opt/jarvis/current") {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => initialize(root, 0),
            _ => bail!("initialization is only allowed before the first release activation"),
        },
        _ => bail!(
            "usage: jarvis-model-policy-storage layout|check-directory|select-directory|select-legacy|initialize"
        ),
    }
}

fn initialize(root: &Path, owner: u32) -> anyhow::Result<()> {
    let parent = directory(root, owner)?;
    lock(&parent)?;
    if root.join("model-policy").try_exists()? {
        bail!("policy layout already exists; initialization refused");
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(root.join("model-policy.json"))?;
    if unsafe { libc::fchown(file.as_raw_fd(), owner, parent.metadata()?.gid()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    file.write_all(b"{\"version\":1,\"models\":[]}\n")?;
    file.set_permissions(fs::Permissions::from_mode(0o640))?;
    file.sync_all()?;
    parent.sync_all()?;
    Ok(())
}

fn layout(root: &Path, owner: u32) -> anyhow::Result<String> {
    let path = root.join("model-policy/layout");
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if fs::symlink_metadata(root.join("model-policy")).is_ok() {
                bail!("policy layout marker is missing; explicit recovery required");
            }
            Ok("legacy".into())
        }
        Err(error) => Err(error.into()),
        Ok(_) => {
            directory(&root.join("model-policy"), owner)?;
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(path)?;
            let metadata = file.metadata()?;
            if !metadata.is_file()
                || metadata.uid() != owner
                || metadata.mode() & 0o022 != 0
                || metadata.nlink() != 1
                || metadata.len() > 16
            {
                bail!("unsafe policy layout marker");
            }
            let mut value = String::new();
            file.take(17).read_to_string(&mut value)?;
            match value.as_str() {
                "legacy\n" => Ok("legacy".into()),
                "directory\n" => Ok("directory".into()),
                _ => bail!("invalid policy layout marker"),
            }
        }
    }
}

fn select(root: &Path, to_directory: bool, owner: u32) -> anyhow::Result<()> {
    let parent = directory(root, owner)?;
    lock(&parent)?;
    let requested = if to_directory { "directory" } else { "legacy" };
    let current = layout(root, owner)?;
    if current == requested {
        let path = if to_directory {
            root.join("model-policy/policy.json")
        } else {
            root.join("model-policy.json")
        };
        policy(&path, owner)?;
        return Ok(());
    }
    transfer_locked(root, to_directory, owner, &parent)?;
    let managed = directory(&root.join("model-policy"), owner)?;
    lock(&managed)?;
    let marker = root.join("model-policy/layout");
    let temporary = root.join(format!("model-policy/.layout-{}", uuid::Uuid::new_v4()));
    let result = (|| -> anyhow::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        writeln!(file, "{requested}")?;
        file.sync_all()?;
        fs::rename(&temporary, marker)?;
        managed.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn directory(path: &Path, owner: u32) -> anyhow::Result<File> {
    // Check every ancestor rather than only the final component: NOFOLLOW on
    // its own would not reject an intermediate symlink.
    for ancestor in path.ancestors() {
        // Unit fixtures are rooted below /tmp; its host-owned ancestors can
        // appear as unmapped UIDs in a user namespace. Production never has
        // this exception and always checks through the filesystem root.
        #[cfg(test)]
        if ancestor == Path::new("/tmp") {
            break;
        }
        let metadata = fs::symlink_metadata(ancestor)?;
        if !metadata.is_dir()
            || (metadata.uid() != owner && metadata.uid() != 0)
            || metadata.mode() & 0o022 != 0
        {
            bail!("unsafe model policy directory ancestry");
        }
    }
    Ok(OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)?)
}

fn lock(directory: &File) -> anyhow::Result<()> {
    if unsafe { libc::flock(directory.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        bail!("model policy writer is active; migration refused");
    }
    Ok(())
}

fn policy(path: &Path, owner: u32) -> anyhow::Result<(Vec<u8>, u32)> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != owner
        || metadata.mode() & 0o022 != 0
        || metadata.nlink() != 1
        || metadata.len() > LIMIT
    {
        bail!("unsafe model policy inode");
    }
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        bail!("model policy exceeds size limit");
    }
    let parsed: ModelAccessPolicy =
        serde_json::from_slice(&bytes).context("invalid model policy; migration refused")?;
    parsed.validate().map_err(anyhow::Error::msg)?;
    // Copy original bytes, including optional metadata, without reserializing.
    Ok((bytes, metadata.gid()))
}

#[cfg(test)]
fn transfer(root: &Path, to_directory: bool, owner: u32) -> anyhow::Result<()> {
    let legacy_directory = directory(root, owner)?;
    lock(&legacy_directory)?;
    transfer_locked(root, to_directory, owner, &legacy_directory)
}

fn transfer_locked(
    root: &Path,
    to_directory: bool,
    owner: u32,
    legacy_directory: &File,
) -> anyhow::Result<()> {
    let managed = root.join("model-policy");
    let legacy_file = root.join("model-policy.json");
    let managed_file = managed.join("policy.json");
    let (source, target) = if to_directory {
        (&legacy_file, &managed_file)
    } else {
        (&managed_file, &legacy_file)
    };
    // Validate source BEFORE creating anything. No implicit empty policy on an
    // upgrade: missing or corrupt owner policy requires explicit recovery.
    let (_, group) = policy(source, owner)?;
    if to_directory {
        match fs::symlink_metadata(&managed) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                use std::os::unix::fs::DirBuilderExt;
                fs::DirBuilder::new().mode(0o750).create(&managed)?;
                let created = directory(&managed, owner)?;
                if unsafe { libc::fchown(created.as_raw_fd(), owner, group) } != 0 {
                    return Err(std::io::Error::last_os_error().into());
                }
                created.set_permissions(fs::Permissions::from_mode(0o750))?;
                legacy_directory.sync_all()?;
            }
            Err(error) => return Err(error.into()),
            Ok(_) => {}
        }
    }
    let managed_directory = directory(&managed, owner)?;
    if managed_directory.metadata()?.gid() != group {
        bail!("model policy directory reader group differs from policy");
    }
    lock(&managed_directory)?;
    // A dedicated-layout writer may have replaced the source while we were
    // acquiring its directory lock. Read the authoritative bytes only now.
    let (bytes, locked_group) = policy(source, owner)?;
    if locked_group != group {
        bail!("model policy reader group changed during migration");
    }
    // Even a preexisting destination is untrusted until checked. Never follow
    // a symlink, overwrite a special inode, or silently repair unsafe modes.
    match fs::symlink_metadata(target) {
        Ok(_) => {
            policy(target, owner)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let parent = target.parent().context("policy has no parent")?;
    let temporary = parent.join(format!(".policy-migration-{}", uuid::Uuid::new_v4()));
    let result = (|| -> anyhow::Result<()> {
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temporary)?;
        if unsafe { libc::fchown(output.as_raw_fd(), owner, group) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        output.write_all(&bytes)?;
        output.set_permissions(fs::Permissions::from_mode(0o640))?;
        output.sync_all()?;
        fs::rename(&temporary, target)?;
        if to_directory {
            managed_directory.sync_all()?;
        } else {
            legacy_directory.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    const ENABLED: &[u8] =
        br#"{"version":1,"models":[{"provider":"openai-api","model":"fixture","enabled":true}]}"#;
    const DISABLED: &[u8] =
        br#"{"version":1,"models":[{"provider":"openai-api","model":"fixture","enabled":false}]}"#;

    fn fixture() -> (tempfile::TempDir, u32) {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(root.path().join("model-policy.json"), ENABLED).unwrap();
        fs::set_permissions(
            root.path().join("model-policy.json"),
            fs::Permissions::from_mode(0o640),
        )
        .unwrap();
        (root, unsafe { libc::geteuid() })
    }

    #[test]
    fn upgrade_downgrade_upgrade_preserves_latest_denial_and_bytes() {
        let (root, owner) = fixture();
        transfer(root.path(), true, owner).unwrap();
        let managed = root.path().join("model-policy/policy.json");
        assert_eq!(fs::read(&managed).unwrap(), ENABLED);
        fs::write(&managed, DISABLED).unwrap();
        transfer(root.path(), false, owner).unwrap();
        assert_eq!(
            fs::read(root.path().join("model-policy.json")).unwrap(),
            DISABLED
        );
        transfer(root.path(), true, owner).unwrap();
        assert_eq!(fs::read(&managed).unwrap(), DISABLED);
        assert_eq!(fs::metadata(&managed).unwrap().mode() & 0o777, 0o640);
    }

    #[test]
    fn invalid_source_never_creates_a_new_layout() {
        let (root, owner) = fixture();
        fs::write(root.path().join("model-policy.json"), b"invalid").unwrap();
        assert!(transfer(root.path(), true, owner).is_err());
        assert!(!root.path().join("model-policy").exists());
    }

    #[test]
    fn directory_and_destination_symlinks_are_rejected() {
        let (root, owner) = fixture();
        symlink(root.path(), root.path().join("model-policy")).unwrap();
        assert!(transfer(root.path(), true, owner).is_err());
        fs::remove_file(root.path().join("model-policy")).unwrap();
        transfer(root.path(), true, owner).unwrap();
        let managed = root.path().join("model-policy/policy.json");
        fs::remove_file(&managed).unwrap();
        symlink(root.path().join("model-policy.json"), &managed).unwrap();
        assert!(transfer(root.path(), true, owner).is_err());
        assert_eq!(
            fs::read(root.path().join("model-policy.json")).unwrap(),
            ENABLED
        );
    }

    #[test]
    fn unsafe_source_and_active_writer_fail_closed() {
        let (root, owner) = fixture();
        let source = root.path().join("model-policy.json");
        fs::set_permissions(&source, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(transfer(root.path(), true, owner).is_err());
        fs::set_permissions(&source, fs::Permissions::from_mode(0o640)).unwrap();
        let writer = directory(root.path(), owner).unwrap();
        lock(&writer).unwrap();
        assert!(transfer(root.path(), true, owner).is_err());
        assert!(!root.path().join("model-policy").exists());
    }

    #[test]
    fn rollback_refuses_busy_managed_writer_and_preserves_legacy() {
        let (root, owner) = fixture();
        transfer(root.path(), true, owner).unwrap();
        let managed = root.path().join("model-policy");
        fs::write(managed.join("policy.json"), DISABLED).unwrap();
        let writer = directory(&managed, owner).unwrap();
        lock(&writer).unwrap();
        assert!(transfer(root.path(), false, owner).is_err());
        assert_eq!(
            fs::read(root.path().join("model-policy.json")).unwrap(),
            ENABLED
        );
    }

    #[test]
    fn oversized_and_hardlinked_sources_are_rejected() {
        let (root, owner) = fixture();
        let source = root.path().join("model-policy.json");
        fs::hard_link(&source, root.path().join("alias")).unwrap();
        assert!(transfer(root.path(), true, owner).is_err());
        fs::remove_file(root.path().join("alias")).unwrap();
        OpenOptions::new()
            .write(true)
            .open(&source)
            .unwrap()
            .set_len(LIMIT + 1)
            .unwrap();
        assert!(transfer(root.path(), true, owner).is_err());
        assert!(!root.path().join("model-policy").exists());
    }

    #[test]
    fn missing_source_never_falls_back_to_stale_other_layout() {
        let (root, owner) = fixture();
        transfer(root.path(), true, owner).unwrap();
        fs::remove_file(root.path().join("model-policy/policy.json")).unwrap();
        assert!(transfer(root.path(), false, owner).is_err());
        assert_eq!(
            fs::read(root.path().join("model-policy.json")).unwrap(),
            ENABLED
        );
    }

    #[test]
    fn malformed_destination_is_not_silently_overwritten() {
        let (root, owner) = fixture();
        transfer(root.path(), true, owner).unwrap();
        let target = root.path().join("model-policy/policy.json");
        fs::write(&target, b"broken").unwrap();
        assert!(transfer(root.path(), true, owner).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"broken");
    }

    #[test]
    fn repeated_selection_never_restores_stale_authorization() {
        let (root, owner) = fixture();
        select(root.path(), true, owner).unwrap();
        let managed = root.path().join("model-policy/policy.json");
        fs::write(&managed, DISABLED).unwrap();
        select(root.path(), true, owner).unwrap();
        assert_eq!(fs::read(&managed).unwrap(), DISABLED);
        select(root.path(), false, owner).unwrap();
        assert_eq!(
            fs::read(root.path().join("model-policy.json")).unwrap(),
            DISABLED
        );
        select(root.path(), true, owner).unwrap();
        assert_eq!(fs::read(managed).unwrap(), DISABLED);
    }

    #[test]
    fn corrupt_layout_does_not_guess_from_other_files() {
        let (root, owner) = fixture();
        select(root.path(), true, owner).unwrap();
        fs::write(root.path().join("model-policy/layout"), b"unknown\n").unwrap();
        assert!(select(root.path(), false, owner).is_err());
        assert_eq!(
            fs::read(root.path().join("model-policy.json")).unwrap(),
            ENABLED
        );
    }
}
