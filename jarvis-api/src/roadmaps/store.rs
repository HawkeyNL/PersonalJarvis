//! Typed repository for roadmaps (schema 0018).
//!
//! Every write loads the roadmap, applies the change to a [`Draft`] and
//! commits it in one transaction that bumps the revision only if it is still
//! the one loaded, and writes one `roadmap_changes` row per change. Two
//! writers can therefore never both build on the same revision: the second
//! commit finds another revision (or SurrealDB refuses the conflicting
//! transaction) and reports [`RoadmapError::Conflict`]. All values are bound
//! parameters; only constants are formatted into queries.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::OffsetDateTime;

use jarvis_client_core::realtime::Event;
use jarvis_store::Database;

use super::github::GithubHandle;
use super::{
    added_links, check, text, validate_new_roadmap, Action, Change, Draft, Item, ItemPatch,
    NewItem, NewRoadmap, ProposalOp, Result, Roadmap, RoadmapError, RoadmapPatch, Snapshot, Status,
    HISTORY_RETENTION_DAYS, MAX_HISTORY, MAX_ITEMS, MAX_OP_BYTES, MAX_PENDING_PROPOSALS,
    MAX_PROPOSAL_OPS, MAX_RATIONALE, MAX_ROADMAPS, PROPOSAL_TTL_DAYS,
};
use crate::realtime::Hub;

/// Rows deleted per statement and statements per table per sweep.
const PRUNE_BATCH: usize = 5_000;
const PRUNE_MAX_BATCHES: usize = 20;

const ROADMAP_FIELDS: &str =
    "record::id(id) AS id, title, description, phases, revision, created_at, updated_at, archived_at";
const ITEM_FIELDS: &str = "record::id(id) AS id, title, status, auto_status, phase, position, \
     notes, depends_on, links, updated_at";
const PENDING: &str = "status = 'pending' AND expires_at > time::now()";
/// Writes `$changes` for `$id` at `$revision` by `$author`.
const WRITE_CHANGES: &str = "FOR $c IN $changes { CREATE roadmap_changes SET id = $c.id, \
     roadmap_id = $id, revision = $revision, at = time::now(), author = $author, \
     device_id = $device, proposal_id = $proposal, action = $c.action, item_id = $c.item_id, \
     title = $c.title, fields = $c.fields, from_status = $c.from_status, \
     to_status = $c.to_status RETURN NONE; };";

async fn run(db: &Database, query: &str, bindings: Value) -> Result<surrealdb::Response> {
    Ok(db.query(query).bind(bindings).await?.check()?)
}

async fn rows<T: DeserializeOwned>(db: &Database, query: &str, bindings: Value) -> Result<Vec<T>> {
    Ok(run(db, query, bindings).await?.take(0)?)
}

fn changes_json(changes: &[Change]) -> Result<Value> {
    changes
        .iter()
        .map(|change| {
            let mut value = serde_json::to_value(change).map_err(|_| RoadmapError::InvalidData)?;
            value["id"] = uuid::Uuid::now_v7().to_string().into();
            Ok(value)
        })
        .collect::<Result<Vec<_>>>()
        .map(Value::from)
}

/// Tell connected apps that roadmap `id` committed `revision`; they reload it.
fn announce(events: &Hub, id: &str, revision: u64) {
    events.publish_all(Event::RoadmapChanged {
        id: id.to_owned(),
        revision,
    });
}

/// Roadmaps that are not archived, oldest first; or the newest archived ones.
pub async fn list(db: &Database, archived: bool) -> Result<Vec<Roadmap>> {
    let query = if archived {
        format!(
            "SELECT {ROADMAP_FIELDS} FROM roadmaps WHERE archived_at != NONE \
             ORDER BY archived_at DESC LIMIT {MAX_HISTORY}"
        )
    } else {
        format!(
            "SELECT {ROADMAP_FIELDS} FROM roadmaps WHERE archived_at = NONE \
             ORDER BY created_at LIMIT {MAX_ROADMAPS}"
        )
    };
    rows(db, &query, json!({})).await
}

async fn roadmap(db: &Database, id: &str) -> Result<Option<Roadmap>> {
    let found: Vec<Roadmap> = rows(
        db,
        &format!("SELECT {ROADMAP_FIELDS} FROM roadmaps WHERE record::id(id) = $id LIMIT 1"),
        json!({"id": id}),
    )
    .await?;
    Ok(found.into_iter().next())
}

/// A roadmap (also an archived one) with its items.
pub async fn get(db: &Database, id: &str) -> Result<Option<Snapshot>> {
    // The roadmap first: items read after it are at least as new, so a draft
    // built on newer items fails its revision check instead of committing
    // over them.
    let Some(roadmap) = roadmap(db, id).await? else {
        return Ok(None);
    };
    let items: Vec<Item> = rows(
        db,
        &format!(
            "SELECT {ITEM_FIELDS} FROM roadmap_items WHERE roadmap_id = $id \
             ORDER BY position, id LIMIT {MAX_ITEMS}"
        ),
        json!({"id": id}),
    )
    .await?;
    Ok(Some(Snapshot { roadmap, items }))
}

