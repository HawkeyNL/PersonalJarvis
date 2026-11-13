//! Scheduled jobs (schemas 0019 and 0021): work Core runs by itself on a
//! schedule, and a record of every run (see `docs/SCHEDULED_JOBS.md`).
//!
//! A job's kind is a closed list with typed parameters. This module holds
//! the types, input validation, the slot math and the runner: a job's slots
//! are wall-clock minutes in [`TIME_ZONE`], so they follow daylight saving
//! time. A minute that does not exist on a spring-forward day moves
//! forward by the gap (02:30 becomes 03:30); a minute that occurs twice on a
//! fall-back day runs once, at its first occurrence.

mod dependency_review;
pub mod store;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use jiff::tz::TimeZone;
use jiff::{civil, Timestamp};
use reqwest::header::HeaderValue;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tokio::sync::Notify;

use jarvis_client_core::realtime::Event;
use jarvis_store::Database;

use crate::realtime::Hub;
use crate::roadmaps::github::GithubHandle;
use store::Finish;

/// The zone every schedule is in.
pub const TIME_ZONE: &str = "Europe/Amsterdam";
/// A slot older than this is not run late; one `missed` row records it.
pub const CATCH_UP: Duration = Duration::from_secs(24 * 60 * 60);
/// Runs are kept this long.
pub const RUN_RETENTION_DAYS: u32 = 180;
pub const MAX_RUNS: usize = 50;
pub const MAX_PRIORITIES: usize = 20;
pub const MAX_REASON: usize = 300;
pub const MAX_SUMMARY: usize = 2000;
/// Serialized size of a run's facts (the schema's bound).
pub const MAX_FACTS_BYTES: usize = 64 * 1024;
const LAST_MINUTE: u16 = 24 * 60 - 1;
/// Between two passes of the runner.
const TICK: Duration = Duration::from_secs(60);
/// Bounds each database call of the runner and the routes.
pub(crate) const DB_TIMEOUT: Duration = Duration::from_secs(10);
/// Bounds the work of one run.
const RUN_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Monday 07:00, the default of a new job.
const DEFAULT_WEEKDAY: u8 = 1;
const DEFAULT_AT_MINUTE: u16 = 7 * 60;
/// The first of the month, the default of a monthly job.
const DEFAULT_MONTH_DAY: u8 = 1;
/// Jobs of the owner's chat-made kinds (`prompt`, `reminder`, `disk_check`).
pub const MAX_OWNER_JOBS: usize = 10;
/// Jobs Jarvis made for itself, apart from the owner's.
pub const MAX_JARVIS_JOBS: usize = 3;
/// A job Jarvis made for itself expires this long after it was made: the
/// runner skips it and it leaves Jarvis's room (deletion follows with the
/// runners).
pub const JARVIS_JOB_DAYS: i64 = 30;
pub const MAX_TITLE: usize = 60;
pub const MAX_INSTRUCTION: usize = 500;
pub const MAX_TEXT: usize = 500;
pub const MAX_RUN_LIMIT: u16 = 100;
/// `start_date` is at most this many days ahead.
const MAX_START_DAYS: i64 = 2 * 366;
const DEFAULT_THRESHOLD_PERCENT: u8 = 80;

/// The zone of every schedule, from the system time zone database; `None`
/// when it cannot be loaded, and the runner stays off.
pub fn time_zone() -> Option<TimeZone> {
    TimeZone::get(TIME_ZONE).ok()
}

/// Why a job operation failed. Database detail is logged by the caller,
/// never returned to clients.
#[derive(Debug)]
pub enum JobError {
    Database(Box<surrealdb::Error>),
    /// A stored row that does not parse.
    InvalidData,
    /// Input that breaks a rule; the message is safe to return.
    Invalid(&'static str),
    NotFound,
    /// The job is not at the expected revision.
    Conflict,
    /// A job of this kind already exists.
    Exists,
    /// No room for another job of this origin; the message says which.
    Full(&'static str),
    /// The owner ran the job less than an hour ago.
    TooSoon,
}

impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(error) => write!(f, "job database error: {error}"),
            Self::InvalidData => f.write_str("invalid stored job row"),
            Self::Invalid(message) => f.write_str(message),
            Self::NotFound => f.write_str("not found"),
            Self::Conflict => f.write_str("job revision conflict"),
            Self::Exists => f.write_str("a job of this kind already exists"),
            Self::Full(message) => f.write_str(message),
            Self::TooSoon => f.write_str("a job runs on request at most once an hour"),
        }
    }
}

impl std::error::Error for JobError {}

impl From<surrealdb::Error> for JobError {
    fn from(error: surrealdb::Error) -> Self {
        Self::Database(Box::new(error))
    }
}

type Result<T> = std::result::Result<T, JobError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    DependencyReview,
    /// A scheduled research run: a web search for `params.instruction`.
    Prompt,
    /// The owner's fixed text at a set time.
    Reminder,
    /// A note when a Home Node disk is fuller than a threshold.
    DiskCheck,
}

impl Kind {
    /// Whether the runner runs jobs of this kind yet. The others are stored
    /// and edited, but no slot of theirs runs and no row is written.
    pub fn runnable(self) -> bool {
        matches!(self, Self::DependencyReview)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cadence {
    /// Once, on `start_date`.
    Once,
    Daily,
    Weekly,
    /// On `month_day`, or a shorter month's last day.
    Monthly,
}

/// Who made a job. Only Core sets it; Jarvis makes jobs through
/// [`store::create_for_jarvis`], never through the API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Owner,
    Jarvis,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PausedReason {
    PausedAfterFailures,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    Schedule,
    Owner,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Ok,
    /// Some of the work failed; what succeeded is kept.
    Partial,
    Failed,
    /// The slot passed more than [`CATCH_UP`] ago without a run.
    Missed,
    /// Core stopped during the run.
    Interrupted,
}

impl RunStatus {
    /// The wire name, as serde writes it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Ok => "ok",
            Self::Partial => "partial",
            Self::Failed => "failed",
            Self::Missed => "missed",
            Self::Interrupted => "interrupted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummarySource {
    Claude,
    Deterministic,
}

/// Typed parameters; unknown fields are refused, and so is a field the
/// job's kind does not take.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
    /// `dependency_review`: the roadmap a run proposes its items to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roadmap_id: Option<String>,
    /// `prompt`, `reminder`: one line, 1 to 60 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// `prompt`: what to research, 1 to 500 characters on one line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,
    /// `reminder`: optional text, 1 to 500 characters on one line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// `prompt`: push when a run ends (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify: Option<bool>,
    /// `disk_check`: 50 to 99 (default 80).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold_percent: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub kind: Kind,
    pub cadence: Cadence,
    /// ISO weekday (Monday = 1) of a weekly job.
    #[serde(default)]
    pub weekday: Option<u8>,
    /// Day of the month (1 to 31) of a monthly job.
    #[serde(default)]
    pub month_day: Option<u8>,
    /// Every this many days, weeks or months; 1 for a one-time job.
    pub every: u16,
    /// `YYYY-MM-DD` in [`TIME_ZONE`]: the day of a one-time job, or the
    /// first day a repeating job may run, from which `every` counts.
    #[serde(default)]
    pub start_date: Option<String>,
    /// Disabled after this many scheduled runs since the last schedule
    /// change.
    #[serde(default)]
    pub max_runs: Option<u16>,
    /// Minutes after midnight in [`TIME_ZONE`].
    pub at_minute: u16,
    pub enabled: bool,
    pub revision: u64,
    #[serde(default)]
    pub params: Params,
    /// The newest slot the runner handled; none since a schedule change.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_slot_at: Option<OffsetDateTime>,
    /// Scheduled runs since the last schedule change.
    pub run_count: u16,
    pub origin: Origin,
    #[serde(default)]
    pub owner_id: Option<String>,
    /// The conversation the job's results go to.
    #[serde(default)]
    pub conversation_id: Option<String>,
    /// When a job Jarvis made for itself goes away.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
    /// Failed runs in a row.
    pub failure_count: u16,
    /// Why Core disabled the job; cleared when the owner enables it.
    #[serde(default)]
    pub paused_reason: Option<PausedReason>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// A new job; absent fields default to weekly on Monday at 07:00, enabled.
/// Who made it and where it reports are set by Core, never by the client.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewJob {
    pub kind: Kind,
    #[serde(default)]
    pub cadence: Option<Cadence>,
    #[serde(default)]
    pub weekday: Option<u8>,
    #[serde(default)]
    pub month_day: Option<u8>,
    #[serde(default)]
    pub every: Option<u16>,
    #[serde(default)]
    pub start_date: Option<String>,
    #[serde(default)]
    pub max_runs: Option<u16>,
    #[serde(default)]
    pub at_minute: Option<u16>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub params: Params,
    /// The owner's user id, from the session.
    #[serde(skip)]
    pub owner_id: Option<String>,
}

/// Absent fields stay; `start_date` and `max_runs` are cleared with null.
/// New `params` replace the stored ones but keep the stored `notify` and
/// `threshold_percent` when they leave them out.
/// Only a weekly job has a weekday and only a monthly one a month day: a job
/// that becomes weekly keeps its weekday or gets Monday, one that becomes
/// monthly keeps its day or gets the 1st. `every` falls back to 1 when the
/// cadence changes; a one-time job has no `max_runs`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobPatch {
    #[serde(default)]
    pub cadence: Option<Cadence>,
    #[serde(default)]
    pub weekday: Option<u8>,
    #[serde(default)]
    pub month_day: Option<u8>,
    #[serde(default)]
    pub every: Option<u16>,
    #[serde(default, deserialize_with = "present")]
    pub start_date: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    pub max_runs: Option<Option<u16>>,
    #[serde(default)]
    pub at_minute: Option<u16>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub params: Option<Params>,
}

