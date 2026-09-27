//! Fixed, credential-free workload protocol for the disposable Codex sandbox.
//! The only reverse channel is a pair of manager-relayed files inside this
//! sandbox. No host socket, broker network address, provider URL or auth store
//! is representable here.

use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    snapshot::{safe_snapshot_path, validate_archive},
    BrokeredCodexRequest, RepositoryIdentity, SandboxRunInput, TaskEnvelope,
    ACTION_CODEX_RUN_APPROVED_TASK, MAX_REPOSITORY_ARCHIVE_BYTES, MAX_TASK_ENVELOPE_BYTES,
};
use jarvis_sandbox::CollectedArtifact;

const MAX_REQUEST_BYTES: usize = 16 * 1024;
const MAX_CAPABILITY_BYTES: usize = 256;
const MAX_RESPONSE_BYTES: usize = 640 * 1024;
const MAX_SUMMARY_BYTES: usize = 8 * 1024;
const MAX_PATCH_BYTES: usize = 512 * 1024;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RuntimeError {
    #[error("sandbox input is invalid")]
    InvalidInput,
    #[error("repository archive is invalid")]
    InvalidArchive,
    #[error("task channel is unavailable")]
    ChannelUnavailable,
    #[error("task channel response is invalid")]
    InvalidResponse,
    #[error("sandbox output could not be written")]
    OutputFailure,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityInput {
    broker_operation: String,
    capability_token: String,
}

/// One fixed operation. This file exists only while the disposable sandbox is
/// alive and is fetched by the trusted broker through OpenSandbox's authenticated
/// manager API. It is never a final run artifact or durable record.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskChannelRequest {
    pub binding: BrokeredCodexRequest,
    pub capability_token: String,
    pub task: TaskEnvelope,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskOutcome {
    Completed,
    SubscriptionUnavailable,
    PlanLimit,
    RuntimeFailure,
}

/// Only safe bounded content crosses back to the workload; no provider auth,
/// raw provider protocol, model selector or arbitrary command is allowed.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskChannelResponse {
    pub binding: BrokeredCodexRequest,
    pub outcome: TaskOutcome,
    pub summary: String,
    pub patch_diff: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxResultDocument {
    pub request_id: uuid::Uuid,
    pub run_id: uuid::Uuid,
    pub coding_session_id: uuid::Uuid,
    pub repository: RepositoryIdentity,
    pub base_commit_sha: String,
    pub outcome: TaskOutcome,
    pub summary: String,
    pub patch_present: bool,
}

/// Final outputs are untrusted even when returned by a disposable workload.
/// A successful run may return a patch, but this function never applies it.
pub fn validate_final_artifacts(
    artifacts: &[CollectedArtifact],
    expected: &SandboxRunInput,
) -> Result<SandboxResultDocument, RuntimeError> {
    if artifacts.len() != 2 {
        return Err(RuntimeError::InvalidResponse);
    }
    let result_bytes = artifacts
        .iter()
        .find(|item| item.path == "result.json")
        .ok_or(RuntimeError::InvalidResponse)?;
    let patch_bytes = artifacts
        .iter()
        .find(|item| item.path == "patch.diff")
        .ok_or(RuntimeError::InvalidResponse)?;
    if result_bytes.contents.len() > MAX_RESPONSE_BYTES
        || patch_bytes.contents.len() > MAX_PATCH_BYTES
        || result_bytes
            .contents
            .len()
            .saturating_add(patch_bytes.contents.len())
            > expected.max_output_bytes as usize
        || artifacts
            .iter()
            .any(|item| !matches!(item.path.as_str(), "result.json" | "patch.diff"))
    {
        return Err(RuntimeError::InvalidResponse);
    }
    let result: SandboxResultDocument = serde_json::from_slice(&result_bytes.contents)
        .map_err(|_| RuntimeError::InvalidResponse)?;
    if result.request_id != expected.request_id
        || result.run_id != expected.run_id
        || result.coding_session_id != expected.coding_session_id
        || result.repository != expected.repository
        || result.base_commit_sha != expected.base_commit_sha
        || result.summary.len() > MAX_SUMMARY_BYTES
        || result.patch_present != !patch_bytes.contents.is_empty()
        || (result.outcome != TaskOutcome::Completed && result.patch_present)
    {
        return Err(RuntimeError::InvalidResponse);
    }
    let patch =
        std::str::from_utf8(&patch_bytes.contents).map_err(|_| RuntimeError::InvalidResponse)?;
    validate_patch(patch)?;
    Ok(result)
}

fn validate_patch(patch: &str) -> Result<(), RuntimeError> {
    if patch.is_empty() {
        return Ok(());
    }
    if patch.len() > MAX_PATCH_BYTES || !patch.ends_with('\n') {
        return Err(RuntimeError::InvalidResponse);
    }
    let mut files = 0usize;
    let mut current_path: Option<&str> = None;
    let mut saw_old = false;
    let mut saw_new = false;
    let mut in_hunk = false;
    let mut old_remaining = 0usize;
    let mut new_remaining = 0usize;
    let mut old_is_null = false;
    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("diff --git a/") {
            if current_path.is_some()
                && (!saw_old || !saw_new || !in_hunk || old_remaining != 0 || new_remaining != 0)
            {
                return Err(RuntimeError::InvalidResponse);
            }
            let (left, right) = rest
                .split_once(" b/")
                .ok_or(RuntimeError::InvalidResponse)?;
            if left != right
                || !safe_snapshot_path(left)
                || left.bytes().any(|byte| byte.is_ascii_whitespace())
            {
                return Err(RuntimeError::InvalidResponse);
            }
            current_path = Some(left);
            files += 1;
            if files > 32 {
                return Err(RuntimeError::InvalidResponse);
            }
            saw_old = false;
            saw_new = false;
            in_hunk = false;
            old_is_null = false;
            continue;
        }
        let Some(path) = current_path else {
            return Err(RuntimeError::InvalidResponse);
        };
        if line == format!("--- a/{path}") || line == "--- /dev/null" {
            if saw_old || in_hunk {
                return Err(RuntimeError::InvalidResponse);
            }
            saw_old = true;
            old_is_null = line == "--- /dev/null";
        } else if line == format!("+++ b/{path}") || line == "+++ /dev/null" {
            if !saw_old || saw_new || in_hunk {
                return Err(RuntimeError::InvalidResponse);
            }
            saw_new = true;
            if old_is_null && line == "+++ /dev/null" {
                return Err(RuntimeError::InvalidResponse);
            }
        } else if line.starts_with("@@ -") && line.contains(" +") && line.contains(" @@") {
            if !saw_old || !saw_new || old_remaining != 0 || new_remaining != 0 {
                return Err(RuntimeError::InvalidResponse);
            }
            let header = line
                .strip_prefix("@@ -")
                .and_then(|rest| rest.split_once(" @@").map(|(range, _)| range))
                .ok_or(RuntimeError::InvalidResponse)?;
            let (old, new) = header
                .split_once(" +")
                .ok_or(RuntimeError::InvalidResponse)?;
            old_remaining = parse_hunk_count(old)?;
            new_remaining = parse_hunk_count(new)?;
            if old_remaining == 0 && new_remaining == 0 {
                return Err(RuntimeError::InvalidResponse);
            }
            in_hunk = true;
        } else if in_hunk && line.starts_with(' ') {
            old_remaining = old_remaining
                .checked_sub(1)
                .ok_or(RuntimeError::InvalidResponse)?;
            new_remaining = new_remaining
                .checked_sub(1)
                .ok_or(RuntimeError::InvalidResponse)?;
        } else if in_hunk && line.starts_with('+') {
            new_remaining = new_remaining
                .checked_sub(1)
                .ok_or(RuntimeError::InvalidResponse)?;
        } else if in_hunk && line.starts_with('-') {
            old_remaining = old_remaining
                .checked_sub(1)
                .ok_or(RuntimeError::InvalidResponse)?;
        } else if in_hunk && line == "\\ No newline at end of file" {
            // Git's marker modifies no hunk line counts.
        } else if !saw_old
            && (line.starts_with("index ")
                || line == "new file mode 100644"
                || line == "new file mode 100755"
                || line == "deleted file mode 100644"
                || line == "deleted file mode 100755")
        {
            // Git's ordinary textual metadata only. No rename, copy, mode-only,
            // binary, submodule or external-diff directives.
        } else {
            return Err(RuntimeError::InvalidResponse);
        }
    }
    if !saw_old || !saw_new || !in_hunk || old_remaining != 0 || new_remaining != 0 {
        return Err(RuntimeError::InvalidResponse);
    }
    Ok(())
}