/// Create a roadmap. At most [`MAX_ROADMAPS`] are not archived.
// ponytail: the count and the insert touch no shared key, so two concurrent
// creates can end at 21; add a counter row both write if that ever matters.
pub async fn create(
    db: &Database,
    input: &NewRoadmap,
    device_id: &str,
    events: &Hub,
) -> Result<Snapshot> {
    validate_new_roadmap(input)?;
    let id = uuid::Uuid::now_v7().to_string();
    let changes = [Change {
        action: Action::RoadmapCreated,
        item_id: None,
        title: input.title.clone(),
        fields: Vec::new(),
        from_status: None,
        to_status: None,
    }];
    let response = db
        .query(format!(
            "BEGIN TRANSACTION; \
             IF array::len((SELECT VALUE id FROM roadmaps WHERE archived_at = NONE)) >= {MAX_ROADMAPS} \
               {{ THROW 'roadmap limit reached'; }}; \
             CREATE roadmaps SET id = $id, title = $title, description = $description, \
               phases = $phases, revision = $revision, created_at = time::now(), \
               updated_at = time::now() RETURN NONE; \
             {WRITE_CHANGES} \
             COMMIT TRANSACTION;"
        ))
        .bind(json!({
            "id": id,
            "title": input.title,
            "description": input.description,
            "phases": input.phases,
            "revision": 1,
            "changes": changes_json(&changes)?,
            "author": "owner",
            "device": device_id,
            "proposal": None::<String>,
        }))
        .await?;
    if let Err(error) = response.check() {
        if list(db, false).await?.len() >= MAX_ROADMAPS {
            return Err(RoadmapError::Limit("at most 20 roadmaps"));
        }
        return Err(error.into());
    }
    announce(events, &id, 1);
    get(db, &id).await?.ok_or(RoadmapError::InvalidData)
}

/// The roadmap as a draft, if it may be changed at `expected` (any revision
/// when `None`).
async fn load(db: &Database, id: &str, expected: Option<u64>) -> Result<Draft> {
    let snapshot = get(db, id).await?.ok_or(RoadmapError::NotFound)?;
    if snapshot.roadmap.archived_at.is_some() {
        return Err(RoadmapError::Archived);
    }
    if expected.is_some_and(|expected| expected != snapshot.roadmap.revision) {
        return Err(RoadmapError::Conflict);
    }
    Ok(Draft::new(snapshot))
}

async fn proposal_pending(db: &Database, id: &str, proposal_id: &str) -> Result<bool> {
    let found: Vec<String> = rows(
        db,
        &format!(
            "SELECT VALUE record::id(id) FROM roadmap_proposals \
             WHERE record::id(id) = $pid AND roadmap_id = $id AND {PENDING}"
        ),
        json!({"id": id, "pid": proposal_id}),
    )
    .await?;
    Ok(!found.is_empty())
}

/// Commit a non-empty draft at the revision it was loaded at. The changes
/// are the owner's from `device_id`; with a proposal they are Jarvis's,
/// accepted from `device_id`, and the proposal is marked accepted in the same
/// transaction; without a device they are the GitHub poller's. Once
/// committed, `events` hears `roadmap.changed`.
async fn commit(
    db: &Database,
    draft: &Draft,
    device_id: Option<&str>,
    proposal_id: Option<&str>,
    events: &Hub,
) -> Result<()> {
    draft.finish()?;
    let writes = draft.writes();
    let expected = draft.roadmap.revision;
    let id = &draft.roadmap.id;
    let response = db
        .query(format!(
            "BEGIN TRANSACTION; \
             LET $bumped = (UPDATE roadmaps SET revision = $revision, updated_at = time::now(), \
               title = $title, description = $description, phases = $phases, \
               archived_at = IF $archive THEN time::now() ELSE NONE END \
               WHERE record::id(id) = $id AND revision = $expected AND archived_at = NONE \
               RETURN VALUE revision); \
             IF array::len($bumped) != 1 {{ THROW 'roadmap revision conflict'; }}; \
             DELETE roadmap_items WHERE roadmap_id = $id AND record::id(id) IN $deletes RETURN NONE; \
             FOR $item IN $updates {{ UPDATE roadmap_items SET title = $item.title, \
               status = $item.status, auto_status = $item.auto_status, phase = $item.phase, \
               position = $item.position, notes = $item.notes, depends_on = $item.depends_on, \
               links = $item.links, updated_at = time::now() \
               WHERE roadmap_id = $id AND record::id(id) = $item.id RETURN NONE; }}; \
             FOR $item IN $creates {{ CREATE roadmap_items SET id = $item.id, roadmap_id = $id, \
               title = $item.title, status = $item.status, phase = $item.phase, \
               position = $item.position, notes = $item.notes, depends_on = $item.depends_on, \
               links = $item.links, created_at = time::now(), updated_at = time::now() \
               RETURN NONE; }}; \
             {WRITE_CHANGES} \
             LET $decided = (UPDATE roadmap_proposals SET status = 'accepted', \
               decided_at = time::now(), decided_by = $device \
               WHERE $accepting AND record::id(id) = $proposal AND roadmap_id = $id \
               AND {PENDING} RETURN VALUE id); \
             IF $accepting AND array::len($decided) != 1 \
               {{ THROW 'proposal unavailable'; }}; \
             COMMIT TRANSACTION;"
        ))
        .bind(json!({
            "id": id,
            "expected": expected,
            "revision": expected + 1,
            "title": draft.roadmap.title,
            "description": draft.roadmap.description,
            "phases": draft.roadmap.phases,
            "archive": draft.archive,
            "deletes": writes.deletes,
            "updates": writes.updates,
            "creates": writes.creates,
            "changes": changes_json(&draft.changes)?,
            "author": match (device_id, proposal_id) {
                (_, Some(_)) => "jarvis",
                (Some(_), None) => "owner",
                (None, None) => "github",
            },
            "device": device_id,
            "proposal": proposal_id,
            "accepting": proposal_id.is_some(),
        }))
        .await?;
    let Err(error) = response.check() else {
        announce(events, id, expected + 1);
        return Ok(());
    };
    // Explain the failure from the current state; anything else is a
    // database error.
    match roadmap(db, id).await? {
        None => Err(RoadmapError::NotFound),
        Some(current) if current.archived_at.is_some() => Err(RoadmapError::Archived),
        Some(current) if current.revision != expected => Err(RoadmapError::Conflict),
        Some(_) => match proposal_id {
            Some(pid) if !proposal_pending(db, id, pid).await? => Err(RoadmapError::NotFound),
            _ => Err(error.into()),
        },
    }
}

