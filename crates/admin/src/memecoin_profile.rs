//! `jarvis memecoin init-profile`: write the default memecoin PAPER profile.
//!
//! The profile only feeds the paper simulation; there is no wallet, signer or
//! execution path. It is validated with the parser Core uses
//! (`jarvis_onchain::profile`), on the staged file, before it replaces
//! anything.

use std::{
    fs,
    io::{ErrorKind, Write},
    os::unix::fs::{fchown, MetadataExt, PermissionsExt},
    path::Path,
};

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use jarvis_onchain::profile::PaperProfile;

const PROFILE: &str = "/etc/jarvis/memecoin-paper.json";

#[derive(Debug, Subcommand)]
pub enum MemecoinCommand {
    /// Write the default memecoin PAPER profile to /etc/jarvis/memecoin-paper.json.
    InitProfile {
        /// Replace an existing profile.
        #[arg(long)]
        force: bool,
    },
}

pub fn run(command: MemecoinCommand) -> Result<()> {
    match command {
        MemecoinCommand::InitProfile { force } => {
            write_profile(Path::new(PROFILE), force, true)?;
            println!("jarvis: wrote {PROFILE} (paper only; validated as Core reads it).");
            println!("Next steps:");
            println!("  1. sudo jarvis-credentials set market-data");
            println!("  2. Set JARVIS_MEMECOIN_PAPER_ENABLED=true in /etc/jarvis/core.env");
            println!("  3. sudo systemctl restart jarvis-core.service");
            println!("  4. sudo jarvis logs core: look for 'memecoin PAPER trader running'");
            Ok(())
        }
    }
}

/// The owner's default: $100 paper stake, at most 5 positions of $20, pump.fun
/// discovery, strict filters, at most 10% risk per trade, and Claude deciding
/// entries and exits and proposing strategy versions within the code bounds.
/// The watchlist mints were checked on chain (SPL Token mints): BONK, WIF and
/// POPCAT.
fn default_profile() -> serde_json::Value {
    serde_json::json!({
        "watchlist": [
            "DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263",
            "EKpQGSJtjMFqKZ9KQanSqYXRcF8fBopzLHYxdM65zcjm",
            "7GCihgDB8fe6KNjn2MYtkzZcRjQy3t9GHdC8uHYmW2hr"
        ],
        "discovery": true,
        "goal_multiple": "2",
        "stake_usd": "100",
        "max_position_usd": "20",
        "max_open_positions": 5,
        "max_risk_per_trade_bps": 1000,
        "filter": {
            "min_liquidity_usd": "50000",
            "min_pool_age_secs": 3600,
            "max_top_holders_share": "0.3",
            "max_sell_impact_bps": 200,
            "allowed_extensions": ["metadata_pointer", "token_metadata"],
            "max_snapshot_age_secs": 60,
            "min_lp_lock_remaining_secs": 2592000
        },
        "costs": {
            "dex_fee_bps": 30,
            "network_fee_usd": "0.001",
            "priority_fee_usd": "0.05",
            "failure_rate": "0.1"
        },
        "agent": {
            "max_calls_per_hour": 4,
            "max_calls_per_day": 48,
            "new_strategies_enabled": true
        }
    })
}

/// Stage the default profile beside `path`, validate the staged file as Core
/// loads it, then move it into place. Without `force` an existing profile is
/// never replaced; a symlink is always refused. `production`: owned by root
/// with the directory's group, and Core's production permission check.
fn write_profile(path: &Path, force: bool, production: bool) -> Result<PaperProfile> {
    let parent = path.parent().context("profile path has no parent")?;
    match fs::symlink_metadata(path) {
        Ok(existing) if existing.file_type().is_symlink() => {
            bail!("{} is a symlink; refusing to write it", path.display())
        }
        Ok(existing) if !existing.is_file() => {
            bail!("{} is not a regular file", path.display())
        }
        Ok(_) if !force => bail!(
            "{} already exists; rerun with --force to replace it",
            path.display()
        ),
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("inspect the existing profile"),
    }
    let gid = fs::metadata(parent)
        .with_context(|| format!("inspect {}", parent.display()))?
        .gid();
    let json = serde_json::to_string_pretty(&default_profile())? + "\n";
    let mut staged = tempfile::Builder::new()
        .prefix(".memecoin-paper.")
        .tempfile_in(parent)
        .with_context(|| format!("stage a file in {}", parent.display()))?;
    if production {
        fchown(staged.as_file(), Some(0), Some(gid)).context("set the profile owner")?;
    }
    staged
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o640))?;
    staged.write_all(json.as_bytes())?;
    staged.as_file().sync_all()?;
    let profile = PaperProfile::load(staged.path(), production)
        .map_err(anyhow::Error::msg)
        .context("the default profile does not validate; nothing was written")?;
    if force {
        staged.persist(path).map_err(|error| error.error)
    } else {
        staged.persist_noclobber(path).map_err(|error| error.error)
    }
    .with_context(|| format!("move the profile to {}", path.display()))?;
    fs::File::open(parent)?.sync_all()?;
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_the_validated_default_and_never_replaces_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memecoin-paper.json");
        let profile = write_profile(&path, false, false).unwrap();
        assert_eq!(profile.watchlist.len(), 3);
        assert!(profile.discovery);
        assert_eq!(profile.stake_usd, "100".parse().unwrap());
        assert_eq!(profile.max_position_usd, "20".parse().unwrap());
        assert_eq!(profile.max_open_positions, 5);
        assert_eq!(profile.max_risk_per_trade_bps, 1_000);
        assert_eq!(profile.rules.min_liquidity_usd, "50000".parse().unwrap());
        assert_eq!(profile.rules.min_pool_age.whole_seconds(), 3_600);
        assert_eq!(profile.rules.max_top_holders_share, "0.3".parse().unwrap());
        assert!(profile.strategy.is_none());
        let agent = profile.agent.unwrap();
        assert_eq!((agent.max_calls_per_hour, agent.max_calls_per_day), (4, 48));
        assert!(agent.new_strategies_enabled);
        let metadata = fs::metadata(&path).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o640);
        // What was written is what Core reads.
        assert!(PaperProfile::load(&path, false).is_ok());

        fs::write(&path, "owner edits").unwrap();
        let error = write_profile(&path, false, false).unwrap_err();
        assert!(format!("{error:#}").contains("--force"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "owner edits");
        write_profile(&path, true, false).unwrap();
        assert!(PaperProfile::load(&path, false).is_ok());
        // No staged file is left behind.
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn refuses_a_symlink_even_with_force() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("elsewhere");
        fs::write(&target, "untouched").unwrap();
        let path = dir.path().join("memecoin-paper.json");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        for force in [false, true] {
            assert!(write_profile(&path, force, false).is_err());
        }
        assert_eq!(fs::read_to_string(&target).unwrap(), "untouched");
        assert!(fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn production_validation_runs_on_the_staged_file() {
        // Not root here: the production check refuses the staged file (or the
        // chown fails), and nothing is moved into place.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memecoin-paper.json");
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        assert!(write_profile(&path, false, true).is_err());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
