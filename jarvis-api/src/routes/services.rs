//! Owner-visible Home Node service and disk status for the Health page.
//!
//! Only a compile-time allowlist of Jarvis units is queried, through a fixed
//! `systemctl show` argv (no shell, cleared environment, bounded time). Only
//! fixed labels and states from a fixed vocabulary leave Core: never PIDs,
//! command lines, environments, paths or arbitrary units.

use std::{collections::HashMap, ffi::CString, os::unix::ffi::OsStrExt, path::Path};
use std::{process::Stdio, time::Duration};

use axum::Json;
use serde_json::{json, Value};

use crate::Authed;

const SYSTEMCTL: &str = "/usr/bin/systemctl";
const TIMEOUT: Duration = Duration::from_secs(2);
const MAX_OUTPUT_BYTES: usize = 64 * 1024;
/// Same units as the Core Admin services view.
const SERVICE_UNITS: [(&str, &str); 7] = [
    ("Core", "jarvis-core.service"),
    ("SurrealDB", "jarvis-surrealdb.service"),
    ("Config broker", "jarvis-config-broker.service"),
    ("Codex broker", "jarvis-codex-broker.service"),
    ("OpenSandbox", "jarvis-opensandbox.service"),
    ("Updater timer", "jarvis-updater.timer"),
    ("Agent updater", "jarvis-private-agent-updater.service"),
];
/// systemd's `ActiveState` vocabulary; anything else is reported as unknown.
const ACTIVE_STATES: [&str; 8] = [
    "active",
    "reloading",
    "inactive",
    "failed",
    "activating",
    "deactivating",
    "maintenance",
    "refreshing",
];
const DISKS: [(&str, &str); 2] = [("system", "/"), ("data", "/var/lib/jarvis")];

pub(crate) async fn status(_authed: Authed) -> Json<Value> {
    let output = tokio::time::timeout(TIMEOUT, systemctl_show())
        .await
        .ok()
        .flatten();
    let states = output.as_deref().map(parse_show).unwrap_or_default();
    let disks = tokio::time::timeout(
        TIMEOUT,
        tokio::task::spawn_blocking(|| {
            DISKS.map(|(label, path)| disk_value(label, Path::new(path)))
        }),
    )
    .await;
    let disks = match disks {
        Ok(Ok(disks)) => disks,
        _ => DISKS.map(|(label, _)| unknown_disk(label)),
    };
    Json(json!({
        "services": services_value(&states),
        "disks": disks,
    }))
}

