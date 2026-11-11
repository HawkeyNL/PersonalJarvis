//! The `dependency_review` job: what Dependabot knows about the allowlisted
//! repositories (see `docs/SCHEDULED_JOBS.md`).

use super::store::Finish;
use super::{RunStatus, Work, MAX_FACTS_BYTES};
use crate::roadmaps::github::{self, DependencyFacts};

/// Read the facts with the GitHub poller's token and allowlist; without
/// either, the run fails as `not_configured`.
pub(super) async fn run(work: &Work) -> Finish {
    let Some(token) = work.token.clone().filter(|_| work.github.configured()) else {
        return Finish::failed("not_configured");
    };
    let repos = work.github.repos();
    let mut facts = github::dependency_facts(token, repos).await;
    record(&mut facts)
}

/// `ok` when every repository was read, `partial` when some repository's
/// alerts were (with the first failure's code), `failed` when none were.
fn record(facts: &mut DependencyFacts) -> Finish {
    let (status, error_code) = match facts.errors.first() {
        None => (RunStatus::Ok, None),
        Some(error) if facts.alerts_read > 0 => (RunStatus::Partial, Some(error.error_code)),
        Some(error) => (RunStatus::Failed, Some(error.error_code)),
    };
    Finish {
        status,
        error_code,
        facts: facts.fit(MAX_FACTS_BYTES),
        summary: None,
        priorities: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::roadmaps::github::RepoError;

    #[tokio::test]
    async fn without_a_token_or_allowlist_the_run_fails_as_not_configured() {
        let work = Work {
            github: Arc::default(),
            token: None,
        };
        assert_eq!(run(&work).await, Finish::failed("not_configured"));
    }

    #[test]
    fn repository_failures_make_a_run_partial_or_failed() {
        let failed = |repo: &str| RepoError {
            repo: repo.into(),
            error_code: "token_needs_dependabot_alerts_read",
        };
        let mut facts = DependencyFacts {
            alerts_read: 2,
            ..DependencyFacts::default()
        };
        let ok = record(&mut facts);
        assert_eq!((ok.status, ok.error_code), (RunStatus::Ok, None));
        assert_eq!(
            ok.facts.as_deref(),
            Some(r#"{"alerts":[],"pull_requests":[],"errors":[],"truncated":false}"#)
        );
        // One repository failed: the other's alerts were read.
        facts.errors.push(failed("a/b"));
        facts.alerts_read = 1;
        let partial = record(&mut facts);
        assert_eq!(
            (partial.status, partial.error_code),
            (
                RunStatus::Partial,
                Some("token_needs_dependabot_alerts_read")
            )
        );
        // Alerts read (none open) but its pull request list failed: partial.
        facts.errors[0].error_code = "github_access_refused";
        assert_eq!(record(&mut facts).status, RunStatus::Partial);
        facts.errors.push(failed("a/c"));
        facts.alerts_read = 0;
        assert_eq!(record(&mut facts).status, RunStatus::Failed);
    }
}
