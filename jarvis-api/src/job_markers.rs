//! Scheduled jobs from a chat answer (see `docs/SCHEDULED_JOBS.md`). An
//! ordinary runs-API answer may end with up to [`MAX_PROPOSALS`] lines
//! `[[job {json}]]` the owner confirms in the app, and one line
//! `[[job_self {json}]]`: a research job Jarvis makes for itself. Core takes
//! every trailing marker line off every answer, valid or not; a marker
//! anywhere else is ordinary text. The JSON is model output that web text in
//! the conversation may have steered, so it is parsed strictly, checked like
//! a `POST /v1/jobs` body and never makes an owner's job by itself.

use jiff::tz::TimeZone;
use serde::Deserialize;
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use jarvis_client_core::realtime::JobProposal;

use crate::auto_research::{last_line, take_marker};
use crate::jobs::store;
use crate::jobs::{Cadence, Job, Kind, NewJob, Origin, Params, DB_TIMEOUT, MAX_OWNER_JOBS};
use crate::notify::{JobResult, NotifyEvent};
use crate::AppState;

/// Owner proposals honoured per answer.
pub(crate) const MAX_PROPOSALS: usize = 3;
const JOB: &str = "[[job ";
const JOB_SELF: &str = "[[job_self ";
/// A marker line longer than a job route's body is refused.
const MAX_LINE_BYTES: usize = crate::routes::jobs::BODY_LIMIT;
const WEEKDAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

/// The trailing marker lines of an answer.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Markers {
    /// A `[[web_search]]` line (see `auto_research`).
    pub web_search: bool,
    /// The JSON of each `[[job …]]` line, in reply order.
    pub jobs: Vec<String>,
    /// The JSON of each `[[job_self …]]` line, in reply order.
    pub job_self: Vec<String>,
    /// Any job marker line was taken, valid or not.
    pub any_job: bool,
}

/// Takes the trailing marker lines off `text`: `[[web_search]]`,
/// `[[job …]]` and `[[job_self …]]` in any order, and any other trailing line
/// starting with `[[job` (dropped). The text before them stays.
pub(crate) fn take(text: &mut String) -> Markers {
    let mut markers = Markers::default();
    loop {
        if take_marker(text) {
            markers.web_search = true;
            continue;
        }
        let (keep, line) = last_line(text);
        if !line.starts_with("[[job") {
            break;
        }
        markers.any_job = true;
        let json = |prefix| line.strip_prefix(prefix)?.strip_suffix("]]");
        if let Some(json) = json(JOB_SELF) {
            markers.job_self.push(json.to_owned());
        } else if let Some(json) = json(JOB) {
            markers.jobs.push(json.to_owned());
        } else {
            tracing::info!("malformed job marker dropped");
        }
        text.truncate(keep);
    }
    markers.jobs.reverse();
    markers.job_self.reverse();
    markers
}

/// A job as the model writes it: weekday by name, time as `HH:MM`, params
/// flat. Unknown fields are refused.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    kind: Kind,
    title: Option<String>,
    instruction: Option<String>,
    text: Option<String>,
    threshold_percent: Option<u8>,
    notify: Option<bool>,
    cadence: Option<Cadence>,
    weekday: Option<String>,
    month_day: Option<u8>,
    every: Option<u16>,
    start_date: Option<String>,
    time: Option<String>,
    max_runs: Option<u16>,
}

/// `HH:MM` as minutes after midnight.
fn at_minute(time: &str) -> Option<u16> {
    let (hour, minute) = time.split_once(':')?;
    let two = |part: &str| {
        (part.len() == 2 && part.bytes().all(|b| b.is_ascii_digit()))
            .then(|| part.parse::<u16>().ok())
            .flatten()
    };
    let (hour, minute) = (two(hour)?, two(minute)?);
    (hour < 24 && minute < 60).then_some(hour * 60 + minute)
}