async fn systemctl_show() -> Option<String> {
    let output = tokio::process::Command::new(SYSTEMCTL)
        .args([
            "show",
            "--no-pager",
            "--property=Id,LoadState,ActiveState",
            "--",
        ])
        .args(SERVICE_UNITS.map(|(_, unit)| unit))
        .env_clear()
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;
    if !output.status.success() || output.stdout.len() > MAX_OUTPUT_BYTES {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

/// Map allowlisted unit -> state. Keys and values are always `'static`
/// constants, so no byte of the subprocess output can reach a client.
fn parse_show(output: &str) -> HashMap<&'static str, &'static str> {
    let mut states = HashMap::new();
    for block in output.split("\n\n") {
        let (mut id, mut load, mut active) = (None, None, None);
        for line in block.lines() {
            match line.split_once('=') {
                Some(("Id", value)) => id = Some(value),
                Some(("LoadState", value)) => load = Some(value),
                Some(("ActiveState", value)) => active = Some(value),
                _ => {}
            }
        }
        let Some(unit) = SERVICE_UNITS
            .iter()
            .map(|(_, unit)| *unit)
            .find(|unit| Some(*unit) == id)
        else {
            continue;
        };
        let state = if load == Some("not-found") {
            "not_found"
        } else {
            ACTIVE_STATES
                .into_iter()
                .find(|state| Some(*state) == active)
                .unwrap_or("unknown")
        };
        states.insert(unit, state);
    }
    states
}

fn services_value(states: &HashMap<&'static str, &'static str>) -> Vec<Value> {
    SERVICE_UNITS
        .iter()
        .map(|(label, unit)| {
            json!({
                "label": label,
                "unit": unit,
                "state": states.get(unit).copied().unwrap_or("unknown"),
            })
        })
        .collect()
}

fn unknown_disk(label: &str) -> Value {
    json!({"label": label, "state": "unknown"})
}

/// `df`-style figures for the filesystem holding `path`; the path itself is
/// never reported.
fn disk_value(label: &str, path: &Path) -> Value {
    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
        return unknown_disk(label);
    };
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    // SAFETY: `path` is a valid NUL-terminated string and `stat` is a
    // writable buffer of the type statvfs(3) fills on success.
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return unknown_disk(label);
    }
    // SAFETY: statvfs returned 0, so the buffer is initialised.
    let stat = unsafe { stat.assume_init() };
    // The statvfs field widths differ per platform (u32 on macOS).
    #[allow(clippy::unnecessary_cast)]
    let (block, blocks, bfree, bavail) = (
        stat.f_frsize as u64,
        stat.f_blocks as u64,
        stat.f_bfree as u64,
        stat.f_bavail as u64,
    );
    let total = blocks.saturating_mul(block);
    let free = bavail.saturating_mul(block);
    let used = blocks.saturating_sub(bfree).saturating_mul(block);
    let usable = used.saturating_add(free);
    if total == 0 || usable == 0 {
        return unknown_disk(label);
    }
    json!({
        "label": label,
        "state": "ok",
        "total_bytes": total,
        "free_bytes": free,
        "used_percent": (used as f64 / usable as f64 * 1000.0).round() / 10.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_allowlisted_units_and_known_states_are_reported() {
        let output = "Id=jarvis-core.service\nLoadState=loaded\nActiveState=active\n\n\
             Id=evil.service\nLoadState=loaded\nActiveState=active\nExecStart=/opt/secret --token=abc\n\n\
             Id=jarvis-surrealdb.service\nLoadState=loaded\nActiveState=$(reboot)\nMainPID=4242\n\n\
             Id=jarvis-opensandbox.service\nLoadState=not-found\nActiveState=inactive\n\n\
             Id=jarvis-updater.timer\nLoadState=loaded\nActiveState=failed\n";
        let states = parse_show(output);
        assert_eq!(states.len(), 4);
        assert_eq!(states["jarvis-core.service"], "active");
        assert_eq!(states["jarvis-surrealdb.service"], "unknown");
        assert_eq!(states["jarvis-opensandbox.service"], "not_found");
        assert_eq!(states["jarvis-updater.timer"], "failed");

        let services = services_value(&states);
        assert_eq!(services.len(), SERVICE_UNITS.len());
        let text = Value::from(services.clone()).to_string();
        for leaked in ["evil", "/opt/secret", "token", "reboot", "4242", "MainPID"] {
            assert!(!text.contains(leaked), "{leaked}");
        }
        for (service, (label, unit)) in services.iter().zip(SERVICE_UNITS) {
            assert_eq!(service["label"], label);
            assert_eq!(service["unit"], unit);
            assert_eq!(service.as_object().unwrap().len(), 3);
        }
        // Units systemctl did not answer for stay unknown.
        assert_eq!(services[3]["state"], "unknown");
    }

    #[test]
    fn missing_systemctl_output_reports_every_unit_unknown() {
        let services = services_value(&parse_show(""));
        assert!(services.iter().all(|service| service["state"] == "unknown"));
    }

    #[test]
    fn disk_status_reports_labels_and_figures_never_paths() {
        let dir = tempfile::tempdir().unwrap();
        let disk = disk_value("data", dir.path());
        assert_eq!(disk["label"], "data");
        assert_eq!(disk["state"], "ok");
        assert!(disk["total_bytes"].as_u64().unwrap() > 0);
        let used = disk["used_percent"].as_f64().unwrap();
        assert!((0.0..=100.0).contains(&used));
        assert!(!disk.to_string().contains(dir.path().to_str().unwrap()));

        let missing = disk_value("data", &dir.path().join("missing"));
        assert_eq!(missing, json!({"label": "data", "state": "unknown"}));
    }
}
