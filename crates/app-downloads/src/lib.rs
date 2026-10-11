//! Retention policy for a separate public client-download archive.
//!
//! This is not the protected update-mirror or Core rollback retention policy.
//! Planning never authorizes deletion or verifies the authenticity of an artifact.
use std::collections::{BTreeMap, BTreeSet};

use semver::Version;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod mirror;

pub const MAX_CATALOG_ENTRIES: usize = 10_000;
pub const MAX_VERSION_BYTES: usize = 64;
const RECENT_PATCHES: usize = 3;
const RECENT_MAJORS: usize = 3;
const RECENT_MINORS: usize = 3;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Target {
    #[serde(rename = "linux-x86_64")]
    LinuxX86_64,
    #[serde(rename = "windows-x86_64")]
    WindowsX86_64,
    MacosArm64,
    AndroidUniversal,
    IosArm64,
}

/// Only already verified, public-installable versions belong in this inventory.
/// No path, URL, private configuration, signature key or credential is accepted.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub target: Target,
    pub version: String,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeepReason {
    RecentPatch,
    MinorLatest,
    MajorLatest,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Retained {
    pub version: String,
    pub reasons: Vec<KeepReason>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TargetPlan {
    pub target: Target,
    pub keep: Vec<Retained>,
    pub remove: Vec<String>,
}

#[derive(Debug, Eq, Error, PartialEq)]
pub enum PlanError {
    #[error("public download catalog exceeds the entry limit")]
    TooManyEntries,
    #[error("public download version must be canonical stable MAJOR.MINOR.PATCH")]
    InvalidVersion,
    #[error("public download catalog contains a duplicate target/version")]
    Duplicate,
}

fn stable_version(value: &str) -> Result<Version, PlanError> {
    if value.len() > MAX_VERSION_BYTES {
        return Err(PlanError::InvalidVersion);
    }
    let version = Version::parse(value).map_err(|_| PlanError::InvalidVersion)?;
    if !version.pre.is_empty() || !version.build.is_empty() || version.to_string() != value {
        return Err(PlanError::InvalidVersion);
    }
    Ok(version)
}

/// Compute an order-independent, per-target policy. The entire inventory is
/// validated before returning any plan. Invalid metadata must never trigger a
/// partial cleanup. The limit is enforced during iteration, not after collecting.
pub fn plan(entries: impl IntoIterator<Item = Entry>) -> Result<Vec<TargetPlan>, PlanError> {
    let mut targets: BTreeMap<Target, BTreeSet<Version>> = BTreeMap::new();
    for (index, entry) in entries.into_iter().enumerate() {
        if index >= MAX_CATALOG_ENTRIES {
            return Err(PlanError::TooManyEntries);
        }
        let version = stable_version(&entry.version)?;
        if !targets.entry(entry.target).or_default().insert(version) {
            return Err(PlanError::Duplicate);
        }
    }
    Ok(targets
        .into_iter()
        .map(|(target, versions)| target_plan(target, &versions))
        .collect())
}

/// The owner's rule: the latest three patches of the current minor, the latest
/// patch of the two previous minors in the current major and the latest
/// release of the two previous majors. Older versions are not retained.
pub fn retain(versions: &BTreeSet<Version>) -> BTreeMap<Version, KeepReason> {
    let mut retained = BTreeMap::new();
    let Some(newest) = versions.last() else {
        return retained;
    };
    let mut majors = vec![newest.major];
    let mut minors = vec![newest.minor];
    let mut patches = 0;
    // Newest first, so the first version seen of an older line is its latest.
    for version in versions.iter().rev() {
        let reason = if version.major != newest.major {
            if majors.len() == RECENT_MAJORS || majors.contains(&version.major) {
                continue;
            }
            majors.push(version.major);
            KeepReason::MajorLatest
        } else if version.minor != newest.minor {
            if minors.len() == RECENT_MINORS || minors.contains(&version.minor) {
                continue;
            }
            minors.push(version.minor);
            KeepReason::MinorLatest
        } else {
            if patches == RECENT_PATCHES {
                continue;
            }
            patches += 1;
            KeepReason::RecentPatch
        };
        retained.insert(version.clone(), reason);
    }
    retained
}

fn target_plan(target: Target, versions: &BTreeSet<Version>) -> TargetPlan {
    let retained = retain(versions);
    TargetPlan {
        target,
        remove: versions
            .iter()
            .filter(|v| !retained.contains_key(*v))
            .map(ToString::to_string)
            .collect(),
        keep: retained
            .into_iter()
            .map(|(version, reason)| Retained {
                version: version.to_string(),
                reasons: vec![reason],
            })
            .collect(),
    }
}
