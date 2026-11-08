//! In-app roadmaps (schema 0018): the owner's plans, their items, an audit
//! of every change and the changes Jarvis proposes.
//!
//! This module holds the types and the pure rules: input validation, limits,
//! dependencies without cycles and the diff that becomes the audit. Every
//! write is first applied to a [`Draft`] of the loaded roadmap; the store
//! then commits the draft in one transaction that also checks the revision.

pub mod github;
pub mod store;

use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Deserializer, Serialize};
use time::OffsetDateTime;

/// Roadmaps that are not archived.
pub const MAX_ROADMAPS: usize = 20;
pub const MAX_ITEMS: usize = 300;
/// Pending, unexpired proposals per roadmap.
pub const MAX_PENDING_PROPOSALS: usize = 10;
pub const MAX_PROPOSAL_OPS: usize = 20;
pub const PROPOSAL_TTL_DAYS: u32 = 14;
/// Changes, and proposals after their decision, are kept this long.
pub const HISTORY_RETENTION_DAYS: u32 = 365;
pub const MAX_HISTORY: usize = 100;

pub const MAX_ROADMAP_TITLE: usize = 120;
pub const MAX_ITEM_TITLE: usize = 200;
pub const MAX_DESCRIPTION: usize = 2000;
pub const MAX_NOTES: usize = 4000;
pub const MAX_PHASES: usize = 20;
pub const MAX_PHASE: usize = 60;
pub const MAX_POSITION: u32 = 100_000;
pub const MAX_DEPENDS_ON: usize = 20;
pub const MAX_LINKS: usize = 10;
pub const MAX_URL: usize = 500;
pub const MAX_LINK_NUMBER: u64 = 1_000_000_000;
pub const MAX_RATIONALE: usize = 1000;
/// Serialized size of one proposal operation (the schema's bound).
pub const MAX_OP_BYTES: usize = 24_576;

/// Why a roadmap operation failed. Database detail is logged by the caller,
/// never returned to clients.
#[derive(Debug)]
pub enum RoadmapError {
    Database(Box<surrealdb::Error>),
    /// A stored row that does not parse.
    InvalidData,
    /// Input that breaks a rule; the message is safe to return.
    Invalid(&'static str),
    NotFound,
    Archived,
    /// The roadmap is not at the expected revision.
    Conflict,
    /// The proposal was made against an older revision.
    Stale,
    Limit(&'static str),
}

impl std::fmt::Display for RoadmapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(error) => write!(f, "roadmap database error: {error}"),
            Self::InvalidData => f.write_str("invalid stored roadmap row"),
            Self::Invalid(message) | Self::Limit(message) => f.write_str(message),
            Self::NotFound => f.write_str("not found"),
            Self::Archived => f.write_str("roadmap is archived"),
            Self::Conflict => f.write_str("roadmap revision conflict"),
            Self::Stale => f.write_str("proposal is stale"),
        }
    }
}

impl std::error::Error for RoadmapError {}

impl From<surrealdb::Error> for RoadmapError {
    fn from(error: surrealdb::Error) -> Self {
        Self::Database(Box::new(error))
    }
}

type Result<T> = std::result::Result<T, RoadmapError>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Live,
    Ready,
    InProgress,
    #[default]
    Planned,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    Pr,
    Issue,
    Release,
    Url,
}

/// A pull request or issue (`repo` and `number`), a release (`repo` and
/// `tag`) or an https `url`. Stored as given. `state` is the GitHub state the
/// poller cached; input cannot set it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Link {
    pub kind: LinkKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

impl Link {
    fn same_target(&self, other: &Link) -> bool {
        (self.kind, &self.repo, self.number, &self.tag, &self.url)
            == (
                other.kind,
                &other.repo,
                other.number,
                &other.tag,
                &other.url,
            )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Roadmap {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub phases: Vec<String>,
    pub revision: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub archived_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub title: String,
    pub status: Status,
    pub auto_status: Option<Status>,
    pub phase: Option<String>,
    pub position: u32,
    pub notes: Option<String>,
    pub depends_on: Vec<String>,
    pub links: Vec<Link>,
    /// Set by the database; a new item in a draft carries the draft's time.
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// A roadmap with its items, ordered by position.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Snapshot {
    pub roadmap: Roadmap,
    pub items: Vec<Item>,
}

/// `Some(None)` for an explicit `null`, `None` when the field is absent.
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewRoadmap {
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub phases: Vec<String>,
}

/// Absent fields stay; `description: null` clears it. Items in a removed
/// phase lose their phase.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoadmapPatch {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default, deserialize_with = "present")]
    pub description: Option<Option<String>>,
    #[serde(default)]
    pub phases: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewItem {
    pub title: String,
    #[serde(default)]
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// Defaults to after the last item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
}

/// Absent fields stay; `phase: null` and `notes: null` clear them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub phase: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<u32>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub notes: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub links: Option<Vec<Link>>,
}

/// One operation of a Jarvis proposal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProposalOp {
    CreateItem { item: NewItem },
    UpdateItem { item_id: String, patch: ItemPatch },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    RoadmapCreated,
    RoadmapUpdated,
    RoadmapArchived,
    ItemCreated,
    ItemUpdated,
    ItemDeleted,
}

