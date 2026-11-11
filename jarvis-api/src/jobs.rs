//! Scheduled jobs (schema 0019): work Core runs by itself on a daily or
//! weekly schedule, and a record of every run.
//!
//! A job's kind is a closed list; there are no free-text jobs. This module
//! holds the types, input validation and the slot math: a job's slots are
//! wall-clock minutes in [`TIME_ZONE`], so they follow daylight saving time.
//! A minute that does not exist on a spring-forward day moves forward by the
//! gap (02:30 becomes 03:30); a minute that occurs twice on a fall-back day
//! runs once, at its first occurrence.

pub mod store;

use std::time::Duration;

use jiff::tz::TimeZone;
use jiff::{civil, Timestamp};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

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
/// Monday 07:00, the default of a new job.
const DEFAULT_WEEKDAY: u8 = 1;
const DEFAULT_AT_MINUTE: u16 = 7 * 60;

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cadence {
    Daily,
    Weekly,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummarySource {
    Claude,
    Deterministic,
}

/// Typed parameters; unknown fields are refused.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
    /// The roadmap a run proposes its items to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roadmap_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub kind: Kind,
    pub cadence: Cadence,
    /// ISO weekday (Monday = 1) of a weekly job.
    #[serde(default)]
    pub weekday: Option<u8>,
    /// Minutes after midnight in [`TIME_ZONE`].
    pub at_minute: u16,
    pub enabled: bool,
    pub revision: u64,
    #[serde(default)]
    pub params: Params,
    /// The newest slot the runner handled; none since a schedule change.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_slot_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// A new job; absent fields default to weekly on Monday at 07:00, enabled.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewJob {
    pub kind: Kind,
    #[serde(default)]
    pub cadence: Option<Cadence>,
    #[serde(default)]
    pub weekday: Option<u8>,
    #[serde(default)]
    pub at_minute: Option<u16>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub params: Params,
}

/// Absent fields stay. A daily job has no weekday; a job that becomes weekly
/// keeps its weekday or gets Monday.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobPatch {
    #[serde(default)]
    pub cadence: Option<Cadence>,
    #[serde(default)]
    pub weekday: Option<u8>,
    #[serde(default)]
    pub at_minute: Option<u16>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub params: Option<Params>,
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
/// is given), a daily job none.
fn weekday_for(cadence: Cadence, given: Option<u8>, fallback: u8) -> Result<Option<u8>> {
    match cadence {
        Cadence::Daily => {
            check(given.is_none(), "a daily job has no weekday")?;
            Ok(None)
        }
        Cadence::Weekly => {
            let weekday = given.unwrap_or(fallback);
            check(
                (1..=7).contains(&weekday),
                "weekday must be 1 (Monday) to 7 (Sunday)",
            )?;
            Ok(Some(weekday))
        }
    }
}

fn validate_at_minute(at_minute: u16) -> Result<()> {
    check(at_minute <= LAST_MINUTE, "at_minute must be 0 to 1439")
}

fn validate_params(params: &Params) -> Result<()> {
    check(
        params.roadmap_id.as_deref().is_none_or(|id| {
            uuid::Uuid::parse_str(id).is_ok_and(|uuid| uuid.hyphenated().to_string() == id)
        }),
        "params.roadmap_id must be a roadmap id",
    )
}

impl NewJob {
    /// The job this input creates, at revision 1. With the zone, its newest
    /// slot at `now` counts as handled, so only later slots run.
    pub fn into_job(self, id: String, now: OffsetDateTime, tz: Option<&TimeZone>) -> Result<Job> {
        let cadence = self.cadence.unwrap_or(Cadence::Weekly);
        let weekday = weekday_for(cadence, self.weekday, DEFAULT_WEEKDAY)?;
        let at_minute = self.at_minute.unwrap_or(DEFAULT_AT_MINUTE);
        validate_at_minute(at_minute)?;
        validate_params(&self.params)?;
        let mut job = Job {
            id,
            kind: self.kind,
            cadence,
            weekday,
            at_minute,
            enabled: self.enabled.unwrap_or(true),
            revision: 1,
            params: self.params,
            last_slot_at: None,
            created_at: now,
            updated_at: now,
        };
        job.last_slot_at = tz.and_then(|tz| job.latest_slot(now, tz));
        Ok(job)
    }
}

