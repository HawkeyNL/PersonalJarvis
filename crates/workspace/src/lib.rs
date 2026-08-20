//! Validation and immutable planning for isolated engineering worktrees.
//!
//! This crate does not execute `git`, create a directory, or remove a worktree.
//! A later policy-gated executor may use [`WorktreePlan::git_args`] only after a
//! real device-signed approval has been verified immediately before execution.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use uuid::Uuid;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WorkspaceError {
    #[error("repository root is not a canonical Git checkout")]
    InvalidRepository,
    #[error("worktree parent must exist and be outside the repository")]
    InvalidParent,
    #[error("revision must be an immutable hexadecimal commit id")]
    InvalidRevision,
}

/// An immutable, validated plan for one detached engineering worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreePlan {
    repo_root: PathBuf,
    worktree_path: PathBuf,
    revision: String,
}

impl WorktreePlan {
    /// Plan a detached worktree outside the source checkout.
    pub fn new(
        repo_root: impl AsRef<Path>,
        worktree_parent: impl AsRef<Path>,
        task_id: Uuid,
        revision: impl Into<String>,
    ) -> Result<Self, WorkspaceError> {
        let repo_root = repo_root
            .as_ref()
            .canonicalize()
            .map_err(|_| WorkspaceError::InvalidRepository)?;
        if !repo_root.join(".git").exists() {
            return Err(WorkspaceError::InvalidRepository);
        }
        let worktree_parent = worktree_parent
            .as_ref()
            .canonicalize()
            .map_err(|_| WorkspaceError::InvalidParent)?;
        if worktree_parent.starts_with(&repo_root) {
            return Err(WorkspaceError::InvalidParent);
        }
        let revision = revision.into();
        if !(7..=64).contains(&revision.len())
            || !revision.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(WorkspaceError::InvalidRevision);
        }
        Ok(Self {
            worktree_path: worktree_parent.join(format!("jarvis-engineering-{task_id}")),
            repo_root,
            revision,
        })
    }

    pub fn repository(&self) -> &Path {
        &self.repo_root
    }
    pub fn path(&self) -> &Path {
        &self.worktree_path
    }
    pub fn revision(&self) -> &str {
        &self.revision
    }

    /// Exact argv for `git worktree add`; the caller must not append arguments.
    pub fn git_args(&self) -> Vec<OsString> {
        vec![
            "-C".into(),
            self.repo_root.as_os_str().to_os_string(),
            "worktree".into(),
            "add".into(),
            "--detach".into(),
            self.worktree_path.as_os_str().to_os_string(),
            self.revision.clone().into(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn repository() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .unwrap()
            .to_path_buf()
    }

    #[test]
    fn plans_only_detached_commit_worktrees_outside_the_repository() {
        let temporary = tempfile::tempdir().unwrap();
        let parent = temporary.path().canonicalize().unwrap();
        let plan = WorktreePlan::new(
            repository(),
            &parent,
            Uuid::nil(),
            "0123456789abcdef0123456789abcdef01234567",
        )
        .unwrap();
        assert!(plan.path().starts_with(parent));
        assert!(!plan.path().starts_with(plan.repository()));
        assert_eq!(plan.git_args()[0], "-C");
        assert!(plan.git_args().contains(&OsString::from("--detach")));
    }

    #[test]
    fn rejects_live_tree_parents_and_mutable_revisions() {
        let repo = repository();
        assert_eq!(
            WorktreePlan::new(&repo, &repo, Uuid::nil(), "0123456"),
            Err(WorkspaceError::InvalidParent)
        );
        let temporary = tempfile::tempdir().unwrap();
        assert_eq!(
            WorktreePlan::new(&repo, temporary.path(), Uuid::nil(), "main"),
            Err(WorkspaceError::InvalidRevision)
        );
        assert_eq!(
            WorktreePlan::new(&repo, temporary.path(), Uuid::nil(), "--upload-pack=x"),
            Err(WorkspaceError::InvalidRevision)
        );
    }
}