/// What one change did; the store adds revision, time and author.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Change {
    pub action: Action,
    pub item_id: Option<String>,
    /// The roadmap's or item's title at the time.
    pub title: String,
    pub fields: Vec<String>,
    pub from_status: Option<Status>,
    pub to_status: Option<Status>,
}

/// Non-blank, at most `max` characters, no control or invisible characters
/// (multi-line text may hold line breaks and tabs).
fn text(value: &str, max: usize, multiline: bool) -> bool {
    !value.trim().is_empty()
        && value.chars().count() <= max
        && if multiline {
            value
                .split(['\n', '\t'])
                .all(crate::validation::no_hidden_chars)
        } else {
            crate::validation::no_hidden_chars(value)
        }
}

fn check(ok: bool, message: &'static str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(RoadmapError::Invalid(message))
    }
}

/// `owner/name` as GitHub allows it: the owner up to 39 letters, digits or
/// hyphens (not first), the name up to 100 letters, digits, `.`, `_` or `-`.
fn github_repo(value: &str) -> bool {
    let Some((owner, name)) = value.split_once('/') else {
        return false;
    };
    (1..=39).contains(&owner.len())
        && !owner.starts_with('-')
        && owner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && (1..=100).contains(&name.len())
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn release_tag(value: &str) -> bool {
    (1..=100).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'/' | b'-'))
}

/// An absolute https URL with a host and no credentials, printable ASCII.
fn https_url(value: &str) -> bool {
    value.len() <= MAX_URL
        && value.bytes().all(|b| (b'!'..=b'~').contains(&b))
        && url::Url::parse(value).is_ok_and(|url| {
            url.scheme() == "https"
                && url.host_str().is_some_and(|host| !host.is_empty())
                && url.username().is_empty()
                && url.password().is_none()
        })
        && value.starts_with("https://")
}

fn validate_link(link: &Link) -> Result<()> {
    let repo_ok = link.repo.as_deref().is_some_and(github_repo);
    let number_ok = link
        .number
        .is_some_and(|n| (1..=MAX_LINK_NUMBER).contains(&n));
    let ok = match link.kind {
        LinkKind::Pr | LinkKind::Issue => {
            repo_ok && number_ok && link.tag.is_none() && link.url.is_none()
        }
        LinkKind::Release => {
            repo_ok
                && link.tag.as_deref().is_some_and(release_tag)
                && link.number.is_none()
                && link.url.is_none()
        }
        LinkKind::Url => {
            link.url.as_deref().is_some_and(https_url)
                && link.repo.is_none()
                && link.number.is_none()
                && link.tag.is_none()
        }
    };
    check(
        ok,
        "a link is a pr or issue (repo owner/name and number), a release (repo and tag) or an https url",
    )
}

/// Validated links; a link that is already on the item keeps its cached
/// GitHub state, any other starts without one.
fn links(new: &[Link], old: &[Link]) -> Result<Vec<Link>> {
    check(new.len() <= MAX_LINKS, "at most 10 links")?;
    new.iter()
        .map(|link| {
            validate_link(link)?;
            Ok(Link {
                state: old
                    .iter()
                    .find(|o| o.same_target(link))
                    .and_then(|o| o.state.clone()),
                ..link.clone()
            })
        })
        .collect()
}

fn validate_roadmap_title(title: &str) -> Result<()> {
    check(
        text(title, MAX_ROADMAP_TITLE, false),
        "title must be 1 to 120 characters on one line",
    )
}

fn validate_description(description: Option<&str>) -> Result<()> {
    check(
        description.is_none_or(|d| text(d, MAX_DESCRIPTION, true)),
        "description must be 1 to 2000 characters",
    )
}

fn validate_phases(phases: &[String]) -> Result<()> {
    check(
        phases.len() <= MAX_PHASES
            && phases.iter().all(|p| text(p, MAX_PHASE, false))
            && phases.iter().collect::<BTreeSet<_>>().len() == phases.len(),
        "phases must be at most 20 distinct names of 1 to 60 characters",
    )
}

/// Validate a new roadmap before it is stored.
pub fn validate_new_roadmap(input: &NewRoadmap) -> Result<()> {
    validate_roadmap_title(&input.title)?;
    validate_description(input.description.as_deref())?;
    validate_phases(&input.phases)
}

