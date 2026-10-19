//! Startup loading of the protected owner persona (`Jarvis.md`).
//!
//! The file is opened without following a final symlink, must be a regular
//! file (in production also root-owned and not group- or world-writable), is
//! read up to one byte past `PERSONA_MAX_BYTES` and is checked with the same
//! validator the owner's `jarvis persona` commands use. Production refuses to
//! start on any problem; development and tests fall back to the built-in
//! persona. Logs and errors carry the path, SHA-256 and size only, never
//! persona text.

use std::{
    fs::OpenOptions,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    sync::Arc,
};

use jarvis_core::JARVIS_SYSTEM_FALLBACK;
use jarvis_privileged::persona::{validate_persona, PERSONA_MAX_BYTES};
use sha2::{Digest, Sha256};

/// The validated persona text (trimmed), the SHA-256 of the file and its size
/// in bytes. `owner`, when set, is the uid the file must belong to.
fn read_persona(path: &Path, owner: Option<u32>) -> Result<(Arc<str>, String, usize), String> {
    let file = OpenOptions::new()
        .read(true)
        // O_NONBLOCK keeps a FIFO from blocking startup before the type check.
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(path)
        .map_err(|error| match error.raw_os_error() {
            Some(libc::ELOOP) => "persona is a symbolic link or its path loops".to_string(),
            _ => format!("cannot open persona: {error}"),
        })?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect persona: {error}"))?;
    if !metadata.is_file() {
        return Err("persona is not a regular file".to_string());
    }
    if let Some(owner) = owner {
        if metadata.uid() != owner || metadata.mode() & 0o022 != 0 {
            return Err("persona is not root-owned or is writable by others".to_string());
        }
    }
    let mut bytes = Vec::new();
    file.take(PERSONA_MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read persona: {error}"))?;
    let text = validate_persona(&bytes).map_err(|error| error.to_string())?;
    Ok((
        Arc::from(text.trim()),
        hex::encode(Sha256::digest(&bytes)),
        bytes.len(),
    ))
}

/// Load the persona for Core's system prompt. In production any problem is
/// an error and Core must not start; elsewhere it is logged and the built-in
/// persona is used.
pub fn startup_persona(path: &Path, production: bool) -> Result<Arc<str>, String> {
    match read_persona(path, production.then_some(0)) {
        Ok((text, sha256, bytes)) => {
            tracing::info!(path = %path.display(), %sha256, bytes, "Jarvis persona loaded");
            Ok(text)
        }
        Err(error) if production => Err(format!(
            "protected persona {} is unusable: {error}; check it with `sudo jarvis persona status`",
            path.display()
        )),
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "persona unusable outside production; using built-in fallback persona");
            Ok(Arc::from(JARVIS_SYSTEM_FALLBACK))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const VALID: &str = "\n# Jarvis\n\nYou are a careful assistant.\n";

    fn write(bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Jarvis.md");
        std::fs::write(&path, bytes).unwrap();
        // Independent of the umask, like the installed root:jarvis 0640 file.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        (dir, path)
    }

    /// The test user, standing in for root in the production owner check.
    fn me() -> u32 {
        tempfile::tempdir()
            .unwrap()
            .path()
            .metadata()
            .unwrap()
            .uid()
    }

    /// Production checks refuse with `reason`, production startup fails and
    /// development falls back to the built-in persona.
    fn assert_rejected(path: &Path, reason: &str) {
        let error = read_persona(path, Some(me())).unwrap_err();
        assert!(error.contains(reason), "{error}");
        assert!(startup_persona(path, true).is_err());
        assert_eq!(
            startup_persona(path, false).as_deref(),
            Ok(JARVIS_SYSTEM_FALLBACK)
        );
    }

    #[test]
    fn valid_persona_loads_trimmed_with_the_file_hash_and_size() {
        let (_dir, path) = write(VALID.as_bytes());
        let trimmed = "# Jarvis\n\nYou are a careful assistant.";
        let (text, sha256, bytes) = read_persona(&path, Some(me())).unwrap();
        assert_eq!(&*text, trimmed);
        assert_eq!(sha256, hex::encode(Sha256::digest(VALID.as_bytes())));
        assert_eq!(bytes, VALID.len());
        assert_eq!(startup_persona(&path, false).as_deref(), Ok(trimmed));
        // Production additionally requires root ownership.
        assert_eq!(startup_persona(&path, true).is_ok(), me() == 0);
    }

    #[test]
    fn production_requires_an_owner_controlled_file() {
        let (_dir, path) = write(VALID.as_bytes());
        for mode in [0o660, 0o646] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            let error = read_persona(&path, Some(me())).unwrap_err();
            assert!(error.contains("writable by others"), "{error}");
            assert!(startup_persona(&path, true).is_err());
            // Development has no owner check and still loads the persona.
            assert!(startup_persona(&path, false).is_ok_and(|text| text.starts_with("# Jarvis")));
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_persona(&path, Some(me())).is_ok());
        let error = read_persona(&path, Some(me().wrapping_add(1))).unwrap_err();
        assert!(error.contains("not root-owned"), "{error}");
    }

    #[test]
    fn missing_and_empty_personas_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        assert_rejected(&dir.path().join("Jarvis.md"), "cannot open persona");
        let (_dir, path) = write(b"");
        assert_rejected(&path, "persona is empty");
    }

    #[test]
    fn size_cap_is_exact_and_large_files_are_refused() {
        let mut bytes = b"# Jarvis\n".to_vec();
        bytes.resize(PERSONA_MAX_BYTES, b'a');
        let (_dir, path) = write(&bytes);
        assert!(read_persona(&path, Some(me())).is_ok());
        bytes.push(b'a');
        let (_dir, path) = write(&bytes);
        assert_rejected(&path, "exceeds");
        bytes.resize(8 * PERSONA_MAX_BYTES, b'a');
        let (_dir, path) = write(&bytes);
        assert_rejected(&path, "exceeds");
    }

    #[test]
    fn symlinks_and_non_regular_files_are_refused() {
        let (dir, target) = write(VALID.as_bytes());
        let link = dir.path().join("link.md");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_rejected(&link, "symbolic link");
        assert_rejected(dir.path(), "not a regular file");
        // A FIFO must be refused without blocking on a writer.
        let fifo = dir.path().join("fifo.md");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(status.success());
        assert_rejected(&fifo, "not a regular file");
    }

    #[test]
    fn invalid_content_is_refused() {
        for (bytes, reason) in [
            (&b"# Jarvis\n\xff\xfe"[..], "UTF-8"),
            (b"# Jarvis\n\x1b[31m", "control"),
            (b"# Jarvis\n\0", "control"),
            (b"Jarvis\n# Jarvis", "heading"),
            (b"## Jarvis", "heading"),
        ] {
            let (_dir, path) = write(bytes);
            assert_rejected(&path, reason);
        }
    }

    #[test]
    fn token_like_content_is_refused_without_echoing_it() {
        // Built at runtime so this file holds no realistic secret literal.
        let token = format!("ghp_{}", "c4".repeat(18));
        let (_dir, path) = write(format!("# Jarvis\nUse {token} here.\n").as_bytes());
        assert_rejected(&path, "key or token");
        let error = read_persona(&path, Some(me())).unwrap_err();
        assert!(
            !error.contains(&token) && !error.contains("Use "),
            "{error}"
        );
    }
}