/// A field that is present, even as null (`Some(None)`); absent is `None`.
fn present<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// One entry of a run's priority list; `ref` names an entry of its facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Priority {
    #[serde(rename = "ref")]
    pub reference: String,
    pub rank: u8,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub job_id: String,
    pub kind: Kind,
    #[serde(with = "time::serde::rfc3339")]
    pub slot_at: OffsetDateTime,
    pub trigger: Trigger,
    pub status: RunStatus,
    #[serde(default)]
    pub error_code: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub summary_source: Option<SummarySource>,
    #[serde(default)]
    pub priorities: Vec<Priority>,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
}

fn check(ok: bool, message: &'static str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(JobError::Invalid(message))
    }
}

/// The weekday a schedule stores: a weekly job has one (`fallback` when none
/// is given), other jobs none.
fn weekday_for(cadence: Cadence, given: Option<u8>, fallback: u8) -> Result<Option<u8>> {
    if cadence != Cadence::Weekly {
        check(given.is_none(), "only a weekly job has a weekday")?;
        return Ok(None);
    }
    let weekday = given.unwrap_or(fallback);
    check(
        (1..=7).contains(&weekday),
        "weekday must be 1 (Monday) to 7 (Sunday)",
    )?;
    Ok(Some(weekday))
}

/// The month day a schedule stores: a monthly job has one (`fallback` when
/// none is given), other jobs none.
fn month_day_for(cadence: Cadence, given: Option<u8>, fallback: u8) -> Result<Option<u8>> {
    if cadence != Cadence::Monthly {
        check(given.is_none(), "only a monthly job has a month_day")?;
        return Ok(None);
    }
    let day = given.unwrap_or(fallback);
    check((1..=31).contains(&day), "month_day must be 1 to 31")?;
    Ok(Some(day))
}

/// The fields of a schedule that depend on each other.
struct Schedule {
    cadence: Cadence,
    weekday: Option<u8>,
    month_day: Option<u8>,
    every: u16,
    start_date: Option<String>,
    max_runs: Option<u16>,
}

impl Schedule {
    /// Check the fields together. A repeating job with `every` above 1 and
    /// no `start_date` counts from `today`. A `dependency_review` keeps the
    /// schedules it had before 0021: daily or weekly, nothing more.
    fn checked(mut self, kind: Kind, today: civil::Date) -> Result<Self> {
        check(
            kind != Kind::DependencyReview
                || (matches!(self.cadence, Cadence::Daily | Cadence::Weekly)
                    && self.every == 1
                    && self.start_date.is_none()
                    && self.max_runs.is_none()),
            "a dependency review runs daily or weekly",
        )?;
        let most = match self.cadence {
            Cadence::Once => 1,
            Cadence::Daily => 365,
            Cadence::Weekly => 104,
            Cadence::Monthly => 24,
        };
        check(
            (1..=most).contains(&self.every),
            "every must be 1 to 365 days, 104 weeks or 24 months, and 1 for a one-time job",
        )?;
        check(
            self.max_runs
                .is_none_or(|max| (1..=MAX_RUN_LIMIT).contains(&max)),
            "max_runs must be 1 to 100",
        )?;
        check(
            self.cadence != Cadence::Once || self.max_runs.is_none(),
            "a one-time job has no max_runs",
        )?;
        if let Some(date) = &self.start_date {
            let latest = today.checked_add(jiff::Span::new().days(MAX_START_DAYS));
            check(
                parse_date(date)
                    .is_some_and(|date| date.year() >= 2000 && latest.is_ok_and(|l| date <= l)),
                "start_date must be a date (YYYY-MM-DD) at most two years ahead",
            )?;
        }
        check(
            self.cadence != Cadence::Once || self.start_date.is_some(),
            "a one-time job needs a start_date",
        )?;
        if self.every > 1 && self.start_date.is_none() {
            self.start_date = Some(today.to_string());
        }
        Ok(self)
    }
}

/// A `YYYY-MM-DD` date, exactly as written.
fn parse_date(value: &str) -> Option<civil::Date> {
    value
        .parse::<civil::Date>()
        .ok()
        .filter(|date| date.to_string() == value)
}

/// The date at `now` in the zone (in UTC without it).
fn today(now: OffsetDateTime, tz: Option<&TimeZone>) -> Option<civil::Date> {
    let now = to_timestamp(now)?;
    Some(tz.unwrap_or(&TimeZone::UTC).to_datetime(now).date())
}

fn validate_at_minute(at_minute: u16) -> Result<()> {
    check(at_minute <= LAST_MINUTE, "at_minute must be 0 to 1439")
}

/// A title, instruction or text: not blank, at most `max` characters, one
/// line without hidden characters.
fn line(value: Option<&str>, max: usize) -> bool {
    value.is_none_or(|value| {
        !value.trim().is_empty()
            && value.chars().count() <= max
            && crate::validation::no_hidden_chars(value)
    })
}

/// The parameters as stored: checked for the kind, defaults filled in.
fn params_for(kind: Kind, mut params: Params) -> Result<Params> {
    let Params {
        roadmap_id,
        title,
        instruction,
        text,
        notify,
        threshold_percent,
    } = &params;
    let only = match kind {
        Kind::DependencyReview => {
            title.is_none()
                && instruction.is_none()
                && text.is_none()
                && notify.is_none()
                && threshold_percent.is_none()
        }
        Kind::Prompt => roadmap_id.is_none() && text.is_none() && threshold_percent.is_none(),
        Kind::Reminder => {
            roadmap_id.is_none()
                && instruction.is_none()
                && notify.is_none()
                && threshold_percent.is_none()
        }
        Kind::DiskCheck => {
            roadmap_id.is_none()
                && title.is_none()
                && instruction.is_none()
                && text.is_none()
                && notify.is_none()
        }
    };
    check(only, "params has a field this kind of job does not take")?;
    check(
        roadmap_id.as_deref().is_none_or(|id| {
            uuid::Uuid::parse_str(id).is_ok_and(|uuid| uuid.hyphenated().to_string() == id)
        }),
        "params.roadmap_id must be a roadmap id",
    )?;
    check(
        line(title.as_deref(), MAX_TITLE),
        "params.title must be 1 to 60 characters on one line",
    )?;
    check(
        line(instruction.as_deref(), MAX_INSTRUCTION)
            && instruction
                .as_deref()
                .is_none_or(|i| jarvis_llm::ResearchRequest::new(i).is_some()),
        "params.instruction must be 1 to 500 characters on one line",
    )?;
    check(
        line(text.as_deref(), MAX_TEXT),
        "params.text must be 1 to 500 characters on one line",
    )?;
    check(
        threshold_percent.is_none_or(|percent| (50..=99).contains(&percent)),
        "params.threshold_percent must be 50 to 99",
    )?;
    match kind {
        Kind::DependencyReview => {}
        Kind::Prompt => {
            check(
                title.is_some() && instruction.is_some(),
                "a prompt job needs params.title and params.instruction",
            )?;
            params.notify.get_or_insert(true);
        }
        Kind::Reminder => check(title.is_some(), "a reminder needs params.title")?,
        Kind::DiskCheck => {
            params
                .threshold_percent
                .get_or_insert(DEFAULT_THRESHOLD_PERCENT);
        }
    }
    Ok(params)
}

impl NewJob {
    /// The owner's job this input creates, at revision 1. With the zone, its
    /// newest slot at `now` counts as handled, so only later slots run.
    pub fn into_job(self, id: String, now: OffsetDateTime, tz: Option<&TimeZone>) -> Result<Job> {
        self.build(id, now, tz, Origin::Owner)
    }

    /// A job Jarvis makes for itself: only `prompt`, expiring after
    /// [`JARVIS_JOB_DAYS`].
    fn into_jarvis_job(
        self,
        id: String,
        now: OffsetDateTime,
        tz: Option<&TimeZone>,
    ) -> Result<Job> {
        check(
            self.kind == Kind::Prompt,
            "Jarvis makes only prompt jobs of its own",
        )?;
        self.build(id, now, tz, Origin::Jarvis)
    }

    fn build(
        self,
        id: String,
        now: OffsetDateTime,
        tz: Option<&TimeZone>,
        origin: Origin,
    ) -> Result<Job> {
        let cadence = self.cadence.unwrap_or(Cadence::Weekly);
        let schedule = Schedule {
            cadence,
            weekday: weekday_for(cadence, self.weekday, DEFAULT_WEEKDAY)?,
            month_day: month_day_for(cadence, self.month_day, DEFAULT_MONTH_DAY)?,
            every: self.every.unwrap_or(1),
            start_date: self.start_date,
            max_runs: self.max_runs,
        }
        .checked(self.kind, today(now, tz).ok_or(JobError::InvalidData)?)?;
        let at_minute = self.at_minute.unwrap_or(DEFAULT_AT_MINUTE);
        validate_at_minute(at_minute)?;
        let mut job = Job {
            id,
            kind: self.kind,
            cadence,
            weekday: schedule.weekday,
            month_day: schedule.month_day,
            every: schedule.every,
            start_date: schedule.start_date,
            max_runs: schedule.max_runs,
            at_minute,
            enabled: self.enabled.unwrap_or(true),
            revision: 1,
            params: params_for(self.kind, self.params)?,
            last_slot_at: None,
            run_count: 0,
            origin,
            owner_id: self.owner_id,
            conversation_id: None,
            expires_at: (origin == Origin::Jarvis)
                .then(|| now + time::Duration::days(JARVIS_JOB_DAYS)),
            failure_count: 0,
            paused_reason: None,
            created_at: now,
            updated_at: now,
        };
        job.check_once_ahead(now, tz)?;
        job.last_slot_at = tz.and_then(|tz| job.latest_slot(now, tz));
        Ok(job)
    }
}