/// Commit the owner's draft (nothing when it changes nothing) and return the
/// roadmap as stored.
async fn save(db: &Database, draft: Draft, device_id: &str, events: &Hub) -> Result<Snapshot> {
    if !draft.is_empty() {
        commit(db, &draft, Some(device_id), None, events).await?;
    }
    get(db, &draft.roadmap.id)
        .await?
        .ok_or(RoadmapError::NotFound)
}

/// Commit the GitHub poller's draft (see [`Draft::github_update`]) as
/// author `github`, at the revision it was loaded at: when the owner changed
/// the roadmap meanwhile, nothing is written and the answer is
/// [`RoadmapError::Conflict`].
pub async fn commit_github(db: &Database, draft: &Draft, events: &Hub) -> Result<()> {
    if draft.is_empty() {
        return Ok(());
    }
    commit(db, draft, None, None, events).await
}

pub async fn update(
    db: &Database,
    id: &str,
    expected: u64,
    patch: &RoadmapPatch,
    device_id: &str,
    events: &Hub,
) -> Result<Snapshot> {
    let mut draft = load(db, id, Some(expected)).await?;
    draft.update_roadmap(patch)?;
    save(db, draft, device_id, events).await
}

/// Archive a roadmap; it stays readable but no longer changes.
pub async fn archive(
    db: &Database,
    id: &str,
    expected: u64,
    device_id: &str,
    events: &Hub,
) -> Result<Snapshot> {
    let mut draft = load(db, id, Some(expected)).await?;
    draft.archive();
    save(db, draft, device_id, events).await
}

/// Add an item; returns its id and the roadmap.
pub async fn create_item(
    db: &Database,
    id: &str,
    expected: u64,
    input: &NewItem,
    device_id: &str,
    events: &Hub,
) -> Result<(String, Snapshot)> {
    let mut draft = load(db, id, Some(expected)).await?;
    let item_id = draft.create_item(input)?;
    Ok((item_id, save(db, draft, device_id, events).await?))
}

#[allow(clippy::too_many_arguments)]
pub async fn update_item(
    db: &Database,
    id: &str,
    expected: u64,
    item_id: &str,
    patch: &ItemPatch,
    device_id: &str,
    github: &GithubHandle,
    events: &Hub,
) -> Result<Snapshot> {
    let mut draft = load(db, id, Some(expected)).await?;
    let before = draft.items.clone();
    draft.update_item(item_id, patch)?;
    github
        .check_links(added_links(&before, &draft.items))
        .map_err(RoadmapError::Invalid)?;
    save(db, draft, device_id, events).await
}

pub async fn delete_item(
    db: &Database,
    id: &str,
    expected: u64,
    item_id: &str,
    device_id: &str,
    events: &Hub,
) -> Result<Snapshot> {
    let mut draft = load(db, id, Some(expected)).await?;
    draft.delete_item(item_id)?;
    save(db, draft, device_id, events).await
}

/// One audited change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: String,
    pub revision: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub author: String,
    pub device_id: Option<String>,
    pub proposal_id: Option<String>,
    pub action: Action,
    pub item_id: Option<String>,
    pub title: String,
    pub fields: Vec<String>,
    pub from_status: Option<Status>,
    pub to_status: Option<Status>,
}

/// The newest `limit` changes (1 to [`MAX_HISTORY`]), newest first.
pub async fn history(db: &Database, id: &str, limit: usize) -> Result<Vec<HistoryEntry>> {
    if roadmap(db, id).await?.is_none() {
        return Err(RoadmapError::NotFound);
    }
    rows(
        db,
        "SELECT record::id(id) AS id, revision, at, author, device_id, proposal_id, action, \
         item_id, title, fields, from_status, to_status FROM roadmap_changes \
         WHERE roadmap_id = $id ORDER BY revision DESC, id DESC LIMIT $limit",
        json!({"id": id, "limit": limit.clamp(1, MAX_HISTORY)}),
    )
    .await
}

/// A pending proposal.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Proposal {
    pub id: String,
    pub base_revision: u64,
    pub ops: Vec<ProposalOp>,
    pub rationale: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}

