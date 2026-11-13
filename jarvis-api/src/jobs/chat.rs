//! The runs of the chat-made kinds (`prompt`, `reminder`, `disk_check`; see
//! `docs/SCHEDULED_JOBS.md`). A run writes at most one assistant message to
//! the job's own conversation and one push; it cannot reach tools, memory,
//! the portfolio or anything else. A `prompt` run is a web search through
//! the fixed Research path only.

use std::path::Path;
use std::time::Duration;

use jiff::tz::TimeZone;
use serde_json::json;
use time::OffsetDateTime;
use uuid::Uuid;

use jarvis_llm::privacy::is_private;

use super::store::{self, Finish};
use super::{Job, RunStatus, SummarySource, MAX_SUMMARY};
use crate::notify::{JobResult, NotifyEvent};
use crate::AppState;

/// Bounds one research run.
const RESEARCH_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Most source links kept in a run's facts, and the longest one.
const MAX_SOURCES: usize = 20;
const MAX_SOURCE_CHARS: usize = 500;
/// The model a fixed message of Core's is stored under.
const NOTICE_MODEL: &str = "job";

/// The fixed notice a failed `prompt` run leaves in its conversation.
fn failure_notice(code: &str) -> &'static str {
    match code {
        "private_instruction" => {
            "This scheduled research did not run: its title or instruction may hold \
             personal or financial details. Edit the job to leave them out."
        }
        "timed_out" => {
            "This scheduled research did not finish in time. It runs again at its next time."
        }
        "not_configured" => {
            "This scheduled research did not run: Research is off or no subscription worker \
             is set up."
        }
        _ => "This scheduled research could not run this time. It runs again at its next time.",
    }
}

/// Where a job's run writes and who gets its push.
struct Target {
    user: Uuid,
    conversation: Option<Uuid>,
    title: String,
}

fn target(job: &Job, fallback_title: &str) -> Option<Target> {
    Some(Target {
        user: job.owner_id.as_deref()?.parse().ok()?,
        conversation: job
            .conversation_id
            .as_deref()
            .and_then(|id| id.parse().ok()),
        title: job
            .params
            .title
            .clone()
            .unwrap_or_else(|| fallback_title.to_owned()),
    })
}

/// Post to the job's conversation and keep a new conversation's id on the
/// job. The conversation and message ids, or `None` when nothing was stored.
async fn post(
    state: &AppState,
    job: &Job,
    to: &Target,
    content: &str,
    model: &str,
) -> Option<(Uuid, Uuid)> {
    let posted = crate::routes::chat::post_job_message(
        state,
        to.user,
        to.conversation,
        &to.title,
        content,
        model,
    )
    .await;
    let Ok((conversation, message)) = posted else {
        tracing::warn!("scheduled job: message not stored");
        return None;
    };
    if to.conversation != Some(conversation) {
        let id = conversation.to_string();
        let saved = tokio::time::timeout(
            super::DB_TIMEOUT,
            store::set_conversation(&state.db, &job.id, &id),
        )
        .await;
        if !matches!(saved, Ok(Ok(()))) {
            tracing::warn!("scheduled job: conversation not saved on the job");
        }
    }
    Some((conversation, message))
}

/// `Monday 7 December 2026` in the zone.
fn run_date(now: OffsetDateTime, tz: &TimeZone) -> String {
    super::to_timestamp(now)
        .map(|at| at.to_zoned(tz.clone()).strftime("%A %-d %B %Y").to_string())
        .unwrap_or_default()
}

/// The https links in an answer, each once.
fn sources(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for word in text.split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | '<' | '>')) {
        let url = word.trim_end_matches(['.', ',', ';', ']']);
        if url.starts_with("https://")
            && url.len() > "https://".len()
            && url.chars().count() <= MAX_SOURCE_CHARS
            && !found.iter().any(|known| known == url)
        {
            found.push(url.to_owned());
            if found.len() == MAX_SOURCES {
                break;
            }
        }
    }
    found
}

/// The first [`MAX_SUMMARY`] characters, if any are visible.
fn summary(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.chars().take(MAX_SUMMARY).collect())
}

/// A web search for the job's instruction. The privacy gate runs again
/// first (and fails closed when the holdings cannot be read), since the
/// holdings may have changed since the job was saved.
pub(super) async fn prompt(state: &AppState, job: &Job, tz: &TimeZone) -> Finish {
    let (Some(to), Some(instruction)) =
        (target(job, "Research"), job.params.instruction.as_deref())
    else {
        return Finish::failed("not_configured");
    };
    let Some(held) = crate::auto_research::held_symbols(state).await else {
        return Finish::failed("holdings_unavailable");
    };
    let code = if is_private(instruction, &held) || is_private(&to.title, &held) {
        "private_instruction"
    } else if !state.llm.can_research() || !state.model_routing.snapshot().research_allowed() {
        "not_configured"
    } else {
        let question = format!(
            "(Scheduled run, {}.) {instruction}",
            run_date(OffsetDateTime::now_utc(), tz)
        );
        let search = crate::routes::chat::job_research(state, Uuid::now_v7(), &question);
        match tokio::time::timeout(RESEARCH_TIMEOUT, search).await {
            Ok(Some((answer, model))) => {
                let Some((conversation, message)) = post(state, job, &to, &answer, &model).await
                else {
                    return Finish::failed("conversation_unavailable");
                };
                if job.params.notify != Some(false) {
                    state
                        .notifier
                        .notify_user(NotifyEvent::JobResult(JobResult::Prompt), to.user);
                }
                return finished(
                    json!({
                        "conversation_id": conversation.to_string(),
                        "message_id": message.to_string(),
                        "sources": sources(&answer),
                    }),
                    summary(&answer).map(|text| (text, SummarySource::Web)),
                );
            }
            Ok(None) => "research_unavailable",
            Err(_) => "timed_out",
        }
    };
    post(state, job, &to, failure_notice(code), NOTICE_MODEL).await;
    Finish::failed(code)
}