/// The exact `POST /v1/jobs` body a marker's JSON stands for and its job
/// input, checked like that route (schedule, params, the privacy gate
/// against `held`) at `now`. `Err` names the rule it broke, for the log.
fn proposal(
    json: &str,
    now: OffsetDateTime,
    tz: Option<&TimeZone>,
    held: &[String],
) -> Result<(Value, NewJob), &'static str> {
    if json.len() > MAX_LINE_BYTES || !crate::validation::no_hidden_chars(json) {
        return Err("too long or hidden characters");
    }
    let marker: Marker = serde_json::from_str(json).map_err(|_| "not a job")?;
    if !matches!(marker.kind, Kind::Prompt | Kind::Reminder | Kind::DiskCheck) {
        return Err("kind");
    }
    let weekday = match marker.weekday {
        None => None,
        Some(name) => Some(
            WEEKDAYS
                .iter()
                .position(|day| day.eq_ignore_ascii_case(&name))
                .ok_or("weekday")?
                + 1,
        ),
    };
    let at_minute = match marker.time {
        None => None,
        Some(time) => Some(at_minute(&time).ok_or("time")?),
    };
    let params = Params {
        roadmap_id: None,
        title: marker.title,
        instruction: marker.instruction,
        text: marker.text,
        notify: marker.notify,
        threshold_percent: marker.threshold_percent,
    };
    let mut body = json!({
        "kind": marker.kind,
        "cadence": marker.cadence,
        "weekday": weekday,
        "month_day": marker.month_day,
        "every": marker.every,
        "start_date": marker.start_date,
        "at_minute": at_minute,
        "max_runs": marker.max_runs,
        "params": params,
    });
    if let Some(fields) = body.as_object_mut() {
        fields.retain(|_, value| !value.is_null());
    }
    let input: NewJob = serde_json::from_value(body.clone()).map_err(|_| "body")?;
    input
        .clone()
        .into_job(String::new(), now, tz)
        .map_err(|_| "invalid job")?;
    if crate::routes::jobs::is_private_job(&input.params, held) {
        return Err("private");
    }
    Ok((body, input))
}

/// The owner proposals of an answer: at most [`MAX_PROPOSALS`] and no more
/// than the owner's room holds, each checked like `POST /v1/jobs`. Anything
/// refused is logged without its text and dropped; an unreadable job count
/// or holdings drop them all.
pub(crate) async fn proposals(state: &AppState, lines: &[String]) -> Vec<JobProposal> {
    if lines.is_empty() {
        return Vec::new();
    }
    if lines.len() > MAX_PROPOSALS {
        tracing::info!(
            dropped = lines.len() - MAX_PROPOSALS,
            "job markers over the limit dropped"
        );
    }
    let used = tokio::time::timeout(DB_TIMEOUT, store::used(&state.db)).await;
    let Ok(Ok((owner, _))) = used else {
        tracing::warn!("job markers dropped: jobs unavailable");
        return Vec::new();
    };
    let Some(held) = crate::auto_research::held_symbols(state).await else {
        tracing::warn!("job markers dropped: holdings unavailable");
        return Vec::new();
    };
    let room = MAX_OWNER_JOBS.saturating_sub(owner);
    let now = OffsetDateTime::now_utc();
    let mut found = Vec::new();
    for json in lines.iter().take(MAX_PROPOSALS) {
        if found.len() >= room {
            tracing::info!("job marker dropped: no room");
            break;
        }
        match proposal(json, now, state.jobs.time_zone(), &held) {
            Ok((body, _)) => found.push(JobProposal {
                id: Uuid::now_v7(),
                body,
            }),
            Err(reason) => tracing::info!(reason, "job marker dropped"),
        }
    }
    found
}

/// Makes the research job of the first `[[job_self …]]` line for `user`, only
/// while Research and automatic web search are on (what its runs need):
/// a `prompt` that passes the privacy gate first, within Jarvis's own 3. Then
/// it writes the audit row and pushes to the owner; the caller posts
/// [`note`]. `None` when nothing was made (logged without its text).
pub(crate) async fn create_own(state: &AppState, user: Uuid, lines: &[String]) -> Option<Job> {
    let json = lines.first()?;
    if lines.len() > 1 {
        tracing::info!(dropped = lines.len() - 1, "extra job_self markers dropped");
    }
    let tz = state.jobs.time_zone();
    let checked = if !crate::jobs::chat::research_on(state, Origin::Jarvis) {
        Err("research off")
    } else if let Some(held) = crate::auto_research::held_symbols(state).await {
        proposal(json, OffsetDateTime::now_utc(), tz, &held).and_then(|(_, input)| {
            if input.kind != Kind::Prompt {
                Err("kind")
            } else if names_a_site(&input.params) {
                Err("site")
            } else {
                Ok(input)
            }
        })
    } else {
        Err("holdings unavailable")
    };
    let input = checked
        .inspect_err(|reason| tracing::info!(reason, "job_self marker dropped"))
        .ok()?;
    let owner = user.hyphenated().to_string();
    let created = tokio::time::timeout(
        DB_TIMEOUT,
        store::create_for_jarvis(&state.db, input, &owner, tz),
    )
    .await;
    let job = match created {
        Ok(Ok(job)) => job,
        Ok(Err(error)) => {
            tracing::info!(%error, "job_self marker dropped");
            return None;
        }
        Err(_) => {
            tracing::warn!("job_self marker dropped: timed out");
            return None;
        }
    };
    crate::audit::record_security_event(
        state,
        None,
        "jobs.created",
        "ok",
        Some(&format!("origin=jarvis job={}", job.id)),
    )
    .await;
    state.notifier.notify_user(
        NotifyEvent::JobResult(JobResult::Created {
            job: job.id.parse().unwrap_or_default(),
            title: job.params.title.clone().unwrap_or_default(),
            schedule: schedule(&job),
        }),
        user,
    );
    Some(job)
}

