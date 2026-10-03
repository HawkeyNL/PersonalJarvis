//! Read-only repository snapshots from a root-owned allowlist and bare mirrors.
//! No public request can supply a path, Git URL, ref or command.

use std::{
    collections::HashSet,
    fs,
    io::{Cursor, Read},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{is_commit_sha, RepositoryIdentity, RepositorySnapshot, MAX_REPOSITORY_ARCHIVE_BYTES};

const REGISTRY_PATH: &str = "/etc/jarvis/codex-repositories.json";
const MIRRORS_ROOT: &str = "/var/lib/jarvis-codex-repositories";
const MAX_REGISTRY_BYTES: u64 = 64 * 1024;
const MAX_TREE_LIST_BYTES: usize = 4 * 1024 * 1024;
const MAX_TREE_ENTRIES: usize = 20_000;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SnapshotError {
    #[error("repository registry is unavailable or untrusted")]
    UntrustedRegistry,
    #[error("repository is not allowlisted")]
    UnknownRepository,
    #[error("repository mirror is unavailable or untrusted")]
    UntrustedMirror,
    #[error("requested commit is not reachable from the reviewed ref")]
    UnreviewedCommit,
    #[error("repository tree contains an unsafe entry")]
    UnsafeTree,
    #[error("repository snapshot exceeds its bound")]
    Oversized,
    #[error("repository snapshot could not be created")]
    GitFailure,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryFile {
    version: u32,
    repositories: Vec<RegistryEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryEntry {
    identity: RepositoryIdentity,
    allowed_ref: String,
}

/// Only this loader can create a production registry. The mapping is fixed to
/// root-owned paths, so even a reviewed registry entry cannot name a host path.
pub struct TrustedRepositoryRegistry {
    entries: Vec<RegistryEntry>,
    mirrors_root: PathBuf,
    expected_owner_uid: u32,
}

impl TrustedRepositoryRegistry {
    pub fn load_production() -> Result<Self, SnapshotError> {
        let registry = Path::new(REGISTRY_PATH);
        let mirrors_root = Path::new(MIRRORS_ROOT);
        verify_trusted_path(registry, 0, false).map_err(|_| SnapshotError::UntrustedRegistry)?;
        verify_trusted_path(mirrors_root, 0, true).map_err(|_| SnapshotError::UntrustedMirror)?;
        let metadata =
            fs::symlink_metadata(registry).map_err(|_| SnapshotError::UntrustedRegistry)?;
        if metadata.len() > MAX_REGISTRY_BYTES || metadata.permissions().mode() & 0o007 != 0 {
            return Err(SnapshotError::UntrustedRegistry);
        }
        let raw = fs::read(registry).map_err(|_| SnapshotError::UntrustedRegistry)?;
        Self::parse(&raw, mirrors_root.to_path_buf(), 0)
    }

    fn parse(
        raw: &[u8],
        mirrors_root: PathBuf,
        expected_owner_uid: u32,
    ) -> Result<Self, SnapshotError> {
        if raw.len() > MAX_REGISTRY_BYTES as usize {
            return Err(SnapshotError::UntrustedRegistry);
        }
        let file: RegistryFile =
            serde_json::from_slice(raw).map_err(|_| SnapshotError::UntrustedRegistry)?;
        if file.version != 1 || file.repositories.is_empty() || file.repositories.len() > 32 {
            return Err(SnapshotError::UntrustedRegistry);
        }
        let mut ids = HashSet::new();
        for entry in &file.repositories {
            if entry.identity.validate().is_err()
                || !ids.insert(entry.identity.id.as_str())
                || !safe_ref(&entry.allowed_ref)
            {
                return Err(SnapshotError::UntrustedRegistry);
            }
        }
        Ok(Self {
            entries: file.repositories,
            mirrors_root,
            expected_owner_uid,
        })
    }

    /// Resolve an exact commit from a trusted local bare mirror. Only Git
    /// object bytes are archived; the source checkout is never mounted.
    pub fn snapshot(
        &self,
        identity: &RepositoryIdentity,
        commit_sha: &str,
    ) -> Result<RepositorySnapshot, SnapshotError> {
        identity
            .validate()
            .map_err(|_| SnapshotError::UnknownRepository)?;
        if !is_commit_sha(commit_sha) {
            return Err(SnapshotError::UnreviewedCommit);
        }
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.identity == *identity)
            .ok_or(SnapshotError::UnknownRepository)?;
        let mirror = self.mirrors_root.join(format!("{}.git", identity.id));
        let meta = fs::symlink_metadata(&mirror).map_err(|_| SnapshotError::UntrustedMirror)?;
        if !meta.file_type().is_dir()
            || meta.uid() != self.expected_owner_uid
            || meta.permissions().mode() & 0o022 != 0
        {
            return Err(SnapshotError::UntrustedMirror);
        }
        let bare = git_output(&mirror, &["rev-parse", "--is-bare-repository"], 32)?;
        if bare.as_slice() != b"true\n" {
            return Err(SnapshotError::UntrustedMirror);
        }
        let resolved = git_output(
            &mirror,
            &["rev-parse", "--verify", &format!("{commit_sha}^{{commit}}")],
            96,
        )
        .map_err(|_| SnapshotError::UnreviewedCommit)?;
        if resolved.as_slice() != format!("{commit_sha}\n").as_bytes() {
            return Err(SnapshotError::UnreviewedCommit);
        }
        git_output(
            &mirror,
            &[
                "merge-base",
                "--is-ancestor",
                commit_sha,
                &entry.allowed_ref,
            ],
            1,
        )
        .map_err(|_| SnapshotError::UnreviewedCommit)?;
        let tree = git_output(
            &mirror,
            &["ls-tree", "-rzl", "--full-tree", commit_sha],
            MAX_TREE_LIST_BYTES,
        )?;
        validate_tree(&tree)?;
        let archive = git_output(
            &mirror,
            &["archive", "--format=tar", commit_sha],
            MAX_REPOSITORY_ARCHIVE_BYTES,
        )?;
        validate_archive(&archive, commit_sha)?;
        Ok(RepositorySnapshot {
            base_commit_sha: commit_sha.to_owned(),
            archive,
        })
    }
}

/// Re-parse the generated tar before it crosses into OpenSandbox. This is a
/// second check after `ls-tree`: archive metadata itself is untrusted input.
pub fn validate_archive(archive: &[u8], commit_sha: &str) -> Result<(), SnapshotError> {
    if archive.is_empty() || archive.len() > MAX_REPOSITORY_ARCHIVE_BYTES {
        return Err(SnapshotError::Oversized);
    }
    if !is_commit_sha(commit_sha) {
        return Err(SnapshotError::UnreviewedCommit);
    }
    let mut entries = tar::Archive::new(Cursor::new(archive));
    let mut count = 0usize;
    let mut total = 0usize;
    let mut saw_commit_header = false;
    for entry in entries.entries().map_err(|_| SnapshotError::UnsafeTree)? {
        let mut entry = entry.map_err(|_| SnapshotError::UnsafeTree)?;
        let kind = entry.header().entry_type();
        if kind.is_pax_global_extensions() {
            if saw_commit_header
                || count != 0
                || entry.path_bytes().as_ref() != b"pax_global_header"
            {
                return Err(SnapshotError::UnsafeTree);
            }
            if entry.size() > 128 {
                return Err(SnapshotError::UnsafeTree);
            }
            let mut data = Vec::new();
            entry
                .read_to_end(&mut data)
                .map_err(|_| SnapshotError::UnsafeTree)?;
            let text = std::str::from_utf8(&data).map_err(|_| SnapshotError::UnsafeTree)?;
            let (length, value) = text.split_once(' ').ok_or(SnapshotError::UnsafeTree)?;
            if length.parse::<usize>().ok() != Some(data.len())
                || value != format!("comment={commit_sha}\n")
            {
                return Err(SnapshotError::UnsafeTree);
            }
            saw_commit_header = true;
            continue;
        }
        if !kind.is_file() && !kind.is_dir() {
            return Err(SnapshotError::UnsafeTree);
        }
        let path_bytes = entry.path_bytes();
        let path = std::str::from_utf8(&path_bytes).map_err(|_| SnapshotError::UnsafeTree)?;
        let path = if kind.is_dir() {
            path.strip_suffix('/').unwrap_or(path)
        } else {
            path
        };
        if !safe_snapshot_path(path) {
            return Err(SnapshotError::UnsafeTree);
        }
        count += 1;
        if count > MAX_TREE_ENTRIES {
            return Err(SnapshotError::Oversized);
        }
        total = total
            .saturating_add(entry.size() as usize)
            .saturating_add(1024);
        if total > MAX_REPOSITORY_ARCHIVE_BYTES {
            return Err(SnapshotError::Oversized);
        }
    }
    if count == 0 || !saw_commit_header {
        return Err(SnapshotError::UnsafeTree);
    }
    Ok(())
}

impl RepositorySnapshot {
    pub fn sha256_hex(&self) -> String {
        hex::encode(Sha256::digest(&self.archive))
    }
}

fn safe_ref(value: &str) -> bool {
    let Some(branch) = value.strip_prefix("refs/heads/") else {
        return false;
    };
    !branch.is_empty()
        && branch.len() <= 128
        && branch.split('/').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
}

fn verify_trusted_path(path: &Path, uid: u32, directory: bool) -> Result<(), ()> {
    if !path.is_absolute() {
        return Err(());
    }
    let mut candidate = PathBuf::from("/");
    for component in path.components().skip(1) {
        candidate.push(component);
        let meta = fs::symlink_metadata(&candidate).map_err(|_| ())?;
        if meta.file_type().is_symlink()
            || meta.uid() != uid
            || meta.permissions().mode() & 0o022 != 0
        {
            return Err(());
        }
    }
    let meta = fs::symlink_metadata(path).map_err(|_| ())?;
    if meta.file_type().is_dir() != directory || (!directory && !meta.file_type().is_file()) {
        return Err(());
    }
    Ok(())
}

fn validate_tree(tree: &[u8]) -> Result<(), SnapshotError> {
    let mut count = 0usize;
    let mut total = 0usize;
    for item in tree
        .split(|byte| *byte == 0)
        .filter(|item| !item.is_empty())
    {
        count += 1;
        if count > MAX_TREE_ENTRIES {
            return Err(SnapshotError::Oversized);
        }
        let line = std::str::from_utf8(item).map_err(|_| SnapshotError::UnsafeTree)?;
        let (metadata, path) = line.split_once('\t').ok_or(SnapshotError::UnsafeTree)?;
        let fields: Vec<_> = metadata.split_ascii_whitespace().collect();
        if fields.len() != 4 || !matches!(fields[0], "100644" | "100755") || fields[1] != "blob" {
            return Err(SnapshotError::UnsafeTree);
        }
        let size: usize = fields[3].parse().map_err(|_| SnapshotError::UnsafeTree)?;
        if !safe_snapshot_path(path) {
            return Err(SnapshotError::UnsafeTree);
        }
        total = total.saturating_add(size).saturating_add(1024);
        if total > MAX_REPOSITORY_ARCHIVE_BYTES.saturating_sub(10 * 1024) {
            return Err(SnapshotError::Oversized);
        }
    }
    if count == 0 {
        return Err(SnapshotError::UnsafeTree);
    }
    Ok(())
}

pub(crate) fn safe_snapshot_path(path: &str) -> bool {
    if path.is_empty() || path.starts_with('/') || path.contains('\\') || path.contains('\0') {
        return false;
    }
    path.split('/').all(|component| {
        if component.is_empty() || matches!(component, "." | "..") {
            return false;
        }
        let lower = component.to_ascii_lowercase();
        !matches!(
            lower.as_str(),
            ".git"
                | ".ssh"
                | ".codex"
                | ".claude"
                | ".env"
                | ".npmrc"
                | ".pypirc"
                | ".netrc"
                | ".aws"
                | ".kube"
                | "secrets"
                | "credentials"
                | "auth.json"
                | "jarvis.md"
                | "id_rsa"
                | "id_ed25519"
        ) && !lower.starts_with(".env.")
            && ![".pem", ".key", ".p12", ".pfx", ".kdbx"]
                .iter()
                .any(|suffix| lower.ends_with(suffix))
            && component
                .bytes()
                .all(|byte| byte.is_ascii_graphic() && byte != b':')
    })
}

fn git_output(mirror: &Path, args: &[&str], max_bytes: usize) -> Result<Vec<u8>, SnapshotError> {
    // The registry is an optional execution gate; a corrupt mirror must not
    // hold a broker worker indefinitely. `timeout` is part of the supported
    // Ubuntu coreutils base and kills this fixed local Git invocation.
    let mut child = Command::new("/usr/bin/timeout")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", "/nonexistent")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .arg("--signal=KILL")
        .arg("30s")
        .arg("/usr/bin/git")
        .arg("-c")
        .arg("core.hooksPath=/dev/null")
        .arg("-c")
        .arg("core.fsmonitor=false")
        .arg("--git-dir")
        .arg(mirror)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| SnapshotError::GitFailure)?;
    let mut output = Vec::new();
    let read = child
        .stdout
        .take()
        .ok_or(SnapshotError::GitFailure)?
        .take((max_bytes as u64).saturating_add(1))
        .read_to_end(&mut output);
    if read.is_err() || output.len() > max_bytes {
        let _ = child.kill();
        let _ = child.wait();
        return Err(SnapshotError::Oversized);
    }
    if !child
        .wait()
        .map_err(|_| SnapshotError::GitFailure)?
        .success()
    {
        return Err(SnapshotError::GitFailure);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::fs::symlink, process::Command};

    fn fixture_git(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("/usr/bin/git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "Fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .output()
            .expect("fixture git command");
        assert!(output.status.success(), "fixture git failed: {args:?}");
        String::from_utf8(output.stdout).expect("fixture git output")
    }

    #[test]
    fn exact_reviewed_commit_produces_bounded_snapshot() {
        let fixture = tempfile::tempdir().expect("fixture directory");
        let source = fixture.path().join("source");
        let mirror = fixture.path().join("sample.git");
        fs::create_dir(&source).expect("source directory");
        fixture_git(&source, &["init", "-b", "main"]);
        fs::write(source.join("README.md"), "safe fixture\n").expect("fixture file");
        fixture_git(&source, &["add", "README.md"]);
        fixture_git(&source, &["commit", "-m", "fixture"]);
        let sha = fixture_git(&source, &["rev-parse", "HEAD"])
            .trim()
            .to_owned();
        let mirror_arg = mirror.to_str().expect("UTF-8 fixture path");
        fixture_git(&source, &["clone", "--bare", ".", mirror_arg]);
        fs::set_permissions(&mirror, fs::Permissions::from_mode(0o755))
            .expect("trusted fixture mirror mode");
        let identity = RepositoryIdentity {
            id: "sample".into(),
            owner: "Example".into(),
            name: "Repo".into(),
        };
        let raw = serde_json::json!({"version":1,"repositories":[{"identity":identity,"allowed_ref":"refs/heads/main"}]});
        let registry = TrustedRepositoryRegistry::parse(
            raw.to_string().as_bytes(),
            fixture.path().to_path_buf(),
            fs::symlink_metadata(&mirror)
                .expect("mirror metadata")
                .uid(),
        )
        .expect("fixture registry");
        let snapshot = registry
            .snapshot(&identity, &sha)
            .expect("reviewed snapshot");
        assert_eq!(snapshot.base_commit_sha, sha);
        assert!(!snapshot.archive.is_empty());
        assert_eq!(snapshot.sha256_hex().len(), 64);
        assert_eq!(
            registry.snapshot(&identity, &"a".repeat(40)).err(),
            Some(SnapshotError::UnreviewedCommit)
        );

        symlink("/etc/passwd", source.join("unsafe-link")).expect("fixture symlink");
        fixture_git(&source, &["add", "unsafe-link"]);
        fixture_git(&source, &["commit", "-m", "unsafe symlink"]);
        fixture_git(&source, &["push", mirror_arg, "main"]);
        let unsafe_sha = fixture_git(&source, &["rev-parse", "HEAD"]);
        assert_eq!(
            registry.snapshot(&identity, unsafe_sha.trim()).err(),
            Some(SnapshotError::UnsafeTree)
        );

        fs::remove_file(source.join("unsafe-link")).expect("remove fixture symlink");
        fs::write(source.join(".env"), "fixture only\n").expect("fixture secret filename");
        fixture_git(&source, &["add", "-A"]);
        fixture_git(&source, &["commit", "-m", "secret filename"]);
        fixture_git(&source, &["push", mirror_arg, "main"]);
        let secret_sha = fixture_git(&source, &["rev-parse", "HEAD"]);
        assert_eq!(
            registry.snapshot(&identity, secret_sha.trim()).err(),
            Some(SnapshotError::UnsafeTree)
        );
    }

    #[test]
    fn registry_rejects_path_and_ref_injection() {
        let identity = RepositoryIdentity {
            id: "sample".into(),
            owner: "Example".into(),
            name: "Repo".into(),
        };
        for reference in ["refs/heads/main", "refs/heads/release_1"] {
            let raw = serde_json::json!({"version":1,"repositories":[{"identity":identity,"allowed_ref":reference}]});
            assert!(TrustedRepositoryRegistry::parse(
                raw.to_string().as_bytes(),
                PathBuf::from("/tmp/fixture"),
                0
            )
            .is_ok());
        }
        for reference in [
            "refs/heads/../main",
            "refs/heads/main;id",
            "refs/tags/v1",
            "refs/heads/feature..x",
        ] {
            let raw = serde_json::json!({"version":1,"repositories":[{"identity":identity,"allowed_ref":reference}]});
            assert_eq!(
                TrustedRepositoryRegistry::parse(
                    raw.to_string().as_bytes(),
                    PathBuf::from("/tmp/fixture"),
                    0
                )
                .err(),
                Some(SnapshotError::UntrustedRegistry)
            );
        }
    }

    #[test]
    fn tree_rejects_secrets_symlinks_and_traversal() {
        assert!(validate_tree(
            b"100644 blob aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 3\tsrc/lib.rs\0"
        )
        .is_ok());
        for entry in [
            "120000 blob aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 3\tsrc/link\0",
            "100644 blob aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 3\t../escape\0",
            "100644 blob aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 3\t.env\0",
            "100644 blob aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 3\tconfig/auth.json\0",
            "160000 commit aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa -\tsubmodule\0",
        ] {
            assert_eq!(
                validate_tree(entry.as_bytes()),
                Err(SnapshotError::UnsafeTree)
            );
        }
    }
}