impl Job {
    /// The job with `patch` applied, and whether its schedule (cadence,
    /// weekday, time or enabled) changed. A change sets `last_slot_at` to the
    /// newest slot at `now` under the new schedule (none without the zone),
    /// so no slot before the change runs.
    pub fn patched(
        &self,
        patch: &JobPatch,
        now: OffsetDateTime,
        tz: Option<&TimeZone>,
    ) -> Result<(Job, bool)> {
        let cadence = patch.cadence.unwrap_or(self.cadence);
        let weekday = weekday_for(
            cadence,
            patch.weekday,
            self.weekday.unwrap_or(DEFAULT_WEEKDAY),
        )?;
        let at_minute = patch.at_minute.unwrap_or(self.at_minute);
        validate_at_minute(at_minute)?;
        let params = patch.params.clone().unwrap_or_else(|| self.params.clone());
        validate_params(&params)?;
        let mut new = Job {
            cadence,
            weekday,
            at_minute,
            enabled: patch.enabled.unwrap_or(self.enabled),
            params,
            ..self.clone()
        };
        let rescheduled = (new.cadence, new.weekday, new.at_minute, new.enabled)
            != (self.cadence, self.weekday, self.at_minute, self.enabled);
        if rescheduled {
            new.last_slot_at = tz.and_then(|tz| new.latest_slot(now, tz));
        }
        Ok((new, rescheduled))
    }

    /// The job's slot on `date`, if it has one that day.
    fn slot_on(&self, date: civil::Date, tz: &TimeZone) -> Option<Timestamp> {
        let iso_weekday = u8::try_from(date.weekday().to_monday_one_offset()).ok();
        if self
            .weekday
            .is_some_and(|weekday| Some(weekday) != iso_weekday)
        {
            return None;
        }
        let minute = i8::try_from(self.at_minute % 60).ok()?;
        let hour = i8::try_from(self.at_minute / 60).ok()?;
        let time = civil::Time::new(hour, minute, 0, 0).ok()?;
        tz.to_timestamp(date.to_datetime(time)).ok()
    }

    /// The newest slot at or before `now`.
    pub fn latest_slot(&self, now: OffsetDateTime, tz: &TimeZone) -> Option<OffsetDateTime> {
        let now = to_timestamp(now)?;
        let mut date = tz.to_datetime(now).date();
        // A weekly slot is at most a week back; one more day covers a slot
        // later today than now.
        for _ in 0..=7 {
            if let Some(slot) = self.slot_on(date, tz).filter(|slot| *slot <= now) {
                return from_timestamp(slot);
            }
            date = date.yesterday().ok()?;
        }
        None
    }

    /// The first slot after `now`.
    pub fn next_slot(&self, now: OffsetDateTime, tz: &TimeZone) -> Option<OffsetDateTime> {
        let now = to_timestamp(now)?;
        let mut date = tz.to_datetime(now).date();
        for _ in 0..=7 {
            if let Some(slot) = self.slot_on(date, tz).filter(|slot| *slot > now) {
                return from_timestamp(slot);
            }
            date = date.tomorrow().ok()?;
        }
        None
    }

    /// What the runner owes this job at `now`: nothing, a run for the newest
    /// slot (late by less than [`CATCH_UP`]), or one `missed` row for it.
    /// Only the newest slot counts: older slots that passed while Core was
    /// down leave no row. Slots at or before `last_slot_at` are handled;
    /// without it (a change made while the zone was unavailable), slots at
    /// or before the last change (`updated_at`) never run.
    pub fn due(&self, now: OffsetDateTime, tz: &TimeZone) -> Due {
        if !self.enabled {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    Nothing,
    Run(OffsetDateTime),
    Missed(OffsetDateTime),
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
            at_minute,
            enabled: true,
            revision: 1,
            params: Params::default(),
            last_slot_at: None,
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
}
