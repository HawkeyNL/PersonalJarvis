//! Fixed, version-aware compatibility-helper execution boundary.

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AdminHelper {
    Models,
    Credentials,
}

impl AdminHelper {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Models => "jarvis-models",
            Self::Credentials => "jarvis-credentials",
        }
    }

    #[cfg(test)]
    pub(super) fn from_name(name: &str) -> Result<Self> {
        match name {
            "jarvis-models" => Ok(Self::Models),
            "jarvis-credentials" => Ok(Self::Credentials),
            _ => bail!("unsupported internal helper"),
        }
    }
}

pub(super) fn compatibility_helper(
    helper: AdminHelper,
    args: Vec<String>,
    verbose: bool,
) -> Result<()> {
    let lock = mutation_lock(CONFIG_LOCK)?;
    let _lock = lock;
    let mut command = trusted_admin_helper_command(helper)?;
    command.args(args);
    // Explicit CLI operations are one-shot commands. Inheriting/capturing the
    // normal terminal keeps their result visible and lets the credential
    // helper use /dev/tty directly without ever entering Ratatui state.
    run_command(&mut command, explicit_helper_subprocess_mode(verbose))
}

pub(super) fn explicit_helper_subprocess_mode(verbose: bool) -> SubprocessMode {
    SubprocessMode::from_verbose(verbose)
}

pub(super) fn trusted_admin_helper_command(helper: AdminHelper) -> Result<ProcessCommand> {
    let helper = resolve_admin_helper(
        Path::new(CURRENT_RELEASE),
        Path::new(RELEASES_ROOT),
        Path::new(SBIN),
        helper,
        0,
        0,
    )?;
    Ok(trusted_command(helper))
}

pub(super) fn resolve_admin_helper(
    current: &Path,
    releases: &Path,
    legacy_sbin: &Path,
    helper: AdminHelper,
    expected_uid: u32,
    expected_gid: u32,
) -> Result<PathBuf> {
    validate_owned_path(releases, expected_uid, expected_gid, false)
        .context("managed release root is unsafe")?;
    if !fs::metadata(releases)
        .context("inspect managed release root")?
        .is_dir()
    {
        bail!("managed release root is not a directory");
    }
    let canonical_releases = fs::canonicalize(releases).context("resolve managed release root")?;
    if canonical_releases != releases {
        bail!("managed release root must not traverse links");
    }
    let current_metadata = fs::symlink_metadata(current).context("inspect active release link")?;
    if !current_metadata.file_type().is_symlink()
        || current_metadata.uid() != expected_uid
        || current_metadata.gid() != expected_gid
    {
        bail!("active release link is unsafe");
    }
    let active = fs::canonicalize(current).context("resolve active release")?;
    let relative = active
        .strip_prefix(&canonical_releases)
        .context("active release is outside the managed release root")?;
    let tag = relative
        .to_str()
        .filter(|value| !value.contains('/'))
        .context("active release path is not a direct managed release")?;
    if !valid_release_tag(tag) || active.parent() != Some(canonical_releases.as_path()) {
        bail!("active release path is not a stable managed release");
    }
    validate_owned_path(&active, expected_uid, expected_gid, false)
        .context("active release directory is unsafe")?;
    if !fs::metadata(&active)
        .context("inspect active release directory")?
        .is_dir()
    {
        bail!("active release is not a directory");
    }

    let manifest = active.join("release.json");
    validate_owned_path(&manifest, expected_uid, expected_gid, false)
        .context("active release manifest is unsafe")?;
    let manifest_metadata = fs::metadata(&manifest).context("inspect active release manifest")?;
    if !manifest_metadata.is_file() {
        bail!("active release manifest is not a regular file");
    }
    if manifest_metadata.len() > 64 * 1024 {
        bail!("active release manifest is unexpectedly large");
    }
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest).context("read active release manifest")?)
            .context("parse active release manifest")?;
    if manifest.get("tag").and_then(serde_json::Value::as_str) != Some(tag) {
        bail!("active release manifest tag does not match its managed directory");
    }
    let versioned = match manifest.pointer("/tooling/admin_helpers") {
        None => false,
        Some(value) if value.as_u64() == Some(1) => true,
        Some(_) => bail!("active release declares an unsupported admin-helper capability"),
    };

    let path = if versioned {
        active.join(helper.name())
    } else {
        legacy_sbin.join(helper.name())
    };
    validate_owned_path(&path, expected_uid, expected_gid, true)
        .with_context(|| format!("trusted admin helper is unsafe: {}", helper.name()))?;
    if versioned {
        let canonical = fs::canonicalize(&path).context("resolve versioned admin helper")?;
        if canonical.parent() != Some(active.as_path())
            || canonical.file_name() != Some(OsStr::new(helper.name()))
        {
            bail!("versioned admin helper escapes the active release");
        }
    } else {
        validate_owned_path(legacy_sbin, expected_uid, expected_gid, false)
            .context("legacy helper directory is unsafe")?;
        let canonical_legacy =
            fs::canonicalize(legacy_sbin).context("resolve legacy helper directory")?;
        let canonical = fs::canonicalize(&path).context("resolve legacy admin helper")?;
        if canonical_legacy != legacy_sbin
            || canonical.parent() != Some(canonical_legacy.as_path())
            || canonical.file_name() != Some(OsStr::new(helper.name()))
        {
            bail!("legacy admin helper escapes its fixed compatibility directory");
        }
    }
    Ok(path)
}

fn validate_owned_path(
    path: &Path,
    expected_uid: u32,
    expected_gid: u32,
    executable: bool,
) -> Result<()> {
    let metadata = fs::symlink_metadata(path).context("inspect trusted path")?;
    if metadata.file_type().is_symlink()
        || metadata.uid() != expected_uid
        || metadata.gid() != expected_gid
        || metadata.permissions().mode() & 0o022 != 0
    {
        bail!("ownership, permissions, or file type is unsafe");
    }
    if executable {
        if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
            bail!("helper is not a regular executable file");
        }
    } else if !metadata.is_dir() && !metadata.is_file() {
        bail!("trusted path is not a regular file or directory");
    }
    Ok(())
}