impl Job {
    /// The job with `patch` applied, and whether its schedule (cadence,
    /// weekday, month day, `every`, start date, run limit, time or enabled)
    /// changed. A change sets `last_slot_at` to the newest slot at `now`
    /// under the new schedule (none without the zone), so no slot before the
    /// change runs, and starts the run count again. Enabling a job clears
    /// its failure streak and pause.
    pub fn patched(
        &self,
        patch: &JobPatch,
        now: OffsetDateTime,
        tz: Option<&TimeZone>,
    ) -> Result<(Job, bool)> {
        let cadence = patch.cadence.unwrap_or(self.cadence);
        let schedule = Schedule {
            cadence,
            weekday: weekday_for(
                cadence,
                patch.weekday,
                self.weekday.unwrap_or(DEFAULT_WEEKDAY),
            )?,
            month_day: month_day_for(
                cadence,
                patch.month_day,
                self.month_day.unwrap_or(DEFAULT_MONTH_DAY),
            )?,
            every: patch.every.unwrap_or(if cadence == self.cadence {
                self.every
            } else {
                1
            }),
            start_date: patch
                .start_date
                .clone()
                .unwrap_or_else(|| self.start_date.clone()),
            max_runs: patch.max_runs.unwrap_or(if cadence == Cadence::Once {
                None
            } else {
                self.max_runs
            }),
        }
        .checked(self.kind, today(now, tz).ok_or(JobError::InvalidData)?)?;
        let at_minute = patch.at_minute.unwrap_or(self.at_minute);
        validate_at_minute(at_minute)?;
        // New params keep the stored `notify` and threshold they leave out.
        let params = patch.params.clone().map_or_else(
            || self.params.clone(),
            |mut params| {
                params.notify = params.notify.or(self.params.notify);
                params.threshold_percent =
                    params.threshold_percent.or(self.params.threshold_percent);
                params
            },
        );
        let mut new = Job {
            cadence,
            weekday: schedule.weekday,
            month_day: schedule.month_day,
            every: schedule.every,
            start_date: schedule.start_date,
            max_runs: schedule.max_runs,
            at_minute,
            enabled: patch.enabled.unwrap_or(self.enabled),
            params: params_for(self.kind, params)?,
            ..self.clone()
        };
        let schedule = |job: &Job| {
            (
                job.cadence,
                job.weekday,
                job.month_day,
                job.every,
                job.start_date.clone(),
                job.max_runs,
                job.at_minute,
                job.enabled,
            )
        };
        let rescheduled = schedule(&new) != schedule(self);
        if rescheduled {
            new.check_once_ahead(now, tz)?;
            new.last_slot_at = tz.and_then(|tz| new.latest_slot(now, tz));
            new.run_count = 0;
        }
        if new.enabled && !self.enabled {
            new.failure_count = 0;
            new.paused_reason = None;
        }
        Ok((new, rescheduled))
    }

    /// An enabled one-time job must be ahead of `now`, or it would never run.
    fn check_once_ahead(&self, now: OffsetDateTime, tz: Option<&TimeZone>) -> Result<()> {
        if !self.enabled || self.cadence != Cadence::Once {
            return Ok(());
        }
        let start = self.start_date.as_deref().and_then(parse_date);
        let ahead = match (tz, start) {
            (Some(tz), Some(date)) => to_timestamp(now)
                .is_some_and(|now| self.slot_on(date, tz).is_some_and(|slot| slot > now)),
            (None, Some(date)) => today(now, None).is_some_and(|today| date >= today),
            (_, None) => false,
        };
        check(ahead, "a one-time job must be in the future")
    }

    /// Whether the job runs on `date` (from its `start_date`, if any).
    fn runs_on(&self, date: civil::Date, start: Option<civil::Date>) -> bool {
        if start.is_some_and(|start| date < start) {
            return false;
        }
        let every = i32::from(self.every.max(1));
        let days_since = |start: civil::Date| date.since(start).map_or(-1, |span| span.get_days());
        match self.cadence {
            Cadence::Once => Some(date) == start,
            Cadence::Daily => start.is_none_or(|start| days_since(start) % every == 0),
            Cadence::Weekly => {
                let iso_weekday = u8::try_from(date.weekday().to_monday_one_offset()).ok();
                iso_weekday.is_some()
                    && iso_weekday == self.weekday
                    && start.is_none_or(|start| (days_since(start) / 7) % every == 0)
            }
            Cadence::Monthly => {
                let Some(day) = self.month_day else {
                    return false;
                };
                let clamped = |of: civil::Date| {
                    i8::try_from(day).map_or(of.days_in_month(), |day| day.min(of.days_in_month()))
                };
                date.day() == clamped(date)
                    && start.is_none_or(|start| {
                        // `every` counts from the month of the first slot.
                        let first = month_index(start) + i32::from(clamped(start) < start.day());
                        (month_index(date) - first).rem_euclid(every) == 0
                    })
            }
        }
    }

    /// The most days between two slots: how far the slot search looks.
    fn period_days(&self) -> i32 {
        let every = i32::from(self.every.max(1));
        match self.cadence {
            Cadence::Once => 0,
            Cadence::Daily => every,
            Cadence::Weekly => 7 * every,
            // A shorter month moves a slot by at most three days.
            Cadence::Monthly => 32 * every,
        }
    }

    /// The job's slot on `date`, at its minute.
    fn slot_on(&self, date: civil::Date, tz: &TimeZone) -> Option<Timestamp> {
        let minute = i8::try_from(self.at_minute % 60).ok()?;
        let hour = i8::try_from(self.at_minute / 60).ok()?;
        let time = civil::Time::new(hour, minute, 0, 0).ok()?;
        tz.to_timestamp(date.to_datetime(time)).ok()
    }

    /// The newest slot at or before `now`.
    // ponytail: walks the days back one by one, at most one period (768 days
    // for every 24 months); compute the date directly if that ever shows up.
    pub fn latest_slot(&self, now: OffsetDateTime, tz: &TimeZone) -> Option<OffsetDateTime> {
        let now = to_timestamp(now)?;
        let start = self.start_date.as_deref().and_then(parse_date);
        let mut date = tz.to_datetime(now).date();
        if self.cadence == Cadence::Once {
            date = date.min(start?);
        }
        // One more day covers a slot later today than now.
        for _ in 0..=self.period_days() {
            if start.is_some_and(|start| date < start) {
                return None;
            }
            if self.runs_on(date, start) {
                if let Some(slot) = self.slot_on(date, tz).filter(|slot| *slot <= now) {
                    return from_timestamp(slot);
                }
            }
            date = date.yesterday().ok()?;
        }
        None
    }

    /// The first slot after `now`.
    pub fn next_slot(&self, now: OffsetDateTime, tz: &TimeZone) -> Option<OffsetDateTime> {
        let now = to_timestamp(now)?;
        let start = self.start_date.as_deref().and_then(parse_date);
        let mut date = tz.to_datetime(now).date();
        if let Some(start) = start {
            date = date.max(start);
        }
        for _ in 0..=self.period_days() {
            if self.runs_on(date, start) {
                if let Some(slot) = self.slot_on(date, tz).filter(|slot| *slot > now) {
                    return from_timestamp(slot);
                }
            }
            date = date.tomorrow().ok()?;
        }
        None
    }

    /// Whether the next scheduled run is the job's last: a one-time job's,
    /// or the one that reaches `max_runs`. A `missed` slot ends a one-time
    /// job too, but does not count as a run.
    pub fn ends_with(&self, status: RunStatus) -> bool {
        self.cadence == Cadence::Once
            || (status == RunStatus::Running
                && self
                    .max_runs
                    .is_some_and(|max| self.run_count.saturating_add(1) >= max))
    }

    /// What the runner owes this job at `now`: nothing, a run for the newest
    /// slot (late by less than [`CATCH_UP`]), or one `missed` row for it.
    /// Only the newest slot counts: older slots that passed while Core was
    /// down leave no row. Slots at or before `last_slot_at` are handled;
    /// without it (a change made while the zone was unavailable), slots at
    /// or before the last change (`updated_at`) never run. Nothing is owed
    /// for a kind the runner does not run yet, a job that reached its
    /// `max_runs` or one that expired.
    pub fn due(&self, now: OffsetDateTime, tz: &TimeZone) -> Due {
        if !self.enabled
            || !self.kind.runnable()
            || self.max_runs.is_some_and(|max| self.run_count >= max)
            || self.expires_at.is_some_and(|at| at <= now)
        {
            return Due::Nothing;
        }
        let Some(slot) = self.latest_slot(now, tz) else {
            return Due::Nothing;
        };
        if slot <= self.last_slot_at.unwrap_or(self.updated_at) {
            Due::Nothing
        } else if now - slot < CATCH_UP {
            Due::Run(slot)
        } else {
            Due::Missed(slot)
        }
    }
}