#[derive(Deserialize)]
struct ProposalRow {
    id: String,
    base_revision: u64,
    ops: Vec<String>,
    rationale: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
}

impl TryFrom<ProposalRow> for Proposal {
    type Error = RoadmapError;

    fn try_from(row: ProposalRow) -> Result<Self> {
        Ok(Self {
            id: row.id,
            base_revision: row.base_revision,
            ops: row
                .ops
                .iter()
                .map(|op| serde_json::from_str(op))
                .collect::<std::result::Result<_, _>>()
                .map_err(|_| RoadmapError::InvalidData)?,
            rationale: row.rationale,
            created_at: row.created_at,
            expires_at: row.expires_at,
        })
    }
}

const PROPOSAL_FIELDS: &str =
    "record::id(id) AS id, base_revision, ops, rationale, created_at, expires_at";

/// Pending, unexpired proposals, oldest first.
pub async fn list_proposals(db: &Database, id: &str) -> Result<Vec<Proposal>> {
    if roadmap(db, id).await?.is_none() {
        return Err(RoadmapError::NotFound);
    }
    let found: Vec<ProposalRow> = rows(
        db,
        &format!(
            "SELECT {PROPOSAL_FIELDS} FROM roadmap_proposals WHERE roadmap_id = $id AND {PENDING} \
             ORDER BY created_at LIMIT {MAX_PENDING_PROPOSALS}"
        ),
        json!({"id": id}),
    )
    .await?;
    found.into_iter().map(Proposal::try_from).collect()
}

/// Record a proposal for the roadmap at `base_revision`, which must be its
/// current revision. The operations are tried on that revision now, so an
/// accept at the same revision applies them as checked here. At most
/// [`MAX_PENDING_PROPOSALS`] are pending per roadmap.
// ponytail: same check-then-insert as `create`; only Core code proposes.
pub async fn create_proposal(
    db: &Database,
    id: &str,
    base_revision: u64,
    ops: &[ProposalOp],
    rationale: Option<&str>,
) -> Result<String> {
    check(
        (1..=MAX_PROPOSAL_OPS).contains(&ops.len()),
        "a proposal has 1 to 20 operations",
    )?;
    check(
        rationale.is_none_or(|r| text(r, MAX_RATIONALE, true)),
        "rationale must be 1 to 1000 characters",
    )?;
    let encoded = ops
        .iter()
        .map(serde_json::to_string)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| RoadmapError::InvalidData)?;
    check(
        encoded.iter().all(|op| op.len() <= MAX_OP_BYTES),
        "a proposal operation is too large",
    )?;
    let mut draft = load(db, id, None).await?;
    if draft.roadmap.revision != base_revision {
        return Err(RoadmapError::Stale);
    }
    for op in ops {
        draft.apply(op)?;
    }
    draft.finish()?;
    check(!draft.is_empty(), "the proposal changes nothing")?;
    let proposal_id = uuid::Uuid::now_v7().to_string();
    let response = db
        .query(format!(
            "BEGIN TRANSACTION; \
             IF array::len((SELECT VALUE id FROM roadmap_proposals WHERE roadmap_id = $id \
               AND {PENDING})) >= {MAX_PENDING_PROPOSALS} \
               {{ THROW 'proposal limit reached'; }}; \
             CREATE roadmap_proposals SET id = $pid, roadmap_id = $id, \
               base_revision = $base_revision, ops = $ops, rationale = $rationale, \
               status = 'pending', created_at = time::now(), \
               expires_at = time::now() + {PROPOSAL_TTL_DAYS}d RETURN NONE; \
             COMMIT TRANSACTION;"
        ))
        .bind(json!({
            "id": id,
            "pid": proposal_id,
            "base_revision": base_revision,
            "ops": encoded,
            "rationale": rationale,
        }))
        .await?;
    if let Err(error) = response.check() {
        if list_proposals(db, id).await?.len() >= MAX_PENDING_PROPOSALS {
            return Err(RoadmapError::Limit("at most 10 pending proposals"));
        }
        return Err(error.into());
    }
    Ok(proposal_id)
}

async fn mark_stale(db: &Database, proposal_id: &str) -> Result<()> {
    run(
        db,
        "UPDATE roadmap_proposals SET status = 'stale', decided_at = time::now() \
         WHERE record::id(id) = $pid AND status = 'pending' RETURN NONE",
        json!({"pid": proposal_id}),
    )
    .await?;
    Ok(())
}

