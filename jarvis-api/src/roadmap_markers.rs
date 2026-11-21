//! Roadmaps from a chat answer (see `docs/ROADMAPS.md`, "From chat"). An
//! ordinary runs-API answer may end with one line `[[roadmap {json}]]`: a new
//! roadmap (`title`) or items for an existing one (`roadmap`, its title).
//! Core checks it like the route the owner's Add calls and offers it as a
//! card; nothing is written here, the database is only read. The JSON is
//! model output that web text in the conversation may have steered, so it is
//! parsed strictly and may carry no web links.

use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use jarvis_client_core::realtime::RoadmapProposal;

use crate::roadmaps::github::GithubHandle;
use crate::roadmaps::{
    new_draft, store, Draft, Link, LinkKind, NewItem, NewRoadmap, Roadmap, RoadmapError, Snapshot,
    Status, MAX_BULK_ITEMS, MAX_ROADMAPS,
};
use crate::routes::roadmaps::DB_TIMEOUT;
use crate::AppState;

/// Longest marker line; a longer one is refused.
pub(crate) const MAX_ROADMAP_LINE: usize = 16 * 1024;

/// A roadmap as the model writes it. Unknown fields are refused.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    title: Option<String>,
    roadmap: Option<String>,
    description: Option<String>,
    phases: Option<Vec<String>>,
    #[serde(default)]
    items: Vec<MarkerItem>,
}

/// An item: no `position` or `depends_on`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MarkerItem {
    title: String,
    #[serde(default)]
    status: Status,
    phase: Option<String>,
    notes: Option<String>,
    #[serde(default)]
    links: Vec<Link>,
}

/// A marker checked as far as it can be without the database.
#[derive(Debug, PartialEq)]
enum Parsed {
    /// The exact `POST /v1/roadmaps` body.
    New { title: String, body: Value },
    /// Items for the live roadmap with the title `name`, in any case.
    Items { name: String, items: Vec<NewItem> },
}

/// A marker checked as far as it can be without the roadmap's items.
#[derive(Debug, PartialEq)]
enum Checked {
    /// The exact `POST /v1/roadmaps` body.
    New { title: String, body: Value },
    /// Items for the one live roadmap with that title.
    Items {
        roadmap: Roadmap,
        items: Vec<NewItem>,
    },
}

/// A web address in model text: refused, so no card carries one.
fn web_link(text: &str) -> bool {
    let text = text.to_lowercase();
    text.contains("://") || text.contains("www.")
}

/// The marker's JSON checked on its own and against the GitHub allowlist.
/// `Err` names the rule it broke, for the log.
fn parse(json: &str, github: &GithubHandle) -> Result<Parsed, &'static str> {
    if json.len() > MAX_ROADMAP_LINE {
        return Err("too long");
    }
    if !crate::validation::no_hidden_chars(json) {
        return Err("hidden characters");
    }
    let marker: Marker = serde_json::from_str(json).map_err(|error| {
        if error.is_data() {
            "not a roadmap"
        } else {
            "not json"
        }
    })?;
    let items: Vec<NewItem> = marker
        .items
        .into_iter()
        .map(|item| NewItem {
            title: item.title,
            status: item.status,
            phase: item.phase,
            notes: item.notes,
            links: item.links,
            ..NewItem::default()
        })
        .collect();
    let links: Vec<&Link> = items.iter().flat_map(|item| &item.links).collect();
    let texts = [&marker.title, &marker.roadmap, &marker.description]
        .into_iter()
        .flatten()
        .chain(marker.phases.iter().flatten())
        .chain(items.iter().flat_map(|item| {
            [Some(&item.title), item.phase.as_ref(), item.notes.as_ref()]
                .into_iter()
                .flatten()
        }))
        .chain(
            links
                .iter()
                .flat_map(|link| [&link.repo, &link.tag])
                .flatten(),
        );
    if texts.into_iter().any(|text| web_link(text)) {
        return Err("web link");
    }
    if links
        .iter()
        .any(|link| link.kind == LinkKind::Url || link.url.is_some() || link.state.is_some())
    {
        return Err("link kind");
    }
    if !links.is_empty() && (github.repos().is_empty() || github.check_links(links).is_err()) {
        return Err("link repo");
    }
    if items.len() > MAX_BULK_ITEMS {
        return Err("too many items");
    }
    match (marker.title, marker.roadmap) {
        (Some(title), None) => {
            let mut body = json!({
                "title": title,
                "description": marker.description,
                "phases": marker.phases.unwrap_or_default(),
                "items": items,
            });
            if let Some(fields) = body.as_object_mut() {
                fields.retain(|_, value| !value.is_null());
            }
            let input: NewRoadmap = serde_json::from_value(body.clone()).map_err(|_| "body")?;
            new_draft(&input).map_err(|_| "invalid roadmap")?;
            Ok(Parsed::New { title, body })
        }
        (None, Some(name)) => {
            if marker.description.is_some() || marker.phases.is_some() {
                return Err("roadmap fields");
            }
            if items.is_empty() {
                return Err("no items");
            }
            Ok(Parsed::Items { name, items })
        }
        _ => Err("title or roadmap"),
    }
}