/// Whether a job's title or instruction names a link or a domain. A job of
/// Jarvis's own searches without the owner's click, for 30 days, from text
/// web content may have steered: it may only name a topic, never a site the
/// search could carry details to.
fn names_a_site(params: &Params) -> bool {
    [params.title.as_deref(), params.instruction.as_deref()]
        .into_iter()
        .flatten()
        .flat_map(str::split_whitespace)
        .any(|word| {
            let word = word.to_ascii_lowercase();
            let word = word.trim_matches(|c: char| !c.is_ascii_alphanumeric());
            word.contains("http")
                || word.contains("www")
                || word.contains('/')
                || word.split_once('.').is_some_and(|(name, rest)| {
                    !name.is_empty()
                        && rest.len() >= 2
                        && rest.chars().all(|c| c.is_ascii_alphabetic() || c == '.')
                })
        })
}

/// The fixed note a job Jarvis made for itself leaves in the conversation.
pub(crate) fn note(job: &Job, tz: Option<&TimeZone>) -> String {
    let expires = job
        .expires_at
        .and_then(|at| jiff::Timestamp::from_second(at.unix_timestamp()).ok())
        .map(|at| {
            at.to_zoned(tz.cloned().unwrap_or(TimeZone::UTC))
                .strftime("%-d %B %Y")
                .to_string()
        })
        .unwrap_or_default();
    format!(
        "I set up a research job for myself: {} — {}. It expires on {expires}; you can pause \
         or delete it in Jobs.",
        // Model text in Core's note: no Markdown link.
        job.params
            .title
            .as_deref()
            .unwrap_or_default()
            .replace(['[', ']', '(', ')'], ""),
        schedule(job)
    )
}