/// Apply a pending proposal atomically, as Jarvis's change accepted from
/// `device_id`. A proposal made against another revision is marked stale
/// and nothing changes; one that adds a link outside the GitHub allowlist
/// is refused and stays pending.
pub async fn accept_proposal(
    db: &Database,
    id: &str,
    proposal_id: &str,
    device_id: &str,
    github: &GithubHandle,
    events: &Hub,
) -> Result<Snapshot> {
    let found: Vec<ProposalRow> = rows(
        db,
        &format!(
            "SELECT {PROPOSAL_FIELDS} FROM roadmap_proposals \
             WHERE record::id(id) = $pid AND roadmap_id = $id AND {PENDING}"
        ),
        json!({"id": id, "pid": proposal_id}),
    )
    .await?;
    let proposal = Proposal::try_from(found.into_iter().next().ok_or(RoadmapError::NotFound)?)?;
    let mut draft = load(db, id, None).await?;
    if draft.roadmap.revision != proposal.base_revision {
        mark_stale(db, proposal_id).await?;
        return Err(RoadmapError::Stale);
    }
    let before = draft.items.clone();
    for op in &proposal.ops {
        draft.apply(op)?;
    }
    check(!draft.is_empty(), "the proposal changes nothing")?;
    github
        .check_links(added_links(&before, &draft.items))
        .map_err(RoadmapError::Invalid)?;
    match commit(db, &draft, Some(device_id), Some(proposal_id), events).await {
        Err(RoadmapError::Conflict) => {
            mark_stale(db, proposal_id).await?;
            Err(RoadmapError::Stale)
        }
        Err(error) => Err(error),
        Ok(()) => get(db, id).await?.ok_or(RoadmapError::NotFound),
    }
}

/// Reject a pending proposal from `device_id`.
pub async fn reject_proposal(
    db: &Database,
    id: &str,
    proposal_id: &str,
    device_id: &str,
) -> Result<()> {
    let rejected: Vec<String> = rows(
        db,
        &format!(
            "UPDATE roadmap_proposals SET status = 'rejected', decided_at = time::now(), \
             decided_by = $device WHERE record::id(id) = $pid AND roadmap_id = $id AND {PENDING} \
             RETURN VALUE record::id(id)"
        ),
        json!({"id": id, "pid": proposal_id, "device": device_id}),
    )
    .await?;
    if rejected.is_empty() {
        return Err(RoadmapError::NotFound);
    }
    Ok(())
}