/// The owner's fixed text, and a push with the title. No model is involved.
pub(super) async fn reminder(state: &AppState, job: &Job) -> Finish {
    let Some(to) = target(job, "Reminder") else {
        return Finish::failed("not_configured");
    };
    let text = job.params.text.clone().unwrap_or_else(|| to.title.clone());
    let Some((conversation, message)) = post(state, job, &to, &text, NOTICE_MODEL).await else {
        return Finish::failed("conversation_unavailable");
    };
    state.notifier.notify_user(
        NotifyEvent::JobResult(JobResult::Reminder(to.title.clone())),
        to.user,
    );
    finished(
        json!({"conversation_id": conversation.to_string(), "message_id": message.to_string()}),
        summary(&text).map(|text| (text, SummarySource::Deterministic)),
    )
}

/// The disks Core reports on the Health page; a note and a push only when
/// one is at or above the threshold. Nothing on the host changes.
pub(super) async fn disk_check(state: &AppState, job: &Job) -> Finish {
    let Some(to) = target(job, "Disk check") else {
        return Finish::failed("not_configured");
    };
    let threshold = job.params.threshold_percent.unwrap_or(80);
    let disks: Vec<(&'static str, f64)> = crate::routes::services::DISKS
        .iter()
        .filter_map(|(label, path)| {
            let disk = crate::routes::services::disk_value(label, Path::new(path));
            Some((*label, disk["used_percent"].as_f64()?))
        })
        .collect();
    if disks.is_empty() {
        return Finish::failed("disk_unavailable");
    }
    let full: Vec<(&'static str, u8)> = disks
        .iter()
        .filter(|(_, used)| *used >= f64::from(threshold))
        .map(|(label, used)| (*label, used.floor().clamp(0.0, 100.0) as u8))
        .collect();
    let facts = json!({
        "threshold_percent": threshold,
        "disks": disks.iter().map(|(label, used)| json!({"label": label, "used_percent": used}))
            .collect::<Vec<_>>(),
    });
    if full.is_empty() {
        return finished(
            facts,
            Some((
                format!("Every disk is below {threshold}%."),
                SummarySource::Deterministic,
            )),
        );
    }
    let text = full
        .iter()
        .map(|(label, percent)| format!("Disk {label} is at {percent}% (threshold {threshold}%)."))
        .collect::<Vec<_>>()
        .join("\n");
    if post(state, job, &to, &text, NOTICE_MODEL).await.is_none() {
        return Finish::failed("conversation_unavailable");
    }
    for (label, percent) in &full {
        state.notifier.notify_user(
            NotifyEvent::JobResult(JobResult::Disk {
                label,
                percent: *percent,
            }),
            to.user,
        );
    }
    finished(facts, Some((text, SummarySource::Deterministic)))
}

fn finished(facts: serde_json::Value, summary: Option<(String, SummarySource)>) -> Finish {
    Finish {
        status: RunStatus::Ok,
        error_code: None,
        facts: Some(facts.to_string()),
        summary,
        priorities: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_keep_https_links_once_and_bounded() {
        let text = "See https://a.example/x. and (https://b.example/y) or \
                    http://plain.example and https://a.example/x again; https://";
        assert_eq!(
            sources(text),
            ["https://a.example/x", "https://b.example/y"]
        );
        let many = (0..30)
            .map(|i| format!("https://e.example/{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(sources(&many).len(), MAX_SOURCES);
        let long = format!("https://e.example/{}", "a".repeat(MAX_SOURCE_CHARS));
        assert!(sources(&long).is_empty());
    }

    #[test]
    fn summaries_and_dates_are_bounded_and_local() {
        assert_eq!(summary("  "), None);
        assert_eq!(
            summary(&"é".repeat(MAX_SUMMARY + 5)).map(|s| s.chars().count()),
            Some(MAX_SUMMARY)
        );
        let tz = super::super::time_zone().expect("zone");
        let at = OffsetDateTime::parse(
            "2026-12-06T23:30:00Z",
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        assert_eq!(run_date(at, &tz), "Monday 7 December 2026");
    }

    #[test]
    fn failure_notices_are_fixed() {
        for code in [
            "private_instruction",
            "timed_out",
            "not_configured",
            "research_unavailable",
        ] {
            assert!(!failure_notice(code).is_empty());
        }
    }
}
