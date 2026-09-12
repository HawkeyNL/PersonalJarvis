//! Fixed read-only APK signature verifier. No shell, user path or inherited
//! environment. The payload is also bound by the authenticated release manifest.
use super::{validate_directory, Result};
use std::{os::unix::fs::MetadataExt, path::Path, process::Stdio, time::Duration};
use tokio::io::AsyncReadExt;

pub async fn verify(path: &Path, expected: &str) -> Result<()> {
    // Distribution apksigner may itself be a packaged symlink. Canonicalize the
    // fixed path, then check EVERY component of that root-controlled executable.
    let executable = std::fs::canonicalize("/usr/bin/apksigner")
        .map_err(|_| "install the trusted apksigner package before syncing Android")?;
    for parent in executable.ancestors().skip(1) {
        validate_directory(parent, 0)?;
    }
    let meta = std::fs::symlink_metadata(&executable).map_err(|_| "APK verifier unavailable")?;
    if !meta.is_file() || meta.uid() != 0 || meta.mode() & 0o022 != 0 || meta.mode() & 0o111 == 0 {
        return Err("unsafe APK verifier");
    }
    let mut child = tokio::process::Command::new(executable)
        .args(["verify", "--print-certs"])
        .arg(path)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C")
        .env("JAVA_TOOL_OPTIONS", "-Xmx128m")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "cannot start APK verifier")?;
    let mut output = child
        .stdout
        .take()
        .ok_or("APK verifier output unavailable")?
        .take(65537);
    let mut bytes = Vec::new();
    let result = tokio::time::timeout(Duration::from_secs(120), async {
        output
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "cannot read APK verification")?;
        if bytes.len() > 65536 {
            return Err("APK verifier output exceeds limit");
        }
        let status = child.wait().await.map_err(|_| "cannot reap APK verifier")?;
        if !status.success() {
            return Err("Android APK signature verification failed");
        }
        check_output(&bytes, expected)
    })
    .await;
    match result {
        Ok(Ok(())) => Ok(()),
        other => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            other.unwrap_or(Err("APK verification timed out"))
        }
    }
}

fn check_output(bytes: &[u8], expected: &str) -> Result<()> {
    let text = std::str::from_utf8(bytes).map_err(|_| "invalid APK verifier response")?;
    let signers: Vec<_> = text
        .lines()
        .filter(|l| l.starts_with("Signer #") && l.contains(" certificate SHA-256 digest: "))
        .collect();
    if signers.len() != 1
        || signers[0] != format!("Signer #1 certificate SHA-256 digest: {expected}")
    {
        return Err("APK signing certificate does not match pinned owner identity");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_single_certificate_required() {
        let expected = "a".repeat(64);
        let line = format!("Signer #1 certificate SHA-256 digest: {expected}\n");
        assert!(check_output(line.as_bytes(), &expected).is_ok());
        assert!(check_output(line.repeat(2).as_bytes(), &expected).is_err());
        assert!(check_output(line.as_bytes(), &"b".repeat(64)).is_err());
    }
}