/// `weekly on Monday at 08:00`, `every 6 months on day 1 at 07:00, 2 times`.
fn schedule(job: &Job) -> String {
    let day = job
        .weekday
        .and_then(|day| WEEKDAYS.get(usize::from(day).checked_sub(1)?))
        .copied()
        .unwrap_or_default();
    let month_day = job.month_day.unwrap_or_default();
    let mut text = match (job.cadence, job.every) {
        (Cadence::Once, _) => format!("once on {}", job.start_date.as_deref().unwrap_or("?")),
        (Cadence::Daily, 1) => "daily".to_owned(),
        (Cadence::Daily, n) => format!("every {n} days"),
        (Cadence::Weekly, 1) => format!("weekly on {day}"),
        (Cadence::Weekly, n) => format!("every {n} weeks on {day}"),
        (Cadence::Monthly, 1) => format!("monthly on day {month_day}"),
        (Cadence::Monthly, n) => format!("every {n} months on day {month_day}"),
    };
    text.push_str(&format!(
        " at {:02}:{:02}",
        job.at_minute / 60,
        job.at_minute % 60
    ));
    if let Some(max) = job.max_runs {
        text.push_str(&format!(", {max} times"));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> OffsetDateTime {
        OffsetDateTime::parse(
            "2026-11-13T10:00:00Z",
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap()
    }

    fn tz() -> TimeZone {
        crate::jobs::time_zone().expect("zone")
    }

    /// `json` on one line (a test may wrap it).
    fn check(json: &str) -> Result<Value, &'static str> {
        let json = json.replace('\n', "");
        proposal(&json, now(), Some(&tz()), &["ASML".to_owned()]).map(|(body, _)| body)
    }

    #[test]
    fn only_trailing_marker_lines_are_taken_in_reply_order() {
        let taken = |text: &str| {
            let mut text = text.to_owned();
            let markers = take(&mut text);
            (text, markers)
        };
        let (text, markers) = taken(
            "Answer.\n[[job {\"a\":1}]]\n\n[[web_search]]\n[[job {\"b\":2}]]\n\
             [[job_self {\"c\":3}]]\n[[job broken\n",
        );
        assert_eq!(text, "Answer.");
        assert_eq!(
            markers,
            Markers {
                web_search: true,
                jobs: vec!["{\"a\":1}".into(), "{\"b\":2}".into()],
                job_self: vec!["{\"c\":3}".into()],
                any_job: true,
            }
        );
        // A marker before ordinary text, or inline, is ordinary text.
        for kept in [
            "[[job {}]]\nAnswer.",
            "Answer. [[job {}]]",
            "Use [[job_self {}]] like this.\nDone.",
            "Answer.\n[[JOB {}]]",
        ] {
            assert_eq!(taken(kept), (kept.to_owned(), Markers::default()), "{kept}");
        }
        let (text, markers) = taken("[[job {}]]");
        assert_eq!((text.as_str(), markers.jobs.len()), ("", 1));
        // The web search marker alone keeps its old meaning.
        let (text, markers) = taken("Answer.\n[[web_search]]");
        assert_eq!(text, "Answer.");
        assert!(markers.web_search && !markers.any_job);
    }

    #[test]
    fn a_marker_becomes_the_exact_api_body() {
        assert_eq!(
            check(
                r#"{"kind":"prompt","title":"AI news","instruction":"Summarize this week's AI news",
                "cadence":"weekly","weekday":"friday","time":"08:30","notify":false}"#
            ),
            Ok(
                json!({"kind": "prompt", "cadence": "weekly", "weekday": 5, "at_minute": 510,
                "params": {"title": "AI news", "instruction": "Summarize this week's AI news",
                    "notify": false}})
            )
        );
        assert_eq!(
            check(
                r#"{"kind":"reminder","title":"Call the bank","text":"Ask about the card",
                "cadence":"once","start_date":"2026-12-01","time":"09:00"}"#
            ),
            Ok(
                json!({"kind": "reminder", "cadence": "once", "start_date": "2026-12-01",
                "at_minute": 540, "params": {"title": "Call the bank",
                    "text": "Ask about the card"}})
            )
        );
        assert_eq!(
            check(
                r#"{"kind":"prompt","title":"Energy","instruction":"Dutch energy price outlook",
                "cadence":"monthly","month_day":1,"every":6,"start_date":"2027-01-01","max_runs":2}"#
            ),
            Ok(
                json!({"kind": "prompt", "cadence": "monthly", "month_day": 1, "every": 6,
                "start_date": "2027-01-01", "max_runs": 2,
                "params": {"title": "Energy", "instruction": "Dutch energy price outlook"}})
            )
        );
        assert_eq!(
            check(r#"{"kind":"disk_check","threshold_percent":90,"cadence":"daily"}"#),
            Ok(json!({"kind": "disk_check", "cadence": "daily",
                "params": {"threshold_percent": 90}}))
        );
        assert_eq!(check(r#"{"kind":"disk_check"}"#).map(|_| ()), Ok(()));
        // The runtime facts' example.
        assert_eq!(
            check(
                r#"{"kind":"reminder","title":"Water the plants","cadence":"weekly","weekday":"saturday","time":"09:00"}"#
            ),
            Ok(
                json!({"kind": "reminder", "cadence": "weekly", "weekday": 6, "at_minute": 540,
                "params": {"title": "Water the plants"}})
            )
        );
        assert!(check(r#"{"kind":"disk_check","weekday":"SUNDAY","cadence":"weekly"}"#).is_ok());
    }

    #[test]
    fn markers_are_refused_like_api_bodies() {
        for (json, reason) in [
            ("not json", "not a job"),
            (r#"{"kind":"disk_check","extra":1}"#, "not a job"),
            (r#"{"kind":"dependency_review"}"#, "kind"),
            (
                r#"{"kind":"prompt","title":"x","instruction":"y","roadmap_id":"z"}"#,
                "not a job",
            ),
            (
                r#"{"kind":"disk_check","cadence":"weekly","weekday":"mon"}"#,
                "weekday",
            ),
            (
                r#"{"kind":"disk_check","cadence":"weekly","weekday":1}"#,
                "not a job",
            ),
            (r#"{"kind":"disk_check","time":"7:30"}"#, "time"),
            (r#"{"kind":"disk_check","time":"24:00"}"#, "time"),
            (r#"{"kind":"disk_check","time":"07:60"}"#, "time"),
            (r#"{"kind":"disk_check","time":"0730"}"#, "time"),
            (
                r#"{"kind":"disk_check","threshold_percent":100}"#,
                "invalid job",
            ),
            (
                r#"{"kind":"disk_check","weekday":"monday","cadence":"daily"}"#,
                "invalid job",
            ),
            (r#"{"kind":"disk_check","cadence":"once"}"#, "invalid job"),
            (
                r#"{"kind":"reminder","title":"Past","cadence":"once","start_date":"2026-11-01"}"#,
                "invalid job",
            ),
            (
                r#"{"kind":"disk_check","cadence":"daily","every":400}"#,
                "invalid job",
            ),
            (r#"{"kind":"disk_check","max_runs":101}"#, "invalid job"),
            (r#"{"kind":"reminder"}"#, "invalid job"),
            (r#"{"kind":"prompt","title":"x"}"#, "invalid job"),
            (
                "{\"kind\":\"reminder\",\"title\":\"a\\u202eb\"}",
                "invalid job",
            ),
            (
                "{\"kind\":\"reminder\",\"title\":\"a\u{200b}b\"}",
                "too long or hidden characters",
            ),
            // The privacy gate: identifiers and held symbols, in either field.
            (
                r#"{"kind":"prompt","title":"Mail","instruction":"news for me@example.com"}"#,
                "private",
            ),
            (
                r#"{"kind":"prompt","title":"ASML outlook","instruction":"chip makers"}"#,
                "private",
            ),
            (
                r#"{"kind":"prompt","title":"Chips","instruction":"asml news"}"#,
                "private",
            ),
        ] {
            assert_eq!(check(json), Err(reason), "{json}");
        }
        let long = format!(
            r#"{{"kind":"reminder","title":"x","text":"{}"}}"#,
            "a".repeat(MAX_LINE_BYTES)
        );
        assert_eq!(check(&long), Err("too long or hidden characters"));
        // A marker is one line.
        assert_eq!(
            proposal("{\"kind\":\n\"disk_check\"}", now(), None, &[]).map(|_| ()),
            Err("too long or hidden characters")
        );
    }

    #[test]
    fn jobs_of_jarvis_own_name_no_site() {
        let params = |title: &str, instruction: &str| Params {
            title: Some(title.into()),
            instruction: Some(instruction.into()),
            ..Params::default()
        };
        for (title, instruction) in [
            ("News", "Open https://evil.example/?q=x"),
            ("News", "see evil.example for it"),
            ("News", "www dot evil"),
            ("evil.co", "news"),
            ("News", "path a/b"),
            ("News", "(EVIL.Example)."),
        ] {
            assert!(
                names_a_site(&params(title, instruction)),
                "{title} {instruction}"
            );
        }
        for (title, instruction) in [
            ("Rust releases", "Rust 1.90 release notes."),
            ("AI news", "Summarize this week's AI news. Keep it short."),
        ] {
            assert!(
                !names_a_site(&params(title, instruction)),
                "{title} {instruction}"
            );
        }
    }

    #[test]
    fn notes_name_the_schedule_and_the_expiry_date() {
        let (_, input) = proposal(
            r#"{"kind":"prompt","title":"Rust releases","instruction":"Rust release notes","cadence":"weekly","weekday":"monday","time":"08:00"}"#,
            now(),
            Some(&tz()),
            &[],
        )
        .unwrap();
        let mut job = input.into_job("id".into(), now(), Some(&tz())).unwrap();
        job.expires_at = Some(now() + time::Duration::days(30));
        assert_eq!(
            note(&job, Some(&tz())),
            "I set up a research job for myself: Rust releases — weekly on Monday at 08:00. \
             It expires on 13 December 2026; you can pause or delete it in Jobs."
        );
        job.cadence = Cadence::Monthly;
        job.weekday = None;
        job.month_day = Some(1);
        job.every = 6;
        job.max_runs = Some(2);
        assert_eq!(schedule(&job), "every 6 months on day 1 at 08:00, 2 times");
        job.cadence = Cadence::Once;
        job.start_date = Some("2026-12-01".into());
        job.max_runs = None;
        assert_eq!(schedule(&job), "once on 2026-12-01 at 08:00");
        job.cadence = Cadence::Daily;
        job.every = 1;
        assert_eq!(schedule(&job), "daily at 08:00");
    }
}