/// A parsed marker checked against the live (not archived) roadmaps: a new
/// one within the limit and with a title of its own, items for exactly one.
fn check(parsed: Parsed, live: &[Roadmap]) -> Result<Checked, &'static str> {
    let named = |name: &str| {
        let name = name.to_lowercase();
        live.iter()
            .filter(move |roadmap| roadmap.title.to_lowercase() == name)
    };
    match parsed {
        Parsed::New { title, body } => {
            if live.len() >= MAX_ROADMAPS {
                Err("roadmap limit")
            } else if named(&title).next().is_some() {
                Err("title taken")
            } else {
                Ok(Checked::New { title, body })
            }
        }
        Parsed::Items { name, items } => {
            let mut found = named(&name);
            match (found.next(), found.next()) {
                (Some(roadmap), None) => Ok(Checked::Items {
                    roadmap: roadmap.clone(),
                    items,
                }),
                (None, _) => Err("no such roadmap"),
                (Some(_), Some(_)) => Err("ambiguous roadmap"),
            }
        }
    }
}

/// The items tried on the roadmap as it is now, like the batch route does.
fn check_items(snapshot: Snapshot, items: &[NewItem]) -> Result<(), &'static str> {
    if snapshot.roadmap.archived_at.is_some() {
        return Err("archived");
    }
    let mut draft = Draft::new(snapshot);
    for item in items {
        draft.create_item(item).map_err(|_| "invalid items")?;
    }
    draft.finish().map_err(|_| "invalid items")
}

/// Why an answer's roadmap marker gets no card. `split` when the line was
/// cut off, too long, not JSON or held too many items: a smaller card may
/// fit.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Refused {
    pub split: bool,
}

fn refused(reason: &'static str) -> Refused {
    tracing::info!(reason, "roadmap marker dropped");
    Refused {
        split: matches!(
            reason,
            "malformed" | "too long" | "not json" | "too many items"
        ),
    }
}

async fn read<T>(
    future: impl std::future::Future<Output = Result<T, RoadmapError>>,
) -> Result<T, Refused> {
    match tokio::time::timeout(DB_TIMEOUT, future).await {
        Ok(Ok(value)) => Ok(value),
        _ => {
            tracing::warn!("roadmap marker dropped: roadmaps unavailable");
            Err(Refused { split: false })
        }
    }
}

/// The card for the first `[[roadmap …]]` line of an answer (`lines` holds
/// the JSON of the well-formed ones; empty when every one was malformed).
/// Reads the roadmaps only once the line passed every check that needs no
/// database, and writes nothing. Anything refused is logged without its
/// text.
pub(crate) async fn proposal(
    state: &AppState,
    lines: &[String],
) -> Result<RoadmapProposal, Refused> {
    let json = lines.first().ok_or_else(|| refused("malformed"))?;
    if lines.len() > 1 {
        tracing::info!(dropped = lines.len() - 1, "extra roadmap markers dropped");
    }
    let parsed = parse(json, &state.github).map_err(refused)?;
    let live = read(store::list(&state.db, false)).await?;
    let card = match check(parsed, &live).map_err(refused)? {
        Checked::New { title, body } => RoadmapProposal {
            id: Uuid::now_v7(),
            roadmap_id: None,
            title,
            body,
        },
        Checked::Items { roadmap, items } => {
            let snapshot = read(store::get(&state.db, &roadmap.id))
                .await?
                .ok_or_else(|| refused("no such roadmap"))?;
            check_items(snapshot, &items).map_err(refused)?;
            RoadmapProposal {
                id: Uuid::now_v7(),
                roadmap_id: Some(roadmap.id),
                title: roadmap.title,
                body: json!({ "items": items }),
            }
        }
    };
    tracing::info!(
        id = %card.id,
        items = card.body["items"].as_array().map_or(0, Vec::len),
        "roadmap card proposed"
    );
    Ok(card)
}

#[cfg(test)]
mod tests {
    use time::OffsetDateTime;

    use super::*;
    use crate::roadmaps::{github, Item};

    fn roadmap(title: &str) -> Roadmap {
        let now = OffsetDateTime::now_utc();
        Roadmap {
            id: Uuid::now_v7().to_string(),
            title: title.into(),
            description: None,
            phases: vec!["Done".into(), "Now".into()],
            revision: 3,
            created_at: now,
            updated_at: now,
            archived_at: None,
        }
    }

    fn allowlist() -> std::sync::Arc<GithubHandle> {
        github::configure("HawkeyNL/PersonalJarvis", None).0
    }