fn parse_hunk_count(range: &str) -> Result<usize, RuntimeError> {
    let (start, count) = range.split_once(',').unwrap_or((range, "1"));
    if start.is_empty()
        || count.is_empty()
        || !start.bytes().all(|byte| byte.is_ascii_digit())
        || !count.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(RuntimeError::InvalidResponse);
    }
    let count = count
        .parse::<usize>()
        .map_err(|_| RuntimeError::InvalidResponse)?;
    if count > MAX_PATCH_BYTES {
        return Err(RuntimeError::InvalidResponse);
    }
    Ok(count)
}

pub fn run_in_workspace(root: &Path) -> Result<(), RuntimeError> {
    let input = root.join("input");
    let request: SandboxRunInput = read_json(&input.join("request.json"), MAX_REQUEST_BYTES)?;
    let context: TaskEnvelope =
        read_json(&input.join("task-context.json"), MAX_TASK_ENVELOPE_BYTES)?;
    request
        .validate_with_context(&context)
        .map_err(|_| RuntimeError::InvalidInput)?;
    let capability: CapabilityInput =
        read_json(&input.join("codex-capability.json"), MAX_CAPABILITY_BYTES)?;
    if capability.broker_operation != ACTION_CODEX_RUN_APPROVED_TASK
        || capability.capability_token.len() != 64
        || !capability
            .capability_token
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(RuntimeError::InvalidInput);
    }
    let archive = read_regular(&input.join("repository.tar"), MAX_REPOSITORY_ARCHIVE_BYTES)?;
    if hex::encode(Sha256::digest(&archive)) != request.snapshot_sha256 {
        return Err(RuntimeError::InvalidArchive);
    }
    unpack_reviewed_archive(&archive, &request.base_commit_sha, &root.join("source"))?;

    let channel = root.join("channel");
    fs::create_dir(&channel).map_err(|_| RuntimeError::ChannelUnavailable)?;
    let channel_request = TaskChannelRequest {
        binding: request.brokered_request(),
        capability_token: capability.capability_token,
        task: context,
    };
    write_new_json(
        &channel.join("task-request.json"),
        &channel_request,
        MAX_REQUEST_BYTES + MAX_TASK_ENVELOPE_BYTES + MAX_CAPABILITY_BYTES,
    )
    .map_err(|_| RuntimeError::ChannelUnavailable)?;

    let response_path = input.join("task-response.json");
    let deadline = Instant::now() + Duration::from_secs(request.timeout_secs);
    let response: TaskChannelResponse = loop {
        match read_json(&response_path, MAX_RESPONSE_BYTES) {
            Ok(response) => break response,
            Err(RuntimeError::InvalidInput) if Instant::now() < deadline => {
                if fs::symlink_metadata(&response_path)
                    .is_ok_and(|meta| !meta.file_type().is_file())
                {
                    return Err(RuntimeError::InvalidResponse);
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(_) if Instant::now() >= deadline => return Err(RuntimeError::ChannelUnavailable),
            Err(_) => return Err(RuntimeError::InvalidResponse),
        }
    };
    if response.binding != request.brokered_request()
        || response.summary.len() > MAX_SUMMARY_BYTES
        || response.patch_diff.len() > MAX_PATCH_BYTES
        || (response.outcome != TaskOutcome::Completed && !response.patch_diff.is_empty())
    {
        return Err(RuntimeError::InvalidResponse);
    }
    let result = SandboxResultDocument {
        request_id: request.request_id,
        run_id: request.run_id,
        coding_session_id: request.coding_session_id,
        repository: request.repository,
        base_commit_sha: request.base_commit_sha,
        outcome: response.outcome,
        summary: response.summary,
        patch_present: !response.patch_diff.is_empty(),
    };
    let result_bytes = serde_json::to_vec(&result).map_err(|_| RuntimeError::OutputFailure)?;
    if result_bytes.len().saturating_add(response.patch_diff.len())
        > request.max_output_bytes as usize
    {
        return Err(RuntimeError::InvalidResponse);
    }
    let artifacts = root.join("artifacts");
    fs::create_dir(&artifacts).map_err(|_| RuntimeError::OutputFailure)?;
    write_new_json(&artifacts.join("result.json"), &result, MAX_RESPONSE_BYTES)
        .map_err(|_| RuntimeError::OutputFailure)?;
    write_new(
        &artifacts.join("patch.diff"),
        response.patch_diff.as_bytes(),
    )
    .map_err(|_| RuntimeError::OutputFailure)?;
    // The short-lived capability is not retained among final artifacts.
    let _ = fs::remove_file(channel.join("task-request.json"));
    Ok(())
}

fn read_regular(path: &Path, maximum: usize) -> Result<Vec<u8>, RuntimeError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| RuntimeError::InvalidInput)?;
    let metadata = file.metadata().map_err(|_| RuntimeError::InvalidInput)?;
    if !metadata.file_type().is_file() || metadata.len() > maximum as u64 {
        return Err(RuntimeError::InvalidInput);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take((maximum + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| RuntimeError::InvalidInput)?;
    if bytes.len() > maximum {
        return Err(RuntimeError::InvalidInput);
    }
    Ok(bytes)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path, maximum: usize) -> Result<T, RuntimeError> {
    serde_json::from_slice(&read_regular(path, maximum)?).map_err(|_| RuntimeError::InvalidInput)
}

fn write_new_json<T: Serialize>(path: &Path, value: &T, maximum: usize) -> Result<(), io::Error> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    if bytes.len() > maximum {
        return Err(io::Error::other("bounded output exceeded"));
    }
    let temporary = path.with_extension(format!(
        "{}.tmp",
        path.extension()
            .and_then(|value| value.to_str())
            .unwrap_or("data")
    ));
    write_new(&temporary, &bytes)?;
    let linked = fs::hard_link(&temporary, path);
    let _ = fs::remove_file(&temporary);
    linked
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), io::Error> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn unpack_reviewed_archive(
    bytes: &[u8],
    base_commit_sha: &str,
    destination: &Path,
) -> Result<(), RuntimeError> {
    validate_archive(bytes, base_commit_sha).map_err(|_| RuntimeError::InvalidArchive)?;
    fs::create_dir(destination).map_err(|_| RuntimeError::InvalidArchive)?;
    let mut archive = tar::Archive::new(io::Cursor::new(bytes));
    for entry in archive
        .entries()
        .map_err(|_| RuntimeError::InvalidArchive)?
    {
        let mut entry = entry.map_err(|_| RuntimeError::InvalidArchive)?;
        let kind = entry.header().entry_type();
        if kind.is_pax_global_extensions() {
            continue;
        }
        let path_bytes = entry.path_bytes();
        let raw = std::str::from_utf8(&path_bytes).map_err(|_| RuntimeError::InvalidArchive)?;
        let relative = if kind.is_dir() {
            raw.trim_end_matches('/')
        } else {
            raw
        };
        if !safe_snapshot_path(relative) {
            return Err(RuntimeError::InvalidArchive);
        }
        let target = destination.join(relative);
        if kind.is_dir() {
            fs::create_dir_all(&target).map_err(|_| RuntimeError::InvalidArchive)?;
        } else if kind.is_file() {
            let parent = target.parent().ok_or(RuntimeError::InvalidArchive)?;
            fs::create_dir_all(parent).map_err(|_| RuntimeError::InvalidArchive)?;
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&target)
                .map_err(|_| RuntimeError::InvalidArchive)?;
            let copied =
                io::copy(&mut entry, &mut file).map_err(|_| RuntimeError::InvalidArchive)?;
            if copied != entry.size() {
                return Err(RuntimeError::InvalidArchive);
            }
        } else {
            return Err(RuntimeError::InvalidArchive);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BrokeredCodexOperation;
    use std::io::Cursor;
    use uuid::Uuid;

    fn fixture_archive(sha: &str, path: &str, kind: tar::EntryType) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        let pax = format!("{} comment={sha}\n", 12 + sha.len());
        let mut header = tar::Header::new_ustar();
        header.set_path("pax_global_header").unwrap();
        header.set_entry_type(tar::EntryType::XGlobalHeader);
        header.set_size(pax.len() as u64);
        header.set_cksum();
        builder.append(&header, Cursor::new(pax)).unwrap();
        let content = b"safe fixture\n";
        let mut header = tar::Header::new_ustar();
        header.set_path(path).unwrap();
        header.set_entry_type(kind);
        header.set_size(content.len() as u64);
        header.set_cksum();
        builder.append(&header, Cursor::new(content)).unwrap();
        builder.into_inner().unwrap()
    }

    #[test]
    fn fixed_runtime_relays_one_capability_and_writes_only_bounded_outputs() {
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("input");
        fs::create_dir(&input).unwrap();
        let sha = "a".repeat(40);
        let archive = fixture_archive(&sha, "README.md", tar::EntryType::Regular);
        assert_eq!(validate_archive(&archive, &sha), Ok(()));
        let repository = RepositoryIdentity {
            id: "fixture".into(),
            owner: "Example".into(),
            name: "Repo".into(),
        };
        let session_id = Uuid::now_v7();
        let request = SandboxRunInput {
            request_id: Uuid::now_v7(),
            run_id: Uuid::now_v7(),
            coding_session_id: session_id,
            repository: repository.clone(),
            base_commit_sha: sha.clone(),
            snapshot_sha256: hex::encode(Sha256::digest(&archive)),
            budget_reservation_id: Uuid::now_v7(),
            operation: BrokeredCodexOperation::RunApprovedTask,
            max_output_bytes: 4096,
            timeout_secs: 5,
        };
        let context = TaskEnvelope {
            intent: "coding".into(),
            objective: "Read the fixture".into(),
            owner_constraints: vec!["Do not deploy".into()],
            recent_deltas: Vec::new(),
            selected_facts: Vec::new(),
            repository,
            base_commit_sha: sha,
            coding_session_id: session_id,
            checkpoint: None,
            timeout_secs: 5,
            max_output_bytes: 4096,
        };
        fs::write(
            input.join("request.json"),
            serde_json::to_vec(&request).unwrap(),
        )
        .unwrap();
        fs::write(
            input.join("task-context.json"),
            serde_json::to_vec(&context).unwrap(),
        )
        .unwrap();
        fs::write(input.join("repository.tar"), archive).unwrap();
        fs::write(
            input.join("codex-capability.json"),
            serde_json::json!({"broker_operation":ACTION_CODEX_RUN_APPROVED_TASK,"capability_token":"b".repeat(64)}).to_string(),
        ).unwrap();

        let workspace = root.path().to_path_buf();
        let worker = std::thread::spawn(move || run_in_workspace(&workspace));
        let channel_path = root.path().join("channel/task-request.json");
        for _ in 0..100 {
            if channel_path.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let channel_request: TaskChannelRequest =
            serde_json::from_slice(&fs::read(&channel_path).unwrap()).unwrap();
        assert_eq!(channel_request.binding, request.brokered_request());
        assert_eq!(channel_request.task.objective, "Read the fixture");
        let response = TaskChannelResponse {
            binding: channel_request.binding,
            outcome: TaskOutcome::Completed,
            summary: "Fixture inspected".into(),
            patch_diff: String::new(),
        };
        fs::write(
            input.join("task-response.json"),
            serde_json::to_vec(&response).unwrap(),
        )
        .unwrap();
        assert_eq!(worker.join().unwrap(), Ok(()));
        let result: SandboxResultDocument =
            serde_json::from_slice(&fs::read(root.path().join("artifacts/result.json")).unwrap())
                .unwrap();
        assert_eq!(result.run_id, request.run_id);
        assert_eq!(result.summary, "Fixture inspected");
        assert!(!result.patch_present);
        assert!(!channel_path.exists());
        assert_eq!(
            fs::read(root.path().join("artifacts/patch.diff")).unwrap(),
            b""
        );
    }

    #[test]
    fn archive_validation_rejects_symlink_and_wrong_commit() {
        let sha = "a".repeat(40);
        let archive = fixture_archive(&sha, "escape", tar::EntryType::Symlink);
        assert_eq!(
            validate_archive(&archive, &sha),
            Err(crate::snapshot::SnapshotError::UnsafeTree)
        );
        let valid = fixture_archive(&sha, "README.md", tar::EntryType::Regular);
        assert_eq!(
            validate_archive(&valid, &"b".repeat(40)),
            Err(crate::snapshot::SnapshotError::UnsafeTree)
        );
    }

    #[test]
    fn fixed_input_reader_never_follows_a_symlink() {
        let root = tempfile::tempdir().unwrap();
        let actual = root.path().join("actual.json");
        fs::write(&actual, b"{}").unwrap();
        let link = root.path().join("input.json");
        std::os::unix::fs::symlink(&actual, &link).unwrap();
        assert_eq!(read_regular(&link, 1024), Err(RuntimeError::InvalidInput));
    }

    #[test]
    fn final_artifacts_reject_wrong_binding_and_protected_patch() {
        let repository = RepositoryIdentity {
            id: "fixture".into(),
            owner: "Example".into(),
            name: "Repo".into(),
        };
        let expected = SandboxRunInput {
            request_id: Uuid::now_v7(),
            run_id: Uuid::now_v7(),
            coding_session_id: Uuid::now_v7(),
            repository: repository.clone(),
            base_commit_sha: "a".repeat(40),
            snapshot_sha256: "b".repeat(64),
            budget_reservation_id: Uuid::now_v7(),
            operation: BrokeredCodexOperation::RunApprovedTask,
            max_output_bytes: 4096,
            timeout_secs: 5,
        };
        let result = SandboxResultDocument {
            request_id: expected.request_id,
            run_id: expected.run_id,
            coding_session_id: expected.coding_session_id,
            repository,
            base_commit_sha: expected.base_commit_sha.clone(),
            outcome: TaskOutcome::Completed,
            summary: "fixture".into(),
            patch_present: true,
        };
        let patch = "diff --git a/src/main.rs b/src/main.rs\nindex 1111111..2222222 100644\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let artifacts = vec![
            CollectedArtifact {
                path: "result.json".into(),
                contents: serde_json::to_vec(&result).unwrap(),
            },
            CollectedArtifact {
                path: "patch.diff".into(),
                contents: patch.as_bytes().to_vec(),
            },
        ];
        assert!(validate_final_artifacts(&artifacts, &expected).is_ok());
        let mut too_small = expected.clone();
        too_small.max_output_bytes = 1;
        assert!(matches!(
            validate_final_artifacts(&artifacts, &too_small),
            Err(RuntimeError::InvalidResponse)
        ));
        let mut altered = expected.clone();
        altered.run_id = Uuid::now_v7();
        assert!(validate_final_artifacts(&artifacts, &altered).is_err());
        let protected = patch.replace("src/main.rs", "Jarvis.md");
        let mut artifacts = artifacts;
        artifacts[1].contents = protected.into_bytes();
        assert!(validate_final_artifacts(&artifacts, &expected).is_err());
        artifacts[1].contents = patch.replace("src/main.rs", "../escape").into_bytes();
        assert!(validate_final_artifacts(&artifacts, &expected).is_err());
        artifacts[1].contents = patch.replace("@@ -1 +1 @@", "@@ -2,2 +1 @@").into_bytes();
        assert!(validate_final_artifacts(&artifacts, &expected).is_err());
        artifacts[1].contents = patch.replace("+new\n", "+new\n+extra\n").into_bytes();
        assert!(validate_final_artifacts(&artifacts, &expected).is_err());
    }
}