/// Months since year 0, for counting `every` months.
fn month_index(date: civil::Date) -> i32 {
    i32::from(date.year()) * 12 + i32::from(date.month()) - 1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    Nothing,
    Run(OffsetDateTime),
    Missed(OffsetDateTime),
}

/// Shared by the runner and the routes.
pub struct JobsHandle {
    /// `None` when [`TIME_ZONE`] cannot be loaded: no runner runs.
    tz: Option<TimeZone>,
    /// The job the owner asked to run, until that run ends.
    queued: Mutex<Option<String>>,
    wake: Notify,
}

impl Default for JobsHandle {
    /// Not configured: no time zone, no runner.
    fn default() -> Self {
        Self::with_time_zone(None)
    }
}

impl JobsHandle {
    /// Load [`TIME_ZONE`]; when that fails, the runner stays off.
    pub fn new() -> Self {
        let tz = time_zone();
        if tz.is_none() {
            tracing::error!(
                zone = TIME_ZONE,
                "scheduled jobs disabled: time zone unavailable"
            );
        }
        Self::with_time_zone(tz)
    }

    fn with_time_zone(tz: Option<TimeZone>) -> Self {
        Self {
            tz,
            queued: Mutex::new(None),
            wake: Notify::new(),
        }
    }

    pub fn configured(&self) -> bool {
        self.tz.is_some()
    }

    pub fn time_zone(&self) -> Option<&TimeZone> {
        self.tz.as_ref()
    }

    /// Queue the owner's run of `job_id` and wake the runner; refused while
    /// another one is queued or running.
    pub fn request_run(&self, job_id: &str) -> std::result::Result<(), &'static str> {
        if !self.configured() {
            return Err("scheduled jobs are not configured");
        }
        let busy = "a run on request is already queued or running";
        let mut queued = self.queued.lock().map_err(|_| busy)?;
        if queued.is_some() {
            return Err(busy);
        }
        *queued = Some(job_id.to_owned());
        drop(queued);
        self.wake.notify_one();
        Ok(())
    }

    fn queued(&self) -> Option<String> {
        self.queued.lock().ok()?.clone()
    }

    fn clear_queued(&self) {
        if let Ok(mut queued) = self.queued.lock() {
            *queued = None;
        }
    }
}

/// What runs need besides the database.
pub struct Work {
    /// The GitHub poller's allowlist; the dependency review reads only
    /// those repositories.
    pub github: Arc<GithubHandle>,
    /// The poller's read-only token; none when it is not configured.
    pub token: Option<HeaderValue>,
    /// The Claude subscription worker for typed tasks, never the router.
    pub brain: Arc<dyn crate::memecoin::agent::Brain>,
}

/// Spawn the runner unless the time zone failed to load. Every minute, or
/// when the owner asks, it runs what is due: one run at a time, its row
/// written before its work starts.
pub fn start(db: Database, handle: Arc<JobsHandle>, events: Hub, work: Work) {
    let Some(tz) = handle.tz.clone() else {
        return;
    };
    let runner = Runner {
        db,
        handle,
        events,
        tz,
        work,
    };
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = runner.handle.wake.notified() => {}
            }
            runner.pass(OffsetDateTime::now_utc()).await;
        }
    });
    tracing::info!("scheduled jobs running");
}

/// Why a runner database call did not succeed.
#[derive(Debug, PartialEq)]
enum Failed {
    /// An expected refusal: the job changed or is gone, or the owner ran it
    /// within the hour.
    Refused,
    /// An error or a timeout; worth trying again.
    Unavailable,
}

/// A database call of the runner, within [`DB_TIMEOUT`].
async fn call<T>(
    future: impl std::future::Future<Output = Result<T>>,
) -> std::result::Result<T, Failed> {
    match tokio::time::timeout(DB_TIMEOUT, future).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error @ (JobError::Conflict | JobError::TooSoon | JobError::NotFound))) => {
            tracing::debug!(%error, "scheduled jobs: refused");
            Err(Failed::Refused)
        }
        Ok(Err(error)) => {
            tracing::warn!(%error, "scheduled jobs: database call failed");
            Err(Failed::Unavailable)
        }
        Err(_) => {
            tracing::warn!("scheduled jobs: database call timed out");
            Err(Failed::Unavailable)
        }
    }
}

struct Runner {
    db: Database,
    handle: Arc<JobsHandle>,
    events: Hub,
    tz: TimeZone,
    work: Work,
}

impl Runner {
    /// Mark runs still running as interrupted: between passes none is, so
    /// such a row was left by an earlier Core or its end could not be
    /// recorded. False when that failed.
    async fn recover(&self) -> bool {
        let Ok(interrupted) = call(store::interrupt_running(&self.db)).await else {
            return false;
        };
        if !interrupted.is_empty() {
            tracing::warn!(runs = interrupted.len(), "scheduled job runs interrupted");
        }
        true
    }

    /// Nothing runs before stale runs are marked. Then the owner's queued
    /// run, which stays queued while the database fails, then every job's
    /// due slot. A job of a kind that does not run yet is skipped: its
    /// queued run is dropped and its slots are never due.
    async fn pass(&self, now: OffsetDateTime) {
        if !self.recover().await {
            return;
        }
        if let Some(job_id) = self.handle.queued() {
            let settled = match call(store::get(&self.db, &job_id)).await {
                Ok(Some(job)) if job.kind.runnable() => {
                    self.run(&job, OffsetDateTime::now_utc(), Trigger::Owner)
                        .await
                }
                Ok(_) => true,
                Err(failed) => failed == Failed::Refused,
            };
            if settled {
                self.handle.clear_queued();
            }
        }
        let Ok(jobs) = call(store::list(&self.db)).await else {
            return;
        };
        for job in jobs {
            match job.due(now, &self.tz) {
                Due::Nothing => {}
                Due::Run(slot) => {
                    self.run(&job, slot, Trigger::Schedule).await;
                }
                Due::Missed(slot) => {
                    let missed = store::start_run(
                        &self.db,
                        &job,
                        slot,
                        Trigger::Schedule,
                        RunStatus::Missed,
                    );
                    if let Ok(run_id) = call(missed).await {
                        self.announce(&job.id, &run_id, RunStatus::Missed);
                    }
                }
            }
        }
    }

    /// Write the row, do the work, record how it ended. False when the row
    /// could not be written for a reason worth retrying.
    async fn run(&self, job: &Job, slot: OffsetDateTime, trigger: Trigger) -> bool {
        let start = store::start_run(&self.db, job, slot, trigger, RunStatus::Running);
        let run_id = match call(start).await {
            Ok(run_id) => run_id,
            Err(failed) => return failed == Failed::Refused,
        };
        let mut finish = tokio::time::timeout(RUN_TIMEOUT, self.work(job))
            .await
            .unwrap_or_else(|_| Finish::failed("timed_out"));
        if let Err(reason) = finish.check() {
            // A result the store would refuse still ends the run.
            tracing::warn!(reason, "scheduled job result refused");
            finish = Finish::failed("invalid_result");
        }
        if call(store::finish_run(&self.db, &run_id, &finish)).await == Ok(true) {
            self.announce(&job.id, &run_id, finish.status);
        }
        true
    }

    async fn work(&self, job: &Job) -> Finish {
        match job.kind {
            Kind::DependencyReview => dependency_review::run(&self.work, &self.db, job).await,
            // Never reached: `pass` does not run these kinds yet.
            Kind::Prompt | Kind::Reminder | Kind::DiskCheck => Finish::failed("not_supported"),
        }
    }

    /// Tell connected apps a run ended; they reload it.
    fn announce(&self, job_id: &str, run_id: &str, status: RunStatus) {
        self.events.publish_all(Event::JobRunFinished {
            job_id: job_id.to_owned(),
            run_id: run_id.to_owned(),
            status: status.as_str().to_owned(),
        });
    }
}

fn to_timestamp(at: OffsetDateTime) -> Option<Timestamp> {
    Timestamp::new(at.unix_timestamp(), i32::try_from(at.nanosecond()).ok()?).ok()
}

/// Slots fall on whole minutes, so seconds are exact.
fn from_timestamp(at: Timestamp) -> Option<OffsetDateTime> {
    OffsetDateTime::from_unix_timestamp(at.as_second()).ok()
}

#[cfg(test)]
mod tests {
    use time::format_description::well_known::Rfc3339;

    use super::*;

    fn at(value: &str) -> OffsetDateTime {
        OffsetDateTime::parse(value, &Rfc3339).unwrap()
    }

    fn tz() -> TimeZone {
        time_zone().expect("Europe/Amsterdam in the system time zone database")
    }

    fn job(cadence: Cadence, weekday: Option<u8>, at_minute: u16) -> Job {
        let created = at("2026-01-01T00:00:00Z");
        Job {
            id: uuid::Uuid::now_v7().to_string(),
            kind: Kind::DependencyReview,
            cadence,
            weekday,
            month_day: None,
            every: 1,
            start_date: None,
            max_runs: None,
            at_minute,
            enabled: true,
            revision: 1,
            params: Params::default(),
            last_slot_at: None,
            run_count: 0,
            origin: Origin::Owner,
            owner_id: None,
            conversation_id: None,
            expires_at: None,
            failure_count: 0,
            paused_reason: None,
            created_at: created,
            updated_at: created,
        }
    }