/// Expire pending proposals past their time, then delete changes older than
/// [`HISTORY_RETENTION_DAYS`] and proposals decided that long ago, in bounded
/// batches. Roadmaps and items are never pruned.
pub async fn prune(db: &Database) -> Result<usize> {
    run(
        db,
        "UPDATE roadmap_proposals SET status = 'expired', decided_at = expires_at \
         WHERE status = 'pending' AND expires_at <= time::now() RETURN NONE",
        json!({}),
    )
    .await?;
    let mut total = 0;
    for (table, expired) in [
        (
            "roadmap_changes",
            format!("at < time::now() - {HISTORY_RETENTION_DAYS}d"),
        ),
        (
            "roadmap_proposals",
            format!("decided_at != NONE AND decided_at < time::now() - {HISTORY_RETENTION_DAYS}d"),
        ),
    ] {
        let query = format!(
            "LET $ids = (SELECT VALUE id FROM {table} WHERE {expired} LIMIT {PRUNE_BATCH}); \
             DELETE $ids RETURN NONE; RETURN array::len($ids);"
        );
        for _ in 0..PRUNE_MAX_BATCHES {
            let deleted: Option<usize> = db.query(query.as_str()).await?.check()?.take(2)?;
            let deleted = deleted.unwrap_or(0);
            total += deleted;
            if deleted < PRUNE_BATCH {
                break;
            }
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use std::env;

    use surrealdb::{engine::remote::ws::Ws, opt::auth::Root, Surreal};

    use super::*;
    use crate::roadmaps::{Link, LinkKind};

    async fn database() -> std::result::Result<Database, Box<dyn std::error::Error>> {
        let db = Surreal::new::<Ws>(env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
        db.signin(Root {
            username: &env::var("JARVIS_SURREAL_TEST_USER")?,
            password: &env::var("JARVIS_SURREAL_TEST_PASS")?,
        })
        .await?;
        db.use_ns(format!("roadmaps_{}", uuid::Uuid::now_v7().simple()))
            .use_db("core")
            .await?;
        jarvis_store::apply_baseline_schema(&db).await?;
        Ok(db)
    }

    fn plan(title: &str) -> NewRoadmap {
        NewRoadmap {
            title: title.into(),
            description: None,
            phases: vec!["Now".into(), "Next".into()],
        }
    }

    /// Proposal statuses in creation order.
    async fn statuses(db: &Database) -> Result<Vec<String>> {
        #[derive(Deserialize)]
        struct Row {
            status: String,
        }
        let found: Vec<Row> = rows(
            db,
            "SELECT record::id(id) AS id, status FROM roadmap_proposals ORDER BY id",
            json!({}),
        )
        .await?;
        Ok(found.into_iter().map(|row| row.status).collect())
    }

    /// A subscriber on `hub`, past its ready event.
    fn listen(hub: &Hub) -> crate::realtime::Subscription {
        let mut listener = hub
            .subscribe(uuid::Uuid::now_v7(), uuid::Uuid::now_v7())
            .expect("subscription");
        listener.receiver.try_recv().expect("ready event");
        listener
    }

    /// The `roadmap.changed` events `listener` heard since the last call.
    fn heard(listener: &mut crate::realtime::Subscription) -> Vec<(String, u64)> {
        std::iter::from_fn(|| listener.receiver.try_recv().ok())
            .map(|envelope| match &envelope.event {
                Event::RoadmapChanged { id, revision } => (id.clone(), *revision),
                other => panic!("unexpected event {other:?}"),
            })
            .collect()
    }

    fn item(title: &str) -> NewItem {
        NewItem {
            title: title.into(),
            ..NewItem::default()
        }
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn roadmaps_items_history_and_archive(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = database().await?;
        let device = uuid::Uuid::now_v7().to_string();
        let hub = Hub::default();
        let events = &hub;
        let mut listener = listen(&hub);

        let created = create(&db, &plan("Plan"), &device, events).await?;
        let id = created.roadmap.id.clone();
        assert_eq!(created.roadmap.revision, 1);
        assert!(created.items.is_empty());
        assert!(matches!(
            create(&db, &plan(" "), &device, events).await,
            Err(RoadmapError::Invalid(_))
        ));
        // Every committed write is heard once, with its revision; a refused
        // one is not.
        assert_eq!(heard(&mut listener), [(id.clone(), 1)]);

        // Items, links and dependencies round-trip; every write is one revision.
        let pr = Link {
            kind: LinkKind::Pr,
            repo: Some("HawkeyNL/PersonalJarvis".into()),
            number: Some(214),
            tag: None,
            url: None,
            state: None,
        };
        let (a, roadmap) = create_item(
            &db,
            &id,
            1,
            &NewItem {
                phase: Some("Now".into()),
                notes: Some("Line one\nline two".into()),
                links: vec![pr.clone()],
                ..item("A")
            },
            &device,
            events,
        )
        .await?;
        assert_eq!(roadmap.roadmap.revision, 2);
        assert_eq!(roadmap.items[0].links, [pr]);
        assert_eq!(
            roadmap.items[0].notes.as_deref(),
            Some("Line one\nline two")
        );
        let (b, _) = create_item(
            &db,
            &id,
            2,
            &NewItem {
                depends_on: vec![a.clone()],
                ..item("B")
            },
            &device,
            events,
        )
        .await?;
        assert_eq!(heard(&mut listener), [(id.clone(), 2), (id.clone(), 3)]);
        // A stale revision is a conflict and changes nothing.
        assert!(matches!(
            create_item(&db, &id, 2, &item("C"), &device, events).await,
            Err(RoadmapError::Conflict)
        ));
        // A cycle is refused.
        let cycle = ItemPatch {
            depends_on: Some(vec![b.clone()]),
            ..ItemPatch::default()
        };
        assert!(matches!(
            update_item(
                &db,
                &id,
                3,
                &a,
                &cycle,
                &device,
                &GithubHandle::default(),
                events
            )
            .await,
            Err(RoadmapError::Invalid(_))
        ));
        let moved = ItemPatch {
            status: Some(Status::InProgress),
            ..ItemPatch::default()
        };
        let roadmap = update_item(
            &db,
            &id,
            3,
            &a,
            &moved,
            &device,
            &GithubHandle::default(),
            events,
        )
        .await?;
        assert_eq!(roadmap.roadmap.revision, 4);
        // A patch that changes nothing writes nothing.
        let roadmap = update_item(
            &db,
            &id,
            4,
            &a,
            &moved,
            &device,
            &GithubHandle::default(),
            events,
        )
        .await?;
        assert_eq!(roadmap.roadmap.revision, 4);
        // Unknown items are not found.
        let unknown = uuid::Uuid::now_v7().to_string();
        assert!(matches!(
            update_item(
                &db,
                &id,
                4,
                &unknown,
                &moved,
                &device,
                &GithubHandle::default(),
                events
            )
            .await,
            Err(RoadmapError::NotFound)
        ));
        assert_eq!(heard(&mut listener), [(id.clone(), 4)]);
        // Two writers on one revision: exactly one wins.
        let rename = |title: &str| RoadmapPatch {
            title: Some(title.into()),
            ..RoadmapPatch::default()
        };
        let (one, two) = (rename("One"), rename("Two"));
        let (first, second) = tokio::join!(
            update(&db, &id, 4, &one, &device, events),
            update(&db, &id, 4, &two, &device, events)
        );
        assert_eq!(
            [&first, &second].iter().filter(|r| r.is_ok()).count(),
            1,
            "{first:?} {second:?}"
        );
        assert!([first, second]
            .into_iter()
            .any(|r| matches!(r, Err(RoadmapError::Conflict))));
        assert_eq!(heard(&mut listener), [(id.clone(), 5)]);
        // Deleting A leaves B without the dependency, audited on B too.
        let roadmap = delete_item(&db, &id, 5, &a, &device, events).await?;
        assert_eq!(roadmap.items.len(), 1);
        assert!(roadmap.items[0].depends_on.is_empty());
        assert_eq!(roadmap.roadmap.revision, 6);

        let changes = history(&db, &id, MAX_HISTORY).await?;
        let actions: Vec<(u64, Action)> = changes.iter().map(|c| (c.revision, c.action)).collect();
        assert_eq!(
            actions,
            [
                (6, Action::ItemDeleted),
                (6, Action::ItemUpdated),
                (5, Action::RoadmapUpdated),
                (4, Action::ItemUpdated),
                (3, Action::ItemCreated),
                (2, Action::ItemCreated),
                (1, Action::RoadmapCreated),
            ]
        );
        assert_eq!(changes[1].item_id.as_deref(), Some(b.as_str()));
        assert_eq!(changes[1].fields, ["depends_on"]);
        let status = &changes[3];
        assert_eq!(status.item_id.as_deref(), Some(a.as_str()));
        assert_eq!(status.fields, ["status"]);
        assert_eq!(
            (status.from_status, status.to_status),
            (Some(Status::Planned), Some(Status::InProgress))
        );
        assert!(changes
            .iter()
            .all(|c| c.author == "owner" && c.device_id.as_deref() == Some(device.as_str())));
        assert_eq!(history(&db, &id, 2).await?.len(), 2);
        assert!(matches!(
            history(&db, &unknown, 10).await,
            Err(RoadmapError::NotFound)
        ));

        // Archive instead of delete: readable, listed apart, frozen.
        let archived = archive(&db, &id, 6, &device, events).await?;
        assert!(archived.roadmap.archived_at.is_some());
        assert_eq!(archived.items.len(), 1);
        assert!(list(&db, false).await?.is_empty());
        assert_eq!(list(&db, true).await?.len(), 1);
        assert!(matches!(
            create_item(&db, &id, 7, &item("Late"), &device, events).await,
            Err(RoadmapError::Archived)
        ));
        assert_eq!(heard(&mut listener), [(id.clone(), 6), (id.clone(), 7)]);

        // At most 20 roadmaps that are not archived.
        for n in 0..MAX_ROADMAPS {
            create(&db, &plan(&format!("Plan {n}")), &device, events).await?;
        }
        assert!(matches!(
            create(&db, &plan("One too many"), &device, events).await,
            Err(RoadmapError::Limit(_))
        ));
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn github_writes_are_audited_and_revision_guarded(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = database().await?;
        let device = uuid::Uuid::now_v7().to_string();
        let hub = Hub::default();
        let events = &hub;
        let mut listener = listen(&hub);
        let id = create(&db, &plan("Plan"), &device, events)
            .await?
            .roadmap
            .id;
        let pr = Link {
            kind: LinkKind::Pr,
            repo: Some("HawkeyNL/PersonalJarvis".into()),
            number: Some(7),
            tag: None,
            url: None,
            state: None,
        };
        let new_item = NewItem {
            links: vec![pr],
            ..item("A")
        };
        let (a, loaded) = create_item(&db, &id, 1, &new_item, &device, events).await?;
        let mut draft = Draft::new(loaded);
        draft.github_update(&a, &[Some("open".into())])?;
        assert_eq!(heard(&mut listener), [(id.clone(), 1), (id.clone(), 2)]);
        commit_github(&db, &draft, events).await?;
        assert_eq!(heard(&mut listener), [(id.clone(), 3)]);
        let roadmap = get(&db, &id).await?.ok_or("roadmap missing")?;
        assert_eq!(roadmap.roadmap.revision, 3);
        let stored = &roadmap.items[0];
        assert_eq!(stored.status, Status::InProgress);
        assert_eq!(stored.auto_status, Some(Status::InProgress));
        assert_eq!(stored.links[0].state.as_deref(), Some("open"));
        let change = &history(&db, &id, 1).await?[0];
        assert_eq!(change.author, "github");
        assert_eq!(change.device_id, None);
        assert_eq!(change.fields, ["links", "status"]);
        // A draft built on an older revision writes nothing.
        assert!(matches!(
            commit_github(&db, &draft, events).await,
            Err(RoadmapError::Conflict)
        ));
        assert!(heard(&mut listener).is_empty());
        // An owner edit keeps the cached state and auto_status.
        let back = ItemPatch {
            status: Some(Status::Planned),
            ..ItemPatch::default()
        };
        let roadmap = update_item(
            &db,
            &id,
            3,
            &a,
            &back,
            &device,
            &GithubHandle::default(),
            events,
        )
        .await?;
        assert_eq!(roadmap.items[0].status, Status::Planned);
        assert_eq!(roadmap.items[0].auto_status, Some(Status::InProgress));
        assert_eq!(roadmap.items[0].links[0].state.as_deref(), Some("open"));
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn proposals_accept_atomically_go_stale_and_expire(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = database().await?;
        let device = uuid::Uuid::now_v7().to_string();
        let hub = Hub::default();
        let events = &hub;
        let mut listener = listen(&hub);
        let id = create(&db, &plan("Plan"), &device, events)
            .await?
            .roadmap
            .id;
        let (a, _) = create_item(&db, &id, 1, &item("A"), &device, events).await?;
        let ready = |item_id: &str| ProposalOp::UpdateItem {
            item_id: item_id.into(),
            patch: ItemPatch {
                status: Some(Status::Ready),
                ..ItemPatch::default()
            },
        };
        let ops = [ready(&a), ProposalOp::CreateItem { item: item("B") }];

        assert!(matches!(
            create_proposal(&db, &id, 1, &ops, None).await,
            Err(RoadmapError::Stale)
        ));
        let unknown = uuid::Uuid::now_v7().to_string();
        assert!(matches!(
            create_proposal(&db, &id, 2, &[ready(&unknown)], None).await,
            Err(RoadmapError::NotFound)
        ));
        assert!(matches!(
            create_proposal(&db, &id, 2, &[], None).await,
            Err(RoadmapError::Invalid(_))
        ));
        let accepted = create_proposal(&db, &id, 2, &ops, Some("PR merged.")).await?;
        let listed = list_proposals(&db, &id).await?;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].ops, ops);
        assert_eq!(listed[0].rationale.as_deref(), Some("PR merged."));

        // A proposal that adds a link outside the allowlist is refused on
        // accept: nothing is applied and it stays pending.
        let outside = [ProposalOp::CreateItem {
            item: NewItem {
                links: vec![Link {
                    kind: LinkKind::Pr,
                    repo: Some("x/y".into()),
                    number: Some(1),
                    tag: None,
                    url: None,
                    state: None,
                }],
                ..item("C")
            },
        }];
        let refused = create_proposal(&db, &id, 2, &outside, None).await?;
        let (allowlist, _) = crate::roadmaps::github::configure("a/b", None);
        assert!(matches!(
            accept_proposal(&db, &id, &refused, &device, &allowlist, events).await,
            Err(RoadmapError::Invalid(_))
        ));
        assert_eq!(get(&db, &id).await?.unwrap().roadmap.revision, 2);
        assert_eq!(heard(&mut listener), [(id.clone(), 1), (id.clone(), 2)]);
        assert_eq!(list_proposals(&db, &id).await?.len(), 2);
        reject_proposal(&db, &id, &refused, &device).await?;

        // Accepting applies every operation in one revision, as Jarvis's
        // change accepted from the device.
        let roadmap = accept_proposal(
            &db,
            &id,
            &accepted,
            &device,
            &GithubHandle::default(),
            events,
        )
        .await?;
        assert_eq!(roadmap.roadmap.revision, 3);
        assert_eq!(heard(&mut listener), [(id.clone(), 3)]);
        assert_eq!(roadmap.items.len(), 2);
        assert_eq!(roadmap.items[0].status, Status::Ready);
        let changes = history(&db, &id, 2).await?;
        assert!(changes.iter().all(|c| c.revision == 3
            && c.author == "jarvis"
            && c.proposal_id.as_deref() == Some(accepted.as_str())
            && c.device_id.as_deref() == Some(device.as_str())));
        assert!(matches!(
            accept_proposal(
                &db,
                &id,
                &accepted,
                &device,
                &GithubHandle::default(),
                events
            )
            .await,
            Err(RoadmapError::NotFound)
        ));
        assert!(list_proposals(&db, &id).await?.is_empty());

        // An owner change makes an open proposal stale; nothing is applied.
        let live = [ProposalOp::UpdateItem {
            item_id: a.clone(),
            patch: ItemPatch {
                status: Some(Status::Live),
                ..ItemPatch::default()
            },
        }];
        // Already applied: nothing to propose.
        assert!(matches!(
            create_proposal(&db, &id, 3, &[ready(&a)], None).await,
            Err(RoadmapError::Invalid(_))
        ));
        let stale = create_proposal(&db, &id, 3, &live, None).await?;
        let rejected = create_proposal(&db, &id, 3, &live, None).await?;
        let renamed = RoadmapPatch {
            title: Some("Renamed".into()),
            ..RoadmapPatch::default()
        };
        update(&db, &id, 3, &renamed, &device, events).await?;
        assert!(matches!(
            accept_proposal(&db, &id, &stale, &device, &GithubHandle::default(), events).await,
            Err(RoadmapError::Stale)
        ));
        // Only the owner's rename was heard: the accepted proposal applied
        // nothing again and the stale one nothing at all.
        assert_eq!(heard(&mut listener), [(id.clone(), 4)]);
        assert_eq!(get(&db, &id).await?.unwrap().roadmap.revision, 4);
        reject_proposal(&db, &id, &rejected, &device).await?;
        assert!(matches!(
            reject_proposal(&db, &id, &rejected, &device).await,
            Err(RoadmapError::NotFound)
        ));
        assert!(list_proposals(&db, &id).await?.is_empty());
        assert_eq!(
            statuses(&db).await?,
            ["accepted", "rejected", "stale", "rejected"]
        );

        // At most 10 pending proposals per roadmap.
        let notes = |n: usize| ProposalOp::UpdateItem {
            item_id: a.clone(),
            patch: ItemPatch {
                notes: Some(Some(format!("Note {n}"))),
                ..ItemPatch::default()
            },
        };
        for n in 0..MAX_PENDING_PROPOSALS {
            create_proposal(&db, &id, 4, &[notes(n)], None).await?;
        }
        assert!(matches!(
            create_proposal(&db, &id, 4, &[notes(99)], None).await,
            Err(RoadmapError::Limit(_))
        ));

        // Prune expires pending proposals past their time and drops history
        // and decisions older than a year; recent rows stay.
        run(
            &db,
            "UPDATE roadmap_proposals SET created_at = time::now() - 20d, \
             expires_at = time::now() - 6d WHERE status = 'pending'; \
             UPDATE roadmap_proposals SET decided_at = time::now() - 400d WHERE status = 'rejected'; \
             UPDATE roadmap_changes SET at = time::now() - 400d WHERE revision = 1;",
            json!({}),
        )
        .await?;
        assert!(list_proposals(&db, &id).await?.is_empty());
        assert_eq!(prune(&db).await?, 3);
        let mut expected = vec!["accepted", "stale"];
        expected.extend(["expired"; MAX_PENDING_PROPOSALS]);
        assert_eq!(statuses(&db).await?, expected);
        assert_eq!(history(&db, &id, MAX_HISTORY).await?.len(), 4);
        Ok(())
    }
}