/// True when following `depends_on` from some item leads back to it.
fn has_cycle(items: &[Item]) -> bool {
    fn visit(at: usize, edges: &[Vec<usize>], mark: &mut [u8]) -> bool {
        mark[at] = 1;
        for &next in &edges[at] {
            if mark[next] == 1 || (mark[next] == 0 && visit(next, edges, mark)) {
                return true;
            }
        }
        mark[at] = 2;
        false
    }
    let index: HashMap<&str, usize> = items
        .iter()
        .enumerate()
        .map(|(i, item)| (item.id.as_str(), i))
        .collect();
    let edges: Vec<Vec<usize>> = items
        .iter()
        .map(|item| {
            item.depends_on
                .iter()
                .filter_map(|id| index.get(id.as_str()).copied())
                .collect()
        })
        .collect();
    // At most MAX_ITEMS deep, so recursion is bounded.
    let mut mark = vec![0u8; items.len()];
    (0..items.len()).any(|i| mark[i] == 0 && visit(i, &edges, &mut mark))
}

/// The status a link's cached GitHub state stands for: an open or draft PR
/// is in progress, a merged PR or an issue closed as completed is ready, a PR
/// in a published release or a published release is live. Any other state
/// (closed, not found, unreleased) is a badge only.
fn link_status(link: &Link) -> Option<Status> {
    match (link.kind, link.state.as_deref()?) {
        (LinkKind::Pr, "open" | "draft") => Some(Status::InProgress),
        (LinkKind::Pr, "merged") | (LinkKind::Issue, "completed") => Some(Status::Ready),
        (LinkKind::Pr, "released") | (LinkKind::Release, "published") => Some(Status::Live),
        _ => None,
    }
}

/// Forward order of the statuses; `blocked` is outside it.
fn progress(status: Status) -> u8 {
    match status {
        Status::Planned | Status::Blocked => 0,
        Status::InProgress => 1,
        Status::Ready => 2,
        Status::Live => 3,
    }
}

/// The most advanced status the item's links stand for.
pub fn derived_status(links: &[Link]) -> Option<Status> {
    links
        .iter()
        .filter_map(link_status)
        .max_by_key(|status| progress(*status))
}

/// A roadmap with pending changes, built from a loaded [`Snapshot`].
#[derive(Debug, Clone)]
pub struct Draft {
    pub(crate) roadmap: Roadmap,
    pub(crate) items: Vec<Item>,
    created: BTreeSet<String>,
    updated: BTreeSet<String>,
    deleted: Vec<String>,
    pub(crate) archive: bool,
    pub(crate) changes: Vec<Change>,
}

/// The rows a committed draft writes.
pub(crate) struct Writes<'a> {
    pub creates: Vec<&'a Item>,
    pub updates: Vec<&'a Item>,
    pub deletes: &'a [String],
}