    /// A Wednesday, 13:00 in Amsterdam.
    fn now() -> OffsetDateTime {
        at("2026-11-11T12:00:00Z")
    }

    fn new_job(body: serde_json::Value) -> Result<Job> {
        serde_json::from_value::<NewJob>(body)
            .map_err(|_| JobError::InvalidData)?
            .into_job("j".into(), at("2026-11-11T12:00:00Z"), None)
    }

    #[test]
    fn run_status_names_match_the_wire() {
        for status in [
            RunStatus::Running,
            RunStatus::Ok,
            RunStatus::Partial,
            RunStatus::Failed,
            RunStatus::Missed,
            RunStatus::Interrupted,
        ] {
            assert_eq!(serde_json::to_value(status).unwrap(), status.as_str());
        }
    }

    #[test]
    fn new_jobs_default_to_monday_seven_and_refuse_bad_input() {
        let job = new_job(serde_json::json!({"kind": "dependency_review"})).unwrap();
        assert_eq!(
            (job.cadence, job.weekday, job.at_minute, job.enabled),
            (Cadence::Weekly, Some(1), 420, true)
        );
        assert_eq!((job.revision, job.last_slot_at), (1, None));
        // With the zone, the slot before creation counts as handled.
        let seeded =
            serde_json::from_value::<NewJob>(serde_json::json!({"kind": "dependency_review"}))
                .unwrap()
                .into_job("j".into(), at("2026-11-11T12:00:00Z"), Some(&tz()))
                .unwrap();
        assert_eq!(seeded.last_slot_at, Some(at("2026-11-09T06:00:00Z")));
        let daily = new_job(
            serde_json::json!({"kind": "dependency_review", "cadence": "daily", "at_minute": 0}),
        )
        .unwrap();
        assert_eq!((daily.weekday, daily.at_minute), (None, 0));
        let id = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b";
        let linked =
            new_job(serde_json::json!({"kind": "dependency_review", "params": {"roadmap_id": id}}))
                .unwrap();
        assert_eq!(linked.params.roadmap_id.as_deref(), Some(id));
        for bad in [
            serde_json::json!({"kind": "research"}),
            serde_json::json!({"kind": "dependency_review", "prompt": "x"}),
            serde_json::json!({"kind": "dependency_review", "cadence": "monthly"}),
            serde_json::json!({"kind": "dependency_review", "cadence": "daily", "weekday": 1}),
            serde_json::json!({"kind": "dependency_review", "weekday": 0}),
            serde_json::json!({"kind": "dependency_review", "weekday": 8}),
            serde_json::json!({"kind": "dependency_review", "at_minute": 1440}),
            serde_json::json!({"kind": "dependency_review", "at_minute": -1}),
            serde_json::json!({"kind": "dependency_review", "params": {"roadmap_id": "plan"}}),
            serde_json::json!({"kind": "dependency_review", "params": {"roadmap_id": id.to_uppercase()}}),
            serde_json::json!({"kind": "dependency_review", "params": {"repo": "a/b"}}),
        ] {
            assert!(new_job(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn patches_keep_absent_fields_and_reset_the_slot_only_on_a_schedule_change() {
        let weekly = job(Cadence::Weekly, Some(3), 600);
        let (daily, rescheduled) = weekly
            .patched(
                &JobPatch {
                    cadence: Some(Cadence::Daily),
                    ..JobPatch::default()
                },
                now(),
                None,
            )
            .unwrap();
        assert_eq!(
            (daily.weekday, daily.at_minute, rescheduled),
            (None, 600, true)
        );
        // Back to weekly: Monday unless a weekday is given.
        let (back, _) = daily
            .patched(
                &JobPatch {
                    cadence: Some(Cadence::Weekly),
                    ..JobPatch::default()
                },
                now(),
                None,
            )
            .unwrap();
        assert_eq!(back.weekday, Some(1));
        let (params, rescheduled) = weekly
            .patched(
                &JobPatch {
                    params: Some(Params {
                        roadmap_id: Some("0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b".into()),
                        ..Params::default()
                    }),
                    ..JobPatch::default()
                },
                now(),
                None,
            )
            .unwrap();
        assert!(!rescheduled);
        assert_eq!(params.weekday, Some(3));
        let (same, rescheduled) = weekly.patched(&JobPatch::default(), now(), None).unwrap();
        assert_eq!((same, rescheduled), (weekly.clone(), false));
        let (off, rescheduled) = weekly
            .patched(
                &JobPatch {
                    enabled: Some(false),
                    ..JobPatch::default()
                },
                now(),
                None,
            )
            .unwrap();
        assert!(!off.enabled && rescheduled);
        // With the zone, a schedule change seeds the newest slot under the
        // new schedule; other changes keep the stored one.
        let mut stored = weekly.clone();
        stored.last_slot_at = Some(at("2026-11-04T09:00:00Z"));
        let (moved, _) = stored
            .patched(
                &JobPatch {
                    at_minute: Some(480),
                    ..JobPatch::default()
                },
                now(),
                Some(&tz()),
            )
            .unwrap();
        assert_eq!(moved.last_slot_at, Some(at("2026-11-11T07:00:00Z")));
        let (kept, _) = stored
            .patched(
                &JobPatch {
                    params: Some(Params::default()),
                    ..JobPatch::default()
                },
                now(),
                Some(&tz()),
            )
            .unwrap();
        assert_eq!(kept.last_slot_at, stored.last_slot_at);
        assert!(weekly
            .patched(
                &JobPatch {
                    cadence: Some(Cadence::Daily),
                    weekday: Some(2),
                    ..JobPatch::default()
                },
                now(),
                None,
            )
            .is_err());
        assert!(daily
            .patched(
                &JobPatch {
                    weekday: Some(2),
                    ..JobPatch::default()
                },
                now(),
                None,
            )
            .is_err());
    }

    #[test]
    fn slots_follow_daylight_saving_time_both_ways() {
        let tz = tz();
        let daily = |minute| job(Cadence::Daily, None, minute);
        // Winter (CET, UTC+1) and summer (CEST, UTC+2).
        assert_eq!(
            daily(420).latest_slot(at("2026-01-15T12:00:00Z"), &tz),
            Some(at("2026-01-15T06:00:00Z"))
        );
        assert_eq!(
            daily(420).latest_slot(at("2026-07-15T12:00:00Z"), &tz),
            Some(at("2026-07-15T05:00:00Z"))
        );
        // Spring forward on 2026-03-29: 02:30 does not exist and runs at
        // 03:30 CEST; 07:00 is CEST.
        assert_eq!(
            daily(150).latest_slot(at("2026-03-29T12:00:00Z"), &tz),
            Some(at("2026-03-29T01:30:00Z"))
        );
        assert_eq!(
            daily(420).latest_slot(at("2026-03-29T12:00:00Z"), &tz),
            Some(at("2026-03-29T05:00:00Z"))
        );
        // Fall back on 2026-10-25: 02:30 occurs twice and runs once, at the
        // first (CEST); the second occurrence is no new slot.
        let fold = daily(150);
        assert_eq!(
            fold.latest_slot(at("2026-10-25T00:45:00Z"), &tz),
            Some(at("2026-10-25T00:30:00Z"))
        );
        assert_eq!(
            fold.latest_slot(at("2026-10-25T01:45:00Z"), &tz),
            Some(at("2026-10-25T00:30:00Z"))
        );
        assert_eq!(
            fold.next_slot(at("2026-10-25T00:30:00Z"), &tz),
            Some(at("2026-10-26T01:30:00Z"))
        );
        assert_eq!(
            daily(420).latest_slot(at("2026-10-25T12:00:00Z"), &tz),
            Some(at("2026-10-25T06:00:00Z"))
        );
    }

    #[test]
    fn weekly_slots_wrap_around_the_week() {
        let tz = tz();
        let monday = job(Cadence::Weekly, Some(1), 420);
        // Sunday evening: last Monday's slot, next Monday's next.
        let sunday = at("2026-11-15T20:00:00Z");
        assert_eq!(
            monday.latest_slot(sunday, &tz),
            Some(at("2026-11-09T06:00:00Z"))
        );
        assert_eq!(
            monday.next_slot(sunday, &tz),
            Some(at("2026-11-16T06:00:00Z"))
        );
        // Monday before 07:00: still the week before.
        assert_eq!(
            monday.latest_slot(at("2026-11-16T05:59:00Z"), &tz),
            Some(at("2026-11-09T06:00:00Z"))
        );
        assert_eq!(
            monday.latest_slot(at("2026-11-16T06:00:00Z"), &tz),
            Some(at("2026-11-16T06:00:00Z"))
        );
        // Sunday 23:59 runs on Sunday, across midnight UTC of Monday.
        let late = job(Cadence::Weekly, Some(7), 1439);
        assert_eq!(
            late.latest_slot(at("2026-11-16T12:00:00Z"), &tz),
            Some(at("2026-11-15T22:59:00Z"))
        );
        assert_eq!(
            late.next_slot(at("2026-11-15T22:59:00Z"), &tz),
            Some(at("2026-11-22T22:59:00Z"))
        );
    }

    #[test]
    fn a_late_slot_runs_once_and_an_old_one_is_missed_once() {
        let tz = tz();
        let mut monday = job(Cadence::Weekly, Some(1), 420);
        monday.updated_at = at("2026-11-05T12:00:00Z");
        let slot = at("2026-11-09T06:00:00Z");
        // Not yet, then due, then late but within a day.
        assert_eq!(monday.due(at("2026-11-09T05:59:00Z"), &tz), Due::Nothing);
        assert_eq!(monday.due(slot, &tz), Due::Run(slot));
        assert_eq!(monday.due(at("2026-11-10T05:59:00Z"), &tz), Due::Run(slot));
        // Handled: nothing more until the next slot.
        monday.last_slot_at = Some(slot);
        assert_eq!(monday.due(at("2026-11-10T05:59:00Z"), &tz), Due::Nothing);
        // A day or more late: one missed row for the newest slot only, even
        // after weeks.
        monday.last_slot_at = Some(at("2026-10-19T06:00:00Z"));
        assert_eq!(
            monday.due(at("2026-11-10T06:00:00Z"), &tz),
            Due::Missed(slot)
        );
        monday.last_slot_at = Some(slot);
        assert_eq!(monday.due(at("2026-11-15T06:00:00Z"), &tz), Due::Nothing);
        // A slot before the last change never runs.
        monday.last_slot_at = None;
        monday.updated_at = at("2026-11-09T08:00:00Z");
        assert_eq!(monday.due(at("2026-11-09T09:00:00Z"), &tz), Due::Nothing);
        assert_eq!(
            monday.due(at("2026-11-16T06:00:00Z"), &tz),
            Due::Run(at("2026-11-16T06:00:00Z"))
        );
        monday.enabled = false;
        assert_eq!(monday.due(at("2026-11-16T06:00:00Z"), &tz), Due::Nothing);
    }

    #[test]
    fn every_day_has_exactly_one_slot_and_they_only_move_forward() {
        let tz = tz();
        for at_minute in [0, 59, 120, 150, 179, 180, 420, 1439] {
            let daily = job(Cadence::Daily, None, at_minute);
            let mut now = at("2026-01-01T00:00:00Z");
            let mut previous = daily.latest_slot(now, &tz).unwrap();
            let mut slots = 0;
            // Every hour through 2026, both transitions included.
            while now < at("2027-01-01T00:00:00Z") {
                now += time::Duration::hours(1);
                let slot = daily.latest_slot(now, &tz).unwrap();
                assert!(slot >= previous && slot <= now, "{at_minute} {now}");
                assert!(daily.next_slot(now, &tz).unwrap() > now);
                if slot != previous {
                    // A day apart, give or take the clock change.
                    assert!(slot - previous >= time::Duration::hours(23));
                    assert!(slot - previous <= time::Duration::hours(25));
                    slots += 1;
                }
                previous = slot;
            }
            assert_eq!(slots, 365, "{at_minute}");
        }
    }

    fn prompt(extra: serde_json::Value) -> serde_json::Value {
        let mut body = serde_json::json!({
            "kind": "prompt",
            "params": {"title": "AI news", "instruction": "Summarize this week's AI news"},
        });
        for (key, value) in extra.as_object().unwrap() {
            body[key] = value.clone();
        }
        body
    }

    #[test]
    fn new_kinds_take_only_their_params_within_bounds() {
        let job = new_job(prompt(serde_json::json!({}))).unwrap();
        assert_eq!((job.kind, job.params.notify), (Kind::Prompt, Some(true)));
        assert_eq!(
            (job.origin, job.expires_at, job.run_count),
            (Origin::Owner, None, 0)
        );
        let quiet = new_job(prompt(serde_json::json!({
            "params": {"title": "T", "instruction": "i".repeat(500), "notify": false}
        })))
        .unwrap();
        assert_eq!(quiet.params.notify, Some(false));
        let reminder = new_job(serde_json::json!({
            "kind": "reminder", "params": {"title": "Call the bank", "text": "Before 17:00."}
        }))
        .unwrap();
        assert_eq!(reminder.params.text.as_deref(), Some("Before 17:00."));
        let disk = new_job(serde_json::json!({"kind": "disk_check"})).unwrap();
        assert_eq!(disk.params.threshold_percent, Some(80));
        let title = |title: &str| serde_json::json!({"title": title, "instruction": "Search"});
        for bad in [
            serde_json::json!({"kind": "prompt"}),
            prompt(serde_json::json!({"params": {"title": "T"}})),
            prompt(serde_json::json!({"params": {"instruction": "Search"}})),
            prompt(serde_json::json!({"params": title("")})),
            prompt(serde_json::json!({"params": title("  ")})),
            prompt(serde_json::json!({"params": title(&"t".repeat(61))})),
            prompt(serde_json::json!({"params": title("Two\nlines")})),
            prompt(serde_json::json!({"params": title("Hidden\u{200B}")})),
            prompt(serde_json::json!({"params": {"title": "T", "instruction": "i".repeat(501)}})),
            prompt(serde_json::json!({"params": {"title": "T", "instruction": " "}})),
            prompt(serde_json::json!({"params": {"title": "T", "instruction": "a\u{202E}b"}})),
            prompt(serde_json::json!({"params": {"title": "T", "instruction": "S", "text": "x"}})),
            prompt(
                serde_json::json!({"params": {"title": "T", "instruction": "S", "prompt": "x"}}),
            ),
            prompt(serde_json::json!({"params": {
                "title": "T", "instruction": "S", "roadmap_id": "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b"
            }})),
            serde_json::json!({"kind": "reminder"}),
            serde_json::json!({"kind": "reminder", "params": {"title": "T", "text": ""}}),
            serde_json::json!({"kind": "reminder", "params": {"title": "T", "text": "t".repeat(501)}}),
            serde_json::json!({"kind": "reminder", "params": {"title": "T", "instruction": "S"}}),
            serde_json::json!({"kind": "reminder", "params": {"title": "T", "notify": false}}),
            serde_json::json!({"kind": "disk_check", "params": {"threshold_percent": 49}}),
            serde_json::json!({"kind": "disk_check", "params": {"threshold_percent": 100}}),
            serde_json::json!({"kind": "disk_check", "params": {"title": "Disk"}}),
            serde_json::json!({"kind": "dependency_review", "params": {"title": "Deps"}}),
            serde_json::json!({"kind": "dependency_review", "params": {"notify": true}}),
        ] {
            assert!(new_job(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn schedules_are_checked_together() {
        let monthly = new_job(prompt(serde_json::json!({"cadence": "monthly"}))).unwrap();
        assert_eq!((monthly.month_day, monthly.weekday), (Some(1), None));
        // Every N counts from today unless a start date is given.
        let every = new_job(prompt(
            serde_json::json!({"cadence": "monthly", "every": 6}),
        ))
        .unwrap();
        assert_eq!(every.start_date.as_deref(), Some("2026-11-11"));
        let once = new_job(prompt(
            serde_json::json!({"cadence": "once", "start_date": "2026-12-01", "at_minute": 540}),
        ))
        .unwrap();
        assert_eq!((once.every, once.max_runs), (1, None));
        for good in [
            serde_json::json!({"cadence": "daily", "every": 365}),
            serde_json::json!({"cadence": "weekly", "every": 104, "weekday": 7}),
            serde_json::json!({"cadence": "monthly", "every": 24, "month_day": 31}),
            serde_json::json!({"cadence": "daily", "max_runs": 100}),
            serde_json::json!({"cadence": "daily", "start_date": "2028-11-11"}),
        ] {
            assert!(new_job(prompt(good.clone())).is_ok(), "{good}");
        }
        for bad in [
            serde_json::json!({"cadence": "daily", "every": 0}),
            serde_json::json!({"cadence": "daily", "every": 366}),
            serde_json::json!({"cadence": "weekly", "every": 105}),
            serde_json::json!({"cadence": "monthly", "every": 25}),
            serde_json::json!({"cadence": "monthly", "month_day": 0}),
            serde_json::json!({"cadence": "monthly", "month_day": 32}),
            serde_json::json!({"cadence": "weekly", "month_day": 1}),
            serde_json::json!({"cadence": "monthly", "weekday": 1}),
            serde_json::json!({"cadence": "once"}),
            serde_json::json!({"cadence": "once", "weekday": 1, "start_date": "2026-12-01"}),
            serde_json::json!({"cadence": "once", "every": 2, "start_date": "2026-12-01"}),
            serde_json::json!({"cadence": "once", "max_runs": 1, "start_date": "2026-12-01"}),
            serde_json::json!({"cadence": "once", "start_date": "2026-11-10"}),
            serde_json::json!({"cadence": "daily", "max_runs": 0}),
            serde_json::json!({"cadence": "daily", "max_runs": 101}),
            serde_json::json!({"cadence": "daily", "start_date": "2026-1-5"}),
            serde_json::json!({"cadence": "daily", "start_date": "2026-02-30"}),
            serde_json::json!({"cadence": "daily", "start_date": "2026-11-13T00:00:00"}),
            serde_json::json!({"cadence": "daily", "start_date": "1999-12-31"}),
            serde_json::json!({"cadence": "daily", "start_date": "2028-11-13"}),
            serde_json::json!({"cadence": "hourly"}),
        ] {
            assert!(new_job(prompt(bad.clone())).is_err(), "{bad}");
        }
        // A dependency review keeps its daily and weekly schedules.
        for bad in [
            serde_json::json!({"kind": "dependency_review", "cadence": "weekly", "every": 2}),
            serde_json::json!({"kind": "dependency_review", "max_runs": 2}),
            serde_json::json!({"kind": "dependency_review", "start_date": "2026-12-01"}),
            serde_json::json!({"kind": "dependency_review", "cadence": "once", "start_date": "2026-12-01"}),
        ] {
            assert!(new_job(bad.clone()).is_err(), "{bad}");
        }
        // With the zone, a one-time job must be ahead of now to the minute.
        let at = |start_date: &str, at_minute: u16| {
            serde_json::from_value::<NewJob>(prompt(serde_json::json!({
                "cadence": "once", "start_date": start_date, "at_minute": at_minute
            })))
            .unwrap()
            .into_job("j".into(), now(), Some(&tz()))
        };
        assert!(at("2026-11-11", 780).is_err());
        assert!(at("2026-11-11", 781).is_ok());
        let disabled = serde_json::from_value::<NewJob>(prompt(serde_json::json!({
            "cadence": "once", "start_date": "2026-11-01", "enabled": false
        })))
        .unwrap()
        .into_job("j".into(), now(), Some(&tz()));
        assert!(disabled.is_ok());
    }

    #[test]
    fn server_set_fields_are_refused_and_jarvis_jobs_are_prompts_that_expire() {
        for field in [
            "origin",
            "owner_id",
            "conversation_id",
            "expires_at",
            "run_count",
            "failure_count",
            "paused_reason",
            "revision",
            "last_slot_at",
            "id",
        ] {
            let mut body = prompt(serde_json::json!({}));
            body[field] = serde_json::json!("jarvis");
            assert!(serde_json::from_value::<NewJob>(body).is_err(), "{field}");
            assert!(
                serde_json::from_value::<JobPatch>(serde_json::json!({ field: "jarvis" })).is_err(),
                "{field}"
            );
        }
        let input = || serde_json::from_value::<NewJob>(prompt(serde_json::json!({}))).unwrap();
        let jarvis = input().into_jarvis_job("j".into(), now(), None).unwrap();
        assert_eq!(jarvis.origin, Origin::Jarvis);
        assert_eq!(jarvis.expires_at, Some(at("2026-12-11T12:00:00Z")));
        let reminder = serde_json::from_value::<NewJob>(
            serde_json::json!({"kind": "reminder", "params": {"title": "T"}}),
        )
        .unwrap();
        assert!(reminder.into_jarvis_job("j".into(), now(), None).is_err());
        // The API shape survives a round trip.
        let wire = serde_json::to_value(&jarvis).unwrap();
        assert_eq!(wire["origin"], "jarvis");
        assert_eq!(wire["every"], 1);
        assert_eq!(wire["params"]["notify"], true);
        assert!(wire["params"].get("roadmap_id").is_none());
        assert_eq!(serde_json::from_value::<Job>(wire).unwrap(), jarvis);
    }

    #[test]
    fn patches_check_the_merged_schedule_and_reset_the_count() {
        let mut stored =
            new_job(prompt(serde_json::json!({"cadence": "daily", "every": 3}))).unwrap();
        stored.run_count = 2;
        let patch = |body: serde_json::Value| serde_json::from_value::<JobPatch>(body).unwrap();
        // A new cadence drops `every`; a new time keeps it.
        let (weekly, rescheduled) = stored
            .patched(
                &patch(serde_json::json!({"cadence": "weekly"})),
                now(),
                None,
            )
            .unwrap();
        assert_eq!(
            (weekly.every, weekly.weekday, weekly.run_count),
            (1, Some(1), 0)
        );
        assert!(rescheduled);
        let (later, _) = stored
            .patched(&patch(serde_json::json!({"at_minute": 600})), now(), None)
            .unwrap();
        assert_eq!(
            (later.every, later.start_date.as_deref()),
            (3, Some("2026-11-11"))
        );
        // Null clears the run limit and the start date; absent keeps them.
        let (mut limited, _) = stored
            .patched(&patch(serde_json::json!({"max_runs": 2})), now(), None)
            .unwrap();
        limited.run_count = 1;
        let (unlimited, _) = limited
            .patched(
                &patch(serde_json::json!({"max_runs": null, "every": 1, "start_date": null})),
                now(),
                None,
            )
            .unwrap();
        assert_eq!((unlimited.max_runs, unlimited.start_date), (None, None));
        let (kept, rescheduled) = limited
            .patched(
                &patch(serde_json::json!({"params": {"title": "New", "instruction": "Search"}})),
                now(),
                None,
            )
            .unwrap();
        assert_eq!(
            (kept.max_runs, kept.run_count, rescheduled),
            (Some(2), 1, false)
        );
        // A one-time job drops the run limit and needs its date.
        let (once, _) = limited
            .patched(
                &patch(serde_json::json!({"cadence": "once", "start_date": "2026-12-01"})),
                now(),
                None,
            )
            .unwrap();
        assert_eq!((once.every, once.max_runs), (1, None));
        for bad in [
            serde_json::json!({"cadence": "once", "start_date": null}),
            serde_json::json!({"cadence": "once", "start_date": "2026-12-01", "max_runs": 2}),
            serde_json::json!({"month_day": 3}),
            serde_json::json!({"every": 366}),
            serde_json::json!({"params": {"title": "T"}}),
            serde_json::json!({"params": {"title": "T", "instruction": "S", "threshold_percent": 80}}),
        ] {
            assert!(
                stored.patched(&patch(bad.clone()), now(), None).is_err(),
                "{bad}"
            );
        }
        // New params keep the stored notify and threshold they leave out.
        let mut quiet = stored.clone();
        quiet.params.notify = Some(false);
        let (renamed, _) = quiet
            .patched(
                &patch(serde_json::json!({"params": {"title": "New", "instruction": "Search"}})),
                now(),
                None,
            )
            .unwrap();
        assert_eq!(renamed.params.notify, Some(false));
        let (loud, _) = quiet
            .patched(
                &patch(serde_json::json!({"params": {"title": "N", "instruction": "S", "notify": true}})),
                now(),
                None,
            )
            .unwrap();
        assert_eq!(loud.params.notify, Some(true));
        let disk =
            new_job(serde_json::json!({"kind": "disk_check", "params": {"threshold_percent": 90}}))
                .unwrap();
        let (kept, _) = disk
            .patched(&patch(serde_json::json!({"params": {}})), now(), None)
            .unwrap();
        assert_eq!(kept.params.threshold_percent, Some(90));
        // Enabling clears a pause; a disabled one-time job in the past may
        // be edited but not enabled again.
        let mut paused = stored.clone();
        paused.enabled = false;
        paused.failure_count = 3;
        paused.paused_reason = Some(PausedReason::PausedAfterFailures);
        let (resumed, _) = paused
            .patched(&patch(serde_json::json!({"enabled": true})), now(), None)
            .unwrap();
        assert_eq!((resumed.failure_count, resumed.paused_reason), (0, None));
        let mut done = once.clone();
        done.enabled = false;
        done.start_date = Some("2026-11-01".into());
        assert!(done
            .patched(
                &patch(serde_json::json!({"params": {"title": "New", "instruction": "Search"}})),
                now(),
                Some(&tz())
            )
            .is_ok());
        assert!(done
            .patched(
                &patch(serde_json::json!({"enabled": true})),
                now(),
                Some(&tz())
            )
            .is_err());
    }

    /// The local dates of a job's slots from `from`, `count` of them.
    fn slot_dates(job: &Job, from: &str, count: usize) -> Vec<String> {
        let tz = tz();
        let mut now = at(from);
        let mut dates = Vec::new();
        for _ in 0..count {
            let Some(slot) = job.next_slot(now, &tz) else {
                break;
            };
            // Each slot is also the newest one at its own time.
            assert_eq!(job.latest_slot(slot, &tz), Some(slot));
            dates.push(tz.to_datetime(to_timestamp(slot).unwrap()).to_string());
            now = slot;
        }
        dates
    }

    #[test]
    fn monthly_slots_clamp_to_the_month_end() {
        let mut last = job(Cadence::Monthly, None, 600);
        last.month_day = Some(31);
        assert_eq!(
            slot_dates(&last, "2027-12-01T00:00:00Z", 4),
            [
                "2027-12-31T10:00:00",
                "2028-01-31T10:00:00",
                "2028-02-29T10:00:00",
                "2028-03-31T10:00:00"
            ]
        );
        last.month_day = Some(30);
        assert_eq!(
            slot_dates(&last, "2027-01-31T00:00:00Z", 2),
            ["2027-02-28T10:00:00", "2027-03-30T10:00:00"]
        );
        // Between two slots the newest is last month's.
        assert_eq!(
            last.latest_slot(at("2027-03-15T00:00:00Z"), &tz()),
            Some(at("2027-02-28T09:00:00Z"))
        );
        // On the spring-forward day 02:30 runs at 03:30.
        let mut spring = job(Cadence::Monthly, None, 150);
        spring.month_day = Some(29);
        assert_eq!(
            spring.next_slot(at("2026-03-01T00:00:00Z"), &tz()),
            Some(at("2026-03-29T01:30:00Z"))
        );
    }

    #[test]
    fn every_n_counts_from_the_start_date() {
        let mut half_year = job(Cadence::Monthly, None, 540);
        half_year.month_day = Some(13);
        half_year.every = 6;
        half_year.start_date = Some("2026-11-13".into());
        assert_eq!(
            slot_dates(&half_year, "2026-01-01T00:00:00Z", 3),
            [
                "2026-11-13T09:00:00",
                "2027-05-13T09:00:00",
                "2027-11-13T09:00:00"
            ]
        );
        // A month day before the start date counts from the next month.
        half_year.start_date = Some("2026-11-20".into());
        assert_eq!(
            slot_dates(&half_year, "2026-11-01T00:00:00Z", 2),
            ["2026-12-13T09:00:00", "2027-06-13T09:00:00"]
        );
        assert_eq!(
            half_year.latest_slot(at("2027-06-01T00:00:00Z"), &tz()),
            Some(at("2026-12-13T08:00:00Z"))
        );
        assert_eq!(
            half_year.latest_slot(at("2026-12-01T00:00:00Z"), &tz()),
            None
        );
        let mut three_days = job(Cadence::Daily, None, 420);
        three_days.every = 3;
        three_days.start_date = Some("2026-03-28".into());
        // Across the spring-forward night.
        assert_eq!(
            slot_dates(&three_days, "2026-03-01T00:00:00Z", 3),
            [
                "2026-03-28T07:00:00",
                "2026-03-31T07:00:00",
                "2026-04-03T07:00:00"
            ]
        );
        let mut fortnight = job(Cadence::Weekly, Some(5), 1439);
        fortnight.every = 2;
        // Wednesday: the first Friday is two days later.
        fortnight.start_date = Some("2026-10-21".into());
        assert_eq!(
            slot_dates(&fortnight, "2026-10-01T00:00:00Z", 3),
            [
                "2026-10-23T23:59:00",
                "2026-11-06T23:59:00",
                "2026-11-20T23:59:00"
            ]
        );
    }

    #[test]
    fn a_one_time_job_runs_once_and_a_run_limit_ends_a_job() {
        let tz = tz();
        let mut once = Job {
            kind: Kind::Reminder,
            ..job(Cadence::Once, None, 150)
        };
        once.start_date = Some("2026-10-25".into());
        // Fall back: 02:30 occurs twice and runs once, at the first.
        let slot = at("2026-10-25T00:30:00Z");
        assert_eq!(once.next_slot(at("2026-01-01T00:00:00Z"), &tz), Some(slot));
        assert_eq!(once.next_slot(slot, &tz), None);
        assert_eq!(once.latest_slot(at("2026-10-25T00:29:00Z"), &tz), None);
        assert_eq!(
            once.latest_slot(at("2027-10-25T00:00:00Z"), &tz),
            Some(slot)
        );
        assert_eq!(
            once.due(slot, &tz),
            Due::Nothing,
            "not a kind that runs yet"
        );
        once.kind = Kind::DependencyReview;
        assert_eq!(once.due(slot, &tz), Due::Run(slot));
        assert_eq!(once.due(at("2027-01-01T00:00:00Z"), &tz), Due::Missed(slot));
        assert!(once.ends_with(RunStatus::Running) && once.ends_with(RunStatus::Missed));
        // Two runs: the second ends the job, a missed slot does not count.
        let mut twice = job(Cadence::Daily, None, 420);
        twice.max_runs = Some(2);
        assert!(!twice.ends_with(RunStatus::Running));
        twice.run_count = 1;
        assert!(twice.ends_with(RunStatus::Running) && !twice.ends_with(RunStatus::Missed));
        let today = at("2026-11-11T06:00:00Z");
        assert_eq!(twice.due(today, &tz), Due::Run(today));
        twice.run_count = 2;
        assert_eq!(twice.due(today, &tz), Due::Nothing);
        assert!(!job(Cadence::Daily, None, 420).ends_with(RunStatus::Running));
    }

    #[test]
    fn the_runner_skips_kinds_it_does_not_run_yet_and_expired_jobs() {
        let tz = tz();
        let today = at("2026-11-11T06:00:00Z");
        let daily = job(Cadence::Daily, None, 420);
        assert_eq!(daily.due(today, &tz), Due::Run(today));
        for kind in [Kind::Prompt, Kind::Reminder, Kind::DiskCheck] {
            assert!(!kind.runnable());
            let skipped = Job {
                kind,
                ..daily.clone()
            };
            assert_eq!(skipped.due(today, &tz), Due::Nothing);
            assert_eq!(
                skipped.due(today + time::Duration::days(2), &tz),
                Due::Nothing
            );
        }
        assert!(Kind::DependencyReview.runnable());
        let expired = Job {
            expires_at: Some(today),
            ..daily.clone()
        };
        assert_eq!(expired.due(today, &tz), Due::Nothing);
    }

    async fn database() -> std::result::Result<Database, Box<dyn std::error::Error>> {
        use surrealdb::{engine::remote::ws::Ws, opt::auth::Root, Surreal};
        let db = Surreal::new::<Ws>(std::env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
        db.signin(Root {
            username: &std::env::var("JARVIS_SURREAL_TEST_USER")?,
            password: &std::env::var("JARVIS_SURREAL_TEST_PASS")?,
        })
        .await?;
        db.use_ns(format!("jobs_{}", uuid::Uuid::now_v7().simple()))
            .use_db("core")
            .await?;
        jarvis_store::apply_baseline_schema(&db).await?;
        Ok(db)
    }

    /// The `jobs.run_finished` statuses `listener` heard since the last call.
    fn heard(listener: &mut crate::realtime::Subscription) -> Vec<String> {
        std::iter::from_fn(|| listener.receiver.try_recv().ok())
            .map(|envelope| match &envelope.event {
                Event::JobRunFinished { status, .. } => status.clone(),
                other => panic!("unexpected event {other:?}"),
            })
            .collect()
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn the_runner_writes_rows_first_and_handles_each_slot_once(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = database().await?;
        let hub = Hub::default();
        let mut listener = hub
            .subscribe(uuid::Uuid::now_v7(), uuid::Uuid::now_v7())
            .ok_or("subscription")?;
        listener.receiver.try_recv()?;
        let handle = Arc::new(JobsHandle::new());
        let runner = Runner {
            db: db.clone(),
            handle: handle.clone(),
            events: hub.clone(),
            tz: tz(),
            work: Work {
                github: Arc::default(),
                token: None,
                brain: Arc::new(jarvis_llm::ClaudeCliProvider::new("unused", "m", "m", "m")),
            },
        };
        let input = serde_json::json!({"kind": "dependency_review", "cadence": "daily"});
        let job = store::create(&db, serde_json::from_value(input)?, None).await?;

        // A run an earlier Core left running is interrupted first.
        let stale = store::start_run(
            &db,
            &job,
            at("2026-11-01T06:00:00Z"),
            Trigger::Owner,
            RunStatus::Running,
        )
        .await?;
        // Started over an hour ago: it does not hold the owner's hour.
        db.query("UPDATE scheduled_job_runs SET started_at = time::now() - 2h")
            .await?
            .check()?;
        assert!(runner.recover().await);
        let status = |id: String| {
            let db = db.clone();
            async move {
                let (run, _) = store::get_run(&db, &id).await?.ok_or("run")?;
                Ok::<_, Box<dyn std::error::Error>>((run.status, run.error_code))
            }
        };
        assert_eq!(
            status(stale).await?,
            (RunStatus::Interrupted, Some("interrupted".into()))
        );

        // A day after the job was made, its newest slot runs once. Without
        // a GitHub token the run fails, and is announced.
        let now = job.updated_at + time::Duration::hours(26);
        let slot = job.latest_slot(now, &tz()).ok_or("slot")?;
        runner.pass(now).await;
        runner.pass(now).await;
        let runs = store::list_runs(&db, &job.id, 10).await?;
        let ran: Vec<_> = runs
            .iter()
            .filter(|run| run.trigger == Trigger::Schedule)
            .map(|run| (run.slot_at, run.status, run.error_code.as_deref()))
            .collect();
        assert_eq!(ran, [(slot, RunStatus::Failed, Some("not_configured"))]);
        assert_eq!(heard(&mut listener), ["failed"]);
        let job = store::get(&db, &job.id).await?.ok_or("job")?;
        assert_eq!(job.last_slot_at, Some(slot));

        // Weekly on the weekday three days before a pass ten days on: that
        // slot is missed, once.
        let later = job.updated_at + time::Duration::days(10);
        let weekday = tz()
            .to_datetime(to_timestamp(later - time::Duration::days(3)).ok_or("time")?)
            .date()
            .weekday()
            .to_monday_one_offset();
        let weekly = store::update(
            &db,
            &job.id,
            job.revision,
            &JobPatch {
                cadence: Some(Cadence::Weekly),
                weekday: Some(u8::try_from(weekday)?),
                ..JobPatch::default()
            },
            None,
        )
        .await?;
        let later = weekly.updated_at + time::Duration::days(10);
        let missed = weekly.latest_slot(later, &tz()).ok_or("slot")?;
        runner.pass(later).await;
        runner.pass(later).await;
        assert_eq!(heard(&mut listener), ["missed"]);
        let runs = store::list_runs(&db, &job.id, 10).await?;
        assert_eq!(
            runs.iter()
                .filter(|run| run.status == RunStatus::Missed)
                .map(|run| run.slot_at)
                .collect::<Vec<_>>(),
            [missed]
        );

        // The owner's run: one at a time, and once an hour.
        handle.request_run(&job.id)?;
        assert!(handle.request_run(&job.id).is_err());
        runner.pass(later).await;
        assert_eq!(heard(&mut listener), ["failed"]);
        handle.request_run(&job.id)?;
        runner.pass(later).await;
        assert!(heard(&mut listener).is_empty());
        let owner = store::list_runs(&db, &job.id, 10)
            .await?
            .into_iter()
            .filter(|run| run.trigger == Trigger::Owner && run.status != RunStatus::Interrupted)
            .count();
        assert_eq!(owner, 1);
        Ok(())
    }
}