    fn run(json: &str, github: &GithubHandle, live: &[Roadmap]) -> Result<Checked, &'static str> {
        parse(json, github).and_then(|parsed| check(parsed, live))
    }

    fn checked(json: &str) -> Result<Checked, &'static str> {
        run(json, &allowlist(), &[roadmap("Jarvis"), roadmap("Old")])
    }

    #[test]
    fn a_new_roadmap_becomes_the_exact_create_body() {
        let json = r#"{"title":"Core","description":"What ships when.","phases":["Done","Now"],"items":[{"title":"Roadmaps","status":"live","phase":"Done","notes":"Line one\nline two","links":[{"kind":"pr","repo":"HawkeyNL/PersonalJarvis","number":294}]},{"title":"Chat cards","phase":"Now"}]}"#;
        assert_eq!(
            checked(json),
            Ok(Checked::New {
                title: "Core".into(),
                body: json!({"title": "Core", "description": "What ships when.",
                    "phases": ["Done", "Now"], "items": [
                        {"title": "Roadmaps", "status": "live", "phase": "Done",
                            "notes": "Line one\nline two", "links": [{"kind": "pr",
                            "repo": "HawkeyNL/PersonalJarvis", "number": 294}]},
                        {"title": "Chat cards", "status": "planned", "phase": "Now"}]}),
            })
        );
        // The smallest marker, and the body the route reads.
        let Ok(Checked::New { body, .. }) = checked(r#"{"title":"Empty"}"#) else {
            panic!("refused");
        };
        assert_eq!(body, json!({"title": "Empty", "phases": [], "items": []}));
        assert!(serde_json::from_value::<NewRoadmap>(body).is_ok()); // The runtime facts' example.
        assert!(checked(
            r#"{"title":"Trip","phases":["Now"],"items":[{"title":"Book","phase":"Now"}]}"#
        )
        .is_ok());
    }

    #[test]
    fn items_name_one_live_roadmap_by_title() {
        let json = r#"{"roadmap":"jARVIS","items":[{"title":"Next","phase":"Now"}]}"#;
        let Ok(Checked::Items {
            roadmap: found,
            items,
        }) = checked(json)
        else {
            panic!("refused");
        };
        assert_eq!(found.title, "Jarvis");
        assert_eq!(
            json!({ "items": items }),
            json!({"items": [{"title": "Next", "status": "planned", "phase": "Now"}]})
        );
        // Tried on the roadmap as it is now: its phases and the item limit.
        let snapshot = |items: usize| Snapshot {
            roadmap: found.clone(),
            items: (0..items)
                .map(|n| Item {
                    id: Uuid::now_v7().to_string(),
                    title: format!("Item {n}"),
                    status: Status::Planned,
                    auto_status: None,
                    phase: None,
                    position: n as u32,
                    notes: None,
                    depends_on: Vec::new(),
                    links: Vec::new(),
                    updated_at: OffsetDateTime::now_utc(),
                })
                .collect(),
        };
        assert_eq!(check_items(snapshot(0), &items), Ok(()));
        assert_eq!(
            check_items(snapshot(crate::roadmaps::MAX_ITEMS), &items),
            Err("invalid items")
        );
        let later = vec![NewItem {
            phase: Some("Later".into()),
            ..items[0].clone()
        }];
        assert_eq!(check_items(snapshot(0), &later), Err("invalid items"));
        // An escaped bidi character is refused on the roadmap's rules too.
        let Ok(Checked::Items { items: hidden, .. }) =
            checked(r#"{"roadmap":"Jarvis","items":[{"title":"a\u202eb"}]}"#)
        else {
            panic!("refused early");
        };
        assert_eq!(check_items(snapshot(0), &hidden), Err("invalid items"));
        let mut archived = snapshot(0);
        archived.roadmap.archived_at = Some(OffsetDateTime::now_utc());
        assert_eq!(check_items(archived, &items), Err("archived"));
        // No match, or two roadmaps with that title, is refused.
        let two = [roadmap("Jarvis"), roadmap("JARVIS")];
        assert_eq!(
            run(json, &allowlist(), &two).map(|_| ()),
            Err("ambiguous roadmap")
        );
        assert_eq!(
            checked(r#"{"roadmap":"Jarvis 2","items":[{"title":"x"}]}"#).map(|_| ()),
            Err("no such roadmap")
        );
    }

    #[test]
    fn markers_are_refused_like_route_bodies_and_stricter() {
        let many = format!(
            r#"{{"title":"Big","items":[{}]}}"#,
            vec![r#"{"title":"x"}"#; MAX_BULK_ITEMS + 1].join(",")
        );
        let long = format!(
            r#"{{"title":"Long","description":"{}"}}"#,
            "a".repeat(MAX_ROADMAP_LINE)
        );
        let pr = |repo: &str| {
            format!(
                r#"{{"title":"T","items":[{{"title":"x","links":[{{"kind":"pr","repo":"{repo}","number":1}}]}}]}}"#
            )
        };
        for (json, reason) in [
            ("{\"title\":", "not json"),
            ("not json", "not json"),
            (long.as_str(), "too long"),
            ("{\"title\":\"a\u{202e}b\"}", "hidden characters"),
            // Escaped in the JSON, the text rules still refuse it.
            (r#"{"title":"a\u202eb"}"#, "invalid roadmap"),
            (
                r#"{"title":"T","items":[{"title":"x","notes":"a\u202Eb"}]}"#,
                "invalid roadmap",
            ),
            (r#"{"title":"T","extra":1}"#, "not a roadmap"),
            (
                r#"{"title":"T","items":[{"title":"x","position":1}]}"#,
                "not a roadmap",
            ),
            (
                r#"{"title":"T","items":[{"title":"x","depends_on":[]}]}"#,
                "not a roadmap",
            ),
            (
                r#"{"title":"T","items":[{"title":"x","status":"done"}]}"#,
                "not a roadmap",
            ),
            (r#"{"items":[]}"#, "title or roadmap"),
            (
                r#"{"title":"T","roadmap":"Jarvis","items":[{"title":"x"}]}"#,
                "title or roadmap",
            ),
            (
                r#"{"roadmap":"Jarvis","phases":["Now"],"items":[{"title":"x"}]}"#,
                "roadmap fields",
            ),
            (
                r#"{"roadmap":"Jarvis","description":"d","items":[{"title":"x"}]}"#,
                "roadmap fields",
            ),
            (r#"{"roadmap":"Jarvis","items":[]}"#, "no items"),
            (many.as_str(), "too many items"),
            (r#"{"title":"See HTTPS://x.y"}"#, "web link"),
            (r#"{"title":"T","phases":["WWW.x.y"]}"#, "web link"),
            (
                r#"{"title":"T","items":[{"title":"x","notes":"ftp:\/\/x"}]}"#,
                "web link",
            ),
            (
                r#"{"title":"T","items":[{"title":"x","links":[{"kind":"url","url":"https:\/\/x.y"}]}]}"#,
                "link kind",
            ),
            (
                r#"{"title":"T","items":[{"title":"x","links":[{"kind":"release","repo":"HawkeyNL/PersonalJarvis","tag":"www.x"}]}]}"#,
                "web link",
            ),
            (
                r#"{"title":"T","items":[{"title":"x","links":[{"kind":"pr","repo":"HawkeyNL/PersonalJarvis","number":1,"state":"merged"}]}]}"#,
                "link kind",
            ),
            (pr("x/y").as_str(), "link repo"),
            (r#"{"title":" "}"#, "invalid roadmap"),
            (r#"{"title":"T","phases":["A","A"]}"#, "invalid roadmap"),
            (
                r#"{"title":"T","items":[{"title":"x","phase":"Later"}]}"#,
                "invalid roadmap",
            ),
            (
                r#"{"title":"T","items":[{"title":"x","links":[{"kind":"pr","repo":"HawkeyNL/PersonalJarvis"}]}]}"#,
                "invalid roadmap",
            ),
        ] {
            assert_eq!(checked(json).map(|_| ()), Err(reason), "{json}");
        }
        assert!(checked(&pr("hawkeynl/personaljarvis")).is_ok());
        // Without an allowlist no link is offered at all.
        assert_eq!(
            run(
                &pr("HawkeyNL/PersonalJarvis"),
                &GithubHandle::default(),
                &[]
            )
            .map(|_| ()),
            Err("link repo")
        );
        // The 20-roadmap limit holds for a new one only.
        let full: Vec<Roadmap> = (0..MAX_ROADMAPS).map(|n| roadmap(&n.to_string())).collect();
        assert_eq!(
            run(r#"{"title":"T"}"#, &allowlist(), &full).map(|_| ()),
            Err("roadmap limit")
        );
        assert!(run(
            r#"{"roadmap":"7","items":[{"title":"x"}]}"#,
            &allowlist(),
            &full
        )
        .is_ok());
        // A new roadmap needs a title no live roadmap has.
        assert_eq!(
            checked(r#"{"title":"OLD"}"#).map(|_| ()),
            Err("title taken")
        );
        // The cheap checks need no live roadmaps.
        assert_eq!(parse("not json", &allowlist()), Err("not json"));
        // Cut off, not JSON or too big: the reply asks for a smaller card.
        for (reason, split) in [
            ("malformed", true),
            ("too long", true),
            ("not json", true),
            ("too many items", true),
            ("not a roadmap", false),
            ("web link", false),
        ] {
            assert_eq!(refused(reason), Refused { split }, "{reason}");
        }
    }
}