impl Draft {
    pub fn new(snapshot: Snapshot) -> Self {
        Self {
            roadmap: snapshot.roadmap,
            items: snapshot.items,
            created: BTreeSet::new(),
            updated: BTreeSet::new(),
            deleted: Vec::new(),
            archive: false,
            changes: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    pub(crate) fn writes(&self) -> Writes<'_> {
        let (creates, updates) = self
            .items
            .iter()
            .filter(|item| self.created.contains(&item.id) || self.updated.contains(&item.id))
            .partition(|item| self.created.contains(&item.id));
        Writes {
            creates,
            updates,
            deletes: &self.deleted,
        }
    }

    fn item_mut(&mut self, id: &str) -> Result<&mut Item> {
        self.items
            .iter_mut()
            .find(|item| item.id == id)
            .ok_or(RoadmapError::NotFound)
    }

    fn validate_depends_on(&self, id: &str, depends_on: &[String]) -> Result<()> {
        check(
            depends_on.len() <= MAX_DEPENDS_ON
                && depends_on.iter().collect::<BTreeSet<_>>().len() == depends_on.len()
                && depends_on
                    .iter()
                    .all(|dep| dep != id && self.items.iter().any(|item| &item.id == dep)),
            "depends_on must name at most 20 other items of this roadmap",
        )
    }

    fn validate_phase(&self, phase: Option<&str>) -> Result<()> {
        check(
            phase.is_none_or(|p| self.roadmap.phases.iter().any(|known| known == p)),
            "phase must be one of the roadmap's phases",
        )
    }

    pub fn update_roadmap(&mut self, patch: &RoadmapPatch) -> Result<()> {
        let mut fields = Vec::new();
        if let Some(title) = &patch.title {
            validate_roadmap_title(title)?;
            if *title != self.roadmap.title {
                self.roadmap.title.clone_from(title);
                fields.push("title".to_string());
            }
        }
        if let Some(description) = &patch.description {
            validate_description(description.as_deref())?;
            if *description != self.roadmap.description {
                self.roadmap.description.clone_from(description);
                fields.push("description".to_string());
            }
        }
        if let Some(phases) = &patch.phases {
            validate_phases(phases)?;
            if *phases != self.roadmap.phases {
                self.roadmap.phases.clone_from(phases);
                fields.push("phases".to_string());
                for item in &mut self.items {
                    if item
                        .phase
                        .as_ref()
                        .is_some_and(|phase| !phases.contains(phase))
                    {
                        item.phase = None;
                        if !self.created.contains(&item.id) {
                            self.updated.insert(item.id.clone());
                        }
                        self.changes.push(Change {
                            action: Action::ItemUpdated,
                            item_id: Some(item.id.clone()),
                            title: item.title.clone(),
                            fields: vec!["phase".to_string()],
                            from_status: None,
                            to_status: None,
                        });
                    }
                }
            }
        }
        if !fields.is_empty() {
            self.changes.push(Change {
                action: Action::RoadmapUpdated,
                item_id: None,
                title: self.roadmap.title.clone(),
                fields,
                from_status: None,
                to_status: None,
            });
        }
        Ok(())
    }

    pub fn archive(&mut self) {
        self.archive = true;
        self.changes.push(Change {
            action: Action::RoadmapArchived,
            item_id: None,
            title: self.roadmap.title.clone(),
            fields: Vec::new(),
            from_status: None,
            to_status: None,
        });
    }

    /// Add an item and return its id.
    pub fn create_item(&mut self, input: &NewItem) -> Result<String> {
        if self.items.len() >= MAX_ITEMS {
            return Err(RoadmapError::Limit("a roadmap has at most 300 items"));
        }
        check(
            text(&input.title, MAX_ITEM_TITLE, false),
            "title must be 1 to 200 characters on one line",
        )?;
        self.validate_phase(input.phase.as_deref())?;
        check(
            input.position.is_none_or(|p| p <= MAX_POSITION),
            "position must be 0 to 100000",
        )?;
        check(
            input
                .notes
                .as_deref()
                .is_none_or(|n| text(n, MAX_NOTES, true)),
            "notes must be 1 to 4000 characters",
        )?;
        let id = uuid::Uuid::now_v7().to_string();
        self.validate_depends_on(&id, &input.depends_on)?;
        let links = links(&input.links, &[])?;
        let position = input.position.unwrap_or_else(|| {
            self.items
                .iter()
                .map(|item| item.position.saturating_add(1))
                .max()
                .unwrap_or(0)
                .min(MAX_POSITION)
        });
        self.items.push(Item {
            id: id.clone(),
            title: input.title.clone(),
            status: input.status,
            auto_status: None,
            phase: input.phase.clone(),
            position,
            notes: input.notes.clone(),
            depends_on: input.depends_on.clone(),
            links,
            updated_at: OffsetDateTime::now_utc(),
        });
        self.created.insert(id.clone());
        self.changes.push(Change {
            action: Action::ItemCreated,
            item_id: Some(id.clone()),
            title: input.title.clone(),
            fields: Vec::new(),
            from_status: None,
            to_status: Some(input.status),
        });
        Ok(id)
    }

    pub fn update_item(&mut self, id: &str, patch: &ItemPatch) -> Result<()> {
        // Validate everything against the current draft first, then apply.
        let old = self.item_mut(id)?.clone();
        if let Some(title) = &patch.title {
            check(
                text(title, MAX_ITEM_TITLE, false),
                "title must be 1 to 200 characters on one line",
            )?;
        }
        if let Some(phase) = &patch.phase {
            self.validate_phase(phase.as_deref())?;
        }
        check(
            patch.position.is_none_or(|p| p <= MAX_POSITION),
            "position must be 0 to 100000",
        )?;
        if let Some(Some(notes)) = &patch.notes {
            check(
                text(notes, MAX_NOTES, true),
                "notes must be 1 to 4000 characters",
            )?;
        }
        if let Some(depends_on) = &patch.depends_on {
            self.validate_depends_on(id, depends_on)?;
        }
        let new_links = patch
            .links
            .as_ref()
            .map(|new| links(new, &old.links))
            .transpose()?;

        let mut new = old.clone();
        if let Some(title) = &patch.title {
            new.title.clone_from(title);
        }
        if let Some(status) = patch.status {
            new.status = status;
        }
        if let Some(phase) = &patch.phase {
            new.phase.clone_from(phase);
        }
        if let Some(position) = patch.position {
            new.position = position;
        }
        if let Some(notes) = &patch.notes {
            new.notes.clone_from(notes);
        }
        if let Some(depends_on) = &patch.depends_on {
            new.depends_on.clone_from(depends_on);
        }
        if let Some(links) = new_links {
            new.links = links;
        }
        let fields: Vec<String> = [
            ("title", old.title != new.title),
            ("status", old.status != new.status),
            ("phase", old.phase != new.phase),
            ("position", old.position != new.position),
            ("notes", old.notes != new.notes),
            ("depends_on", old.depends_on != new.depends_on),
            ("links", old.links != new.links),
        ]
        .into_iter()
        .filter(|(_, changed)| *changed)
        .map(|(field, _)| field.to_string())
        .collect();
        if fields.is_empty() {
            return Ok(());
        }
        let status_changed = old.status != new.status;
        self.changes.push(Change {
            action: Action::ItemUpdated,
            item_id: Some(id.to_string()),
            title: new.title.clone(),
            fields,
            from_status: status_changed.then_some(old.status),
            to_status: status_changed.then_some(new.status),
        });
        *self.item_mut(id)? = new;
        if !self.created.contains(id) {
            self.updated.insert(id.to_string());
        }
        Ok(())
    }

    /// Delete an item; items that depended on it no longer do.
    pub fn delete_item(&mut self, id: &str) -> Result<()> {
        let index = self
            .items
            .iter()
            .position(|item| item.id == id)
            .ok_or(RoadmapError::NotFound)?;
        let removed = self.items.remove(index);
        for item in &mut self.items {
            if item.depends_on.iter().any(|dep| dep == id) {
                item.depends_on.retain(|dep| dep != id);
                if !self.created.contains(&item.id) {
                    self.updated.insert(item.id.clone());
                }
            }
        }
        self.updated.remove(id);
        if !self.created.remove(id) {
            self.deleted.push(removed.id);
        }
        self.changes.push(Change {
            action: Action::ItemDeleted,
            item_id: Some(id.to_string()),
            title: removed.title,
            fields: Vec::new(),
            from_status: Some(removed.status),
            to_status: None,
        });
        Ok(())
    }

    /// Cache the GitHub states the poller saw on an item's links (`states[i]`
    /// for `links[i]`; `None` keeps the cached one). When the new states stand
    /// for a more advanced status than the cached ones did, the item moves
    /// forward to it, never back and never out of `blocked`. An owner's status
    /// edit therefore holds until GitHub shows something new.
    pub fn github_update(&mut self, id: &str, states: &[Option<String>]) -> Result<()> {
        let old = self.item_mut(id)?.clone();
        let mut new = old.clone();
        for (link, state) in new.links.iter_mut().zip(states) {
            if state.is_some() {
                link.state.clone_from(state);
            }
        }
        if new.links == old.links {
            return Ok(());
        }
        let before = derived_status(&old.links);
        let after = derived_status(&new.links);
        new.auto_status = after;
        let mut fields = vec!["links".to_string()];
        if let Some(after) = after {
            let new_fact = before.is_none_or(|before| progress(after) > progress(before));
            if new_fact && old.status != Status::Blocked && progress(after) > progress(old.status) {
                new.status = after;
                fields.push("status".to_string());
            }
        }
        let status_changed = old.status != new.status;
        self.changes.push(Change {
            action: Action::ItemUpdated,
            item_id: Some(id.to_string()),
            title: new.title.clone(),
            fields,
            from_status: status_changed.then_some(old.status),
            to_status: status_changed.then_some(new.status),
        });
        *self.item_mut(id)? = new;
        if !self.created.contains(id) {
            self.updated.insert(id.to_string());
        }
        Ok(())
    }

    pub fn apply(&mut self, op: &ProposalOp) -> Result<()> {
        match op {
            ProposalOp::CreateItem { item } => self.create_item(item).map(drop),
            ProposalOp::UpdateItem { item_id, patch } => self.update_item(item_id, patch),
        }
    }

    /// Rules over the whole roadmap, checked once before a commit.
    pub fn finish(&self) -> Result<()> {
        check(!has_cycle(&self.items), "depends_on must not form a cycle")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(items: usize) -> Snapshot {
        let now = OffsetDateTime::now_utc();
        Snapshot {
            roadmap: Roadmap {
                id: uuid::Uuid::now_v7().to_string(),
                title: "Plan".into(),
                description: None,
                phases: vec!["Now".into(), "Next".into()],
                revision: 1,
                created_at: now,
                updated_at: now,
                archived_at: None,
            },
            items: (0..items)
                .map(|n| Item {
                    id: uuid::Uuid::now_v7().to_string(),
                    title: format!("Item {n}"),
                    status: Status::Planned,
                    auto_status: None,
                    phase: Some("Now".into()),
                    position: n as u32,
                    notes: None,
                    depends_on: Vec::new(),
                    links: Vec::new(),
                    updated_at: now,
                })
                .collect(),
        }
    }

    fn new_item(title: &str) -> NewItem {
        NewItem {
            title: title.into(),
            ..NewItem::default()
        }
    }

    fn link(kind: LinkKind) -> Link {
        Link {
            kind,
            repo: None,
            number: None,
            tag: None,
            url: None,
            state: None,
        }
    }

    #[test]
    fn text_and_link_rules() {
        assert!(text("Ship it", 10, false));
        assert!(text("Gus\u{2019}s plan", 20, false));
        for bad in ["", "  ", "a\nb", "a\u{202E}b", "elevenchars"] {
            assert!(!text(bad, 10, false), "{bad:?}");
        }
        assert!(text("one\n\ttwo", 20, true));
        assert!(!text("one\r\ntwo", 20, true));
        assert!(text(&"é".repeat(10), 10, false));

        for ok in [
            "HawkeyNL/PersonalJarvis",
            "a/b",
            "a-1/x.y_z",
            &format!("{}/r", "o".repeat(39)),
        ] {
            assert!(github_repo(ok), "{ok}");
        }
        for bad in [
            "owner",
            "/r",
            "o/",
            "-o/r",
            "o/r/x",
            "o/..",
            "o/.",
            "o_x/r",
            "o/r ",
            &format!("{}/r", "o".repeat(40)),
        ] {
            assert!(!github_repo(bad), "{bad}");
        }
        assert!(https_url("https://example.com/a?b=c#d"));
        for bad in [
            "http://example.com",
            "https://",
            "https://user:pw@example.com",
            "https://exa mple.com",
            "javascript:alert(1)",
            "HTTPS://example.com",
            &format!("https://e.com/{}", "a".repeat(MAX_URL)),
        ] {
            assert!(!https_url(bad), "{bad}");
        }

        let pr = Link {
            repo: Some("a/b".into()),
            number: Some(214),
            ..link(LinkKind::Pr)
        };
        let release = Link {
            repo: Some("a/b".into()),
            tag: Some("v0.0.78".into()),
            ..link(LinkKind::Release)
        };
        let url = Link {
            url: Some("https://example.com".into()),
            ..link(LinkKind::Url)
        };
        for ok in [&pr, &release, &url] {
            assert!(validate_link(ok).is_ok(), "{ok:?}");
        }
        for bad in [
            Link {
                number: None,
                ..pr.clone()
            },
            Link {
                number: Some(0),
                ..pr.clone()
            },
            Link {
                tag: Some("v1".into()),
                ..pr.clone()
            },
            Link {
                kind: LinkKind::Issue,
                repo: Some("nope".into()),
                ..pr.clone()
            },
            Link {
                tag: Some("v 1".into()),
                ..release.clone()
            },
            Link {
                number: Some(1),
                ..release.clone()
            },
            Link {
                url: Some("http://example.com".into()),
                ..url.clone()
            },
            Link {
                repo: Some("a/b".into()),
                ..url.clone()
            },
        ] {
            assert!(validate_link(&bad).is_err(), "{bad:?}");
        }
        // Input cannot set the cached state; an unchanged link keeps it.
        let cached = Link {
            state: Some("merged".into()),
            ..pr.clone()
        };
        let forged = Link {
            state: Some("open".into()),
            ..url.clone()
        };
        let kept = links(&[pr.clone(), forged], &[cached]).unwrap();
        assert_eq!(kept[0].state.as_deref(), Some("merged"));
        assert_eq!(kept[1].state, None);
        assert!(links(&vec![url; MAX_LINKS + 1], &[]).is_err());
        // Unknown fields are refused, not dropped.
        assert!(
            serde_json::from_str::<Link>(r#"{"kind":"url","url":"https://a.b","x":1}"#).is_err()
        );
    }

    #[test]
    fn roadmap_input_is_bounded() {
        let ok = NewRoadmap {
            title: "Plan".into(),
            description: Some("Line\nline".into()),
            phases: vec!["Now".into(), "Next".into()],
        };
        assert!(validate_new_roadmap(&ok).is_ok());
        for bad in [
            NewRoadmap {
                title: " ".into(),
                ..ok.clone()
            },
            NewRoadmap {
                title: "t".repeat(121),
                ..ok.clone()
            },
            NewRoadmap {
                description: Some(String::new()),
                ..ok.clone()
            },
            NewRoadmap {
                phases: vec!["Now".into(), "Now".into()],
                ..ok.clone()
            },
            NewRoadmap {
                phases: (0..21).map(|n| n.to_string()).collect(),
                ..ok.clone()
            },
            NewRoadmap {
                phases: vec!["p".repeat(61)],
                ..ok.clone()
            },
        ] {
            assert!(validate_new_roadmap(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn items_are_limited_and_positioned() {
        let mut draft = Draft::new(snapshot(MAX_ITEMS - 1));
        let id = draft.create_item(&new_item("Last")).unwrap();
        let last = draft.items.iter().find(|item| item.id == id).unwrap();
        assert_eq!(last.position, MAX_ITEMS as u32 - 1);
        assert!(matches!(
            draft.create_item(&new_item("One too many")),
            Err(RoadmapError::Limit(_))
        ));
        let mut draft = Draft::new(snapshot(0));
        for bad in [
            NewItem {
                phase: Some("Later".into()),
                ..new_item("x")
            },
            NewItem {
                position: Some(MAX_POSITION + 1),
                ..new_item("x")
            },
            NewItem {
                notes: Some("n".repeat(MAX_NOTES + 1)),
                ..new_item("x")
            },
            NewItem {
                depends_on: vec!["missing".into()],
                ..new_item("x")
            },
            new_item(&"t".repeat(MAX_ITEM_TITLE + 1)),
        ] {
            assert!(
                matches!(draft.create_item(&bad), Err(RoadmapError::Invalid(_))),
                "{bad:?}"
            );
        }
        assert!(draft.is_empty());
    }

    #[test]
    fn dependencies_stay_in_the_roadmap_and_never_cycle() {
        let snap = snapshot(3);
        let ids: Vec<String> = snap.items.iter().map(|item| item.id.clone()).collect();
        let mut draft = Draft::new(snap);
        let depends = |deps: &[&String]| ItemPatch {
            depends_on: Some(deps.iter().map(|d| (*d).clone()).collect()),
            ..ItemPatch::default()
        };
        draft.update_item(&ids[1], &depends(&[&ids[0]])).unwrap();
        draft.update_item(&ids[2], &depends(&[&ids[1]])).unwrap();
        draft.finish().unwrap();
        // Self, duplicates and unknown items are refused at once.
        let unknown = uuid::Uuid::now_v7().to_string();
        for bad in [
            depends(&[&ids[1], &ids[1]]),
            depends(&[&ids[0]]),
            depends(&[&unknown]),
        ] {
            assert!(
                matches!(
                    draft.clone().update_item(&ids[0], &bad),
                    Err(RoadmapError::Invalid(_))
                ),
                "{bad:?}"
            );
        }
        // 0 -> 2 -> 1 -> 0 is a cycle.
        draft.update_item(&ids[0], &depends(&[&ids[2]])).unwrap();
        assert!(matches!(draft.finish(), Err(RoadmapError::Invalid(_))));
        // Deleting an item drops it from the items that depended on it.
        let mut draft = Draft::new(snapshot(2));
        let (a, b) = (draft.items[0].id.clone(), draft.items[1].id.clone());
        draft.update_item(&b, &depends(&[&a])).unwrap();
        draft.delete_item(&a).unwrap();
        assert!(draft.items[0].depends_on.is_empty());
        let writes = draft.writes();
        assert_eq!(writes.updates.len(), 1);
        assert_eq!(writes.deletes, [a]);
        assert!(writes.creates.is_empty());
    }

    #[test]
    fn has_cycle_finds_long_and_short_cycles() {
        let mut items = snapshot(4).items;
        assert!(!has_cycle(&items));
        let ids: Vec<String> = items.iter().map(|item| item.id.clone()).collect();
        items[1].depends_on = vec![ids[0].clone()];
        items[2].depends_on = vec![ids[0].clone(), ids[1].clone()];
        assert!(!has_cycle(&items));
        items[0].depends_on = vec![ids[3].clone()];
        items[3].depends_on = vec![ids[2].clone()];
        assert!(has_cycle(&items));
        items[3].depends_on.clear();
        items[3].depends_on.push(ids[3].clone());
        assert!(has_cycle(&items));
    }

    #[test]
    fn changes_record_only_what_changed() {
        let mut draft = Draft::new(snapshot(1));
        let id = draft.items[0].id.clone();
        let same = ItemPatch {
            title: Some("Item 0".into()),
            status: Some(Status::Planned),
            ..ItemPatch::default()
        };
        draft.update_item(&id, &same).unwrap();
        draft
            .update_roadmap(&RoadmapPatch {
                title: Some("Plan".into()),
                ..RoadmapPatch::default()
            })
            .unwrap();
        assert!(draft.is_empty());
        let patch: ItemPatch =
            serde_json::from_str(r#"{"status":"in_progress","phase":null,"notes":"Why"}"#).unwrap();
        draft.update_item(&id, &patch).unwrap();
        let change = &draft.changes[0];
        assert_eq!(change.fields, ["status", "phase", "notes"]);
        assert_eq!(change.from_status, Some(Status::Planned));
        assert_eq!(change.to_status, Some(Status::InProgress));
        assert_eq!(draft.items[0].phase, None);
        // Removing a phase clears it on the items that used it.
        let mut draft = Draft::new(snapshot(1));
        draft
            .update_roadmap(&RoadmapPatch {
                phases: Some(vec!["Next".into()]),
                ..RoadmapPatch::default()
            })
            .unwrap();
        assert_eq!(draft.items[0].phase, None);
        assert_eq!(draft.writes().updates.len(), 1);
        let fields: Vec<_> = draft
            .changes
            .iter()
            .map(|c| (c.action, c.fields.clone()))
            .collect();
        assert_eq!(
            fields,
            [
                (Action::ItemUpdated, vec!["phase".to_string()]),
                (Action::RoadmapUpdated, vec!["phases".to_string()])
            ]
        );
        // A created item that is updated or deleted in the same draft is
        // written once, or not at all.
        let mut draft = Draft::new(snapshot(0));
        let a = draft.create_item(&new_item("A")).unwrap();
        let b = draft.create_item(&new_item("B")).unwrap();
        draft
            .update_item(
                &a,
                &ItemPatch {
                    notes: Some(Some("n".into())),
                    ..ItemPatch::default()
                },
            )
            .unwrap();
        draft.delete_item(&b).unwrap();
        let writes = draft.writes();
        assert_eq!(
            (
                writes.creates.len(),
                writes.updates.len(),
                writes.deletes.len()
            ),
            (1, 0, 0)
        );
    }

    #[test]
    fn github_states_move_items_forward_only() {
        let pr = Link {
            repo: Some("a/b".into()),
            number: Some(1),
            ..link(LinkKind::Pr)
        };
        let issue = Link {
            kind: LinkKind::Issue,
            ..pr.clone()
        };
        let release = Link {
            repo: Some("a/b".into()),
            tag: Some("v1".into()),
            ..link(LinkKind::Release)
        };
        let with = |link: &Link, state: &str| Link {
            state: Some(state.into()),
            ..link.clone()
        };
        // The mapping, and the most advanced link wins.
        for (links, expected) in [
            (vec![with(&pr, "open")], Some(Status::InProgress)),
            (vec![with(&pr, "draft")], Some(Status::InProgress)),
            (vec![with(&pr, "merged")], Some(Status::Ready)),
            (vec![with(&issue, "completed")], Some(Status::Ready)),
            (vec![with(&pr, "released")], Some(Status::Live)),
            (vec![with(&release, "published")], Some(Status::Live)),
            (vec![with(&pr, "closed"), with(&issue, "open")], None),
            (
                vec![with(&pr, "not_found"), with(&release, "unreleased")],
                None,
            ),
            (
                vec![with(&pr, "open"), with(&issue, "completed")],
                Some(Status::Ready),
            ),
        ] {
            assert_eq!(derived_status(&links), expected, "{links:?}");
        }

        let state = |s: &str| vec![Some(s.to_string())];
        let item_with = |status: Status, links: Vec<Link>| {
            let mut snap = snapshot(1);
            snap.items[0].status = status;
            snap.items[0].links = links;
            let id = snap.items[0].id.clone();
            (Draft::new(snap), id)
        };

        // PR opened: planned -> in_progress, link state and auto_status cached.
        let (mut draft, id) = item_with(Status::Planned, vec![pr.clone()]);
        draft.github_update(&id, &state("open")).unwrap();
        let item = &draft.items[0];
        assert_eq!(item.status, Status::InProgress);
        assert_eq!(item.auto_status, Some(Status::InProgress));
        assert_eq!(item.links[0].state.as_deref(), Some("open"));
        assert_eq!(draft.changes[0].fields, ["links", "status"]);
        assert_eq!(
            (draft.changes[0].from_status, draft.changes[0].to_status),
            (Some(Status::Planned), Some(Status::InProgress))
        );
        assert_eq!(draft.writes().updates.len(), 1);

        // The same state again is no change at all.
        let (mut draft, id) = item_with(Status::Planned, vec![with(&pr, "open")]);
        draft.github_update(&id, &state("open")).unwrap();
        draft.github_update(&id, &[None]).unwrap();
        assert!(draft.is_empty());

        // Owner wins: the owner moved the item back while the PR stayed open;
        // a closed PR is a badge only and moves nothing.
        draft.github_update(&id, &state("closed")).unwrap();
        assert_eq!(draft.items[0].status, Status::Planned);
        assert_eq!(draft.items[0].auto_status, None);
        assert_eq!(draft.changes[0].fields, ["links"]);

        // A new fact (merged) moves an owner-set status forward, never back.
        let (mut draft, id) = item_with(Status::Planned, vec![with(&pr, "open")]);
        draft.github_update(&id, &state("merged")).unwrap();
        assert_eq!(draft.items[0].status, Status::Ready);
        let (mut draft, id) = item_with(Status::Live, vec![with(&pr, "open")]);
        draft.github_update(&id, &state("merged")).unwrap();
        assert_eq!(draft.items[0].status, Status::Live);
        assert_eq!(draft.items[0].auto_status, Some(Status::Ready));
        // A step back on GitHub (release gone) is not a new fact.
        let (mut draft, id) = item_with(Status::Planned, vec![with(&release, "published")]);
        draft.github_update(&id, &state("unreleased")).unwrap();
        assert_eq!(draft.items[0].status, Status::Planned);
        // Blocked is never touched.
        let (mut draft, id) = item_with(Status::Blocked, vec![pr.clone()]);
        draft.github_update(&id, &state("released")).unwrap();
        assert_eq!(draft.items[0].status, Status::Blocked);
        assert_eq!(draft.items[0].auto_status, Some(Status::Live));
        assert!(draft.changes[0].to_status.is_none());
    }

    #[test]
    fn proposal_ops_round_trip_and_refuse_unknown_fields() {
        let ops = [
            ProposalOp::CreateItem {
                item: new_item("New"),
            },
            ProposalOp::UpdateItem {
                item_id: "x".into(),
                patch: ItemPatch {
                    notes: Some(None),
                    status: Some(Status::Ready),
                    ..ItemPatch::default()
                },
            },
        ];
        for op in &ops {
            let json = serde_json::to_string(op).unwrap();
            assert_eq!(
                &serde_json::from_str::<ProposalOp>(&json).unwrap(),
                op,
                "{json}"
            );
        }
        assert_eq!(
            serde_json::to_string(&ops[1]).unwrap(),
            r#"{"op":"update_item","item_id":"x","patch":{"status":"ready","notes":null}}"#
        );
        for bad in [
            r#"{"op":"delete_item","item_id":"x"}"#,
            r#"{"op":"update_item","item_id":"x","patch":{"auto_status":"live"}}"#,
            r#"{"op":"create_item","item":{"title":"t"},"extra":1}"#,
        ] {
            assert!(serde_json::from_str::<ProposalOp>(bad).is_err(), "{bad}");
        }
    }
}
