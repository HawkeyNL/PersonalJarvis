//! Jarvis's memory (schema 0025): short lasting facts about the owner.
//!
//! Every write goes through [`check`], which refuses markup, links and
//! anything that looks like a credential before the database sees it. Every
//! query is scoped to one owner by `user_id` and binds all values; only
//! constants and clamped numbers are formatted into queries. Errors carry
//! fixed messages, never the fact text or a database detail.
//!
//! Facts with a sensitivity of [`STRICT_SENSITIVITY`] or more must never
//! leave the Home Node: callers that build a prompt for a cloud model or any
//! outbound call read with [`CLOUD_MAX_SENSITIVITY`].

use serde::{Deserialize, Serialize};
use serde_json::json;
use time::OffsetDateTime;

use jarvis_store::Database;

/// Most facts one owner may keep. When full, a save is refused: evicting
/// would silently lose facts and let injected text push them out.
pub const MAX_FACTS: usize = 1000;
/// Longest fact, in characters.
pub const MAX_TEXT_CHARS: usize = 300;
/// Longest normalised key (lower-casing can make text longer).
const MAX_KEY_CHARS: usize = 400;
/// Facts at or above this score stay on the Home Node.
pub const STRICT_SENSITIVITY: u8 = 9;
/// The highest score a cloud model or outbound call may ever see.
pub const CLOUD_MAX_SENSITIVITY: u8 = STRICT_SENSITIVITY - 1;
/// The score of a fact saved without a valid one (fail closed).
pub const DEFAULT_SENSITIVITY: u8 = 10;
const _: () = assert!(DEFAULT_SENSITIVITY >= STRICT_SENSITIVITY);

const FIELDS: &str = "record::id(id) AS id, text, sensitivity, created_by, \
     source_conversation_id, revision, created_at, updated_at";

/// Every message is fixed: none carries the input or a database detail.
#[derive(Debug, PartialEq, Eq)]
pub enum MemoryError {
    Invalid(&'static str),
    Credential,
    Duplicate,
    Full,
    NotFound,
    Conflict,
    Database,
    InvalidData,
}

impl std::fmt::Display for MemoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Invalid(message) => message,
            Self::Credential => "memory never keeps passwords, keys, tokens or seed phrases",
            Self::Duplicate => "this fact is saved already",
            Self::Full => "memory is full",
            Self::NotFound => "memory fact not found",
            Self::Conflict => "memory fact changed meanwhile",
            Self::Database => "memory database error",
            Self::InvalidData => "invalid memory row",
        })
    }
}

impl std::error::Error for MemoryError {}

impl From<surrealdb::Error> for MemoryError {
    fn from(_: surrealdb::Error) -> Self {
        Self::Database
    }
}

type Result<T> = std::result::Result<T, MemoryError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CreatedBy {
    Jarvis,
    Owner,
}

/// A stored fact.
#[derive(Debug, Clone, Deserialize)]
pub struct Fact {
    pub id: String,
    pub text: String,
    pub sensitivity: u8,
    pub created_by: CreatedBy,
    /// The chat it came from; kept after that chat is deleted.
    #[serde(default)]
    pub source_conversation_id: Option<String>,
    pub revision: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// The stored score for `raw`: 1 to 10 as given, anything else (missing,
/// out of range) [`DEFAULT_SENSITIVITY`].
pub fn sensitivity(raw: Option<i64>) -> u8 {
    match raw {
        Some(score @ 1..=10) => score as u8,
        _ => DEFAULT_SENSITIVITY,
    }
}

/// The fact as it is stored: trimmed, one visible line of at most
/// [`MAX_TEXT_CHARS`], no markup or links, nothing like a credential.
pub fn check(text: &str) -> Result<String> {
    let text = text.trim();
    let chars = text.chars().count();
    if chars == 0 || chars > MAX_TEXT_CHARS || !crate::validation::no_hidden_chars(text) {
        return Err(MemoryError::Invalid(
            "a fact is one line of 1 to 300 visible characters",
        ));
    }
    let lower = text.to_lowercase();
    if ["[[", "]]", "<", "http", "www."]
        .iter()
        .any(|bad| lower.contains(bad))
    {
        return Err(MemoryError::Invalid("a fact holds no markup or links"));
    }
    if looks_like_credential(text, &lower) {
        return Err(MemoryError::Credential);
    }
    Ok(text.to_owned())
}

/// What de-duplication compares: lower case, whitespace collapsed, a
/// trailing `.` or `!` dropped.
fn key(text: &str) -> Result<String> {
    let key = text
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let key = key.trim_end_matches(['.', '!']).trim_end();
    let chars = key.chars().count();
    if chars == 0 || chars > MAX_KEY_CHARS {
        return Err(MemoryError::Invalid(
            "a fact needs visible words and fewer special letters",
        ));
    }
    Ok(key.to_owned())
}

const CREDENTIAL_WORDS: [&str; 18] = [
    "password",
    "passwords",
    "passcode",
    "passphrase",
    "wachtwoord",
    "wachtwoorden",
    "inlogcode",
    "toegangscode",
    "herstelzin",
    "seedzin",
    "privésleutel",
    "seed",
    "pincode",
    "token",
    "tokens",
    "apikey",
    "mnemonic",
    "2fa",
];
const CREDENTIAL_PAIRS: [(&str, &str); 8] = [
    ("api", "key"),
    ("private", "key"),
    ("secret", "key"),
    ("geheime", "sleutel"),
    ("recovery", "phrase"),
    // "pin" alone is also Dutch for paying by card.
    ("pin", "is"),
    ("pin", "code"),
    ("pin", "number"),
];

fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '+' | '/' | '=' | '.' | '~')
}

/// Refuses on sight rather than proving a secret: a word about one, a long
/// token-like run, a long digit run (card, account, phone numbers) or a
/// bare seed phrase.
// ponytail: heuristics, not a secret scanner; extend the lists when a real
// credential gets through.
fn looks_like_credential(text: &str, lower: &str) -> bool {
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    if words.iter().any(|word| CREDENTIAL_WORDS.contains(word))
        || words
            .windows(2)
            .any(|pair| CREDENTIAL_PAIRS.contains(&(pair[0], pair[1])))
    {
        return true;
    }
    // A long run of token characters with a digit, or a longer one with an
    // inner capital: keys, tokens, hashes. A long lower-case Dutch compound
    // word or a double-barrelled name stays allowed.
    let token = text.split(|c: char| !token_char(c)).any(|run| {
        (run.len() >= 20 && run.bytes().any(|b| b.is_ascii_digit()))
            || (run.len() >= 32 && run.bytes().skip(1).any(|b| b.is_ascii_uppercase()))
    });
    let digits = text
        .split(|c: char| !c.is_ascii_digit())
        .any(|run| run.len() >= 9);
    // A seed phrase on its own: 12 to 24 short lower-case words, in steps
    // of three.
    let phrase: Vec<&str> = text.split_whitespace().collect();
    let seed = (12..=24).contains(&phrase.len())
        && phrase.len() % 3 == 0
        && phrase.iter().all(|word| {
            (3..=8).contains(&word.len()) && word.bytes().all(|b| b.is_ascii_lowercase())
        });
    token || digits || seed
}

/// Serialises creates, so the de-duplication and cap checks and the write
/// cannot interleave. The transaction repeats the checks behind the guard
/// row (0025) for a caller whose timeout released the lock while its write
/// was still in flight: of two such creates only one commits.
// ponytail: an in-process lock; Core is the only writer of memories.
static CREATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn get(db: &Database, user: &str, id: &str) -> Result<Option<Fact>> {
    let found: Vec<Fact> = db
        .query(format!(
            "SELECT {FIELDS} FROM memories WHERE user_id = $user AND record::id(id) = $id LIMIT 1"
        ))
        .bind(json!({ "user": user, "id": id }))
        .await?
        .check()?
        .take(0)?;
    Ok(found.into_iter().next())
}

async fn by_key(db: &Database, user: &str, key: &str) -> Result<Option<Fact>> {
    let found: Vec<Fact> = db
        .query(format!(
            "SELECT {FIELDS} FROM memories WHERE user_id = $user AND key = $key LIMIT 1"
        ))
        .bind(json!({ "user": user, "key": key }))
        .await?
        .check()?
        .take(0)?;
    Ok(found.into_iter().next())
}

/// The owner's facts with a score of at most `max_sensitivity`, most
/// recently changed first, at most `limit` (1 to [`MAX_FACTS`]).
pub async fn list(
    db: &Database,
    user: &str,
    max_sensitivity: u8,
    limit: usize,
) -> Result<Vec<Fact>> {
    let limit = limit.clamp(1, MAX_FACTS);
    Ok(db
        .query(format!(
            "SELECT {FIELDS} FROM memories WHERE user_id = $user AND sensitivity <= $max \
             ORDER BY updated_at DESC, id DESC LIMIT {limit}"
        ))
        .bind(json!({ "user": user, "max": max_sensitivity }))
        .await?
        .check()?
        .take(0)?)
}

/// Save a fact for `user`. The same fact again (by its normalised text)
/// changes no text: it bumps the stored one and keeps the higher score.
/// [`MemoryError::Full`] at [`MAX_FACTS`].
pub async fn create(
    db: &Database,
    user: &str,
    text: &str,
    score: Option<i64>,
    created_by: CreatedBy,
    source_conversation_id: Option<uuid::Uuid>,
) -> Result<Fact> {
    let text = check(text)?;
    let key = key(&text)?;
    let _creating = CREATE.lock().await;
    let bindings = json!({
        "id": uuid::Uuid::now_v7().to_string(),
        "user": user,
        "text": text,
        "key": key,
        "sensitivity": sensitivity(score),
        "by": created_by,
        "source": source_conversation_id.map(|id| id.to_string()),
        "max": MAX_FACTS,
    });
    let response = db
        .query(
            "BEGIN TRANSACTION; \
             UPSERT memory_guards:create SET touched_at = time::now() RETURN NONE; \
             LET $same = (SELECT VALUE id FROM memories WHERE user_id = $user AND key = $key); \
             IF array::len($same) = 0 AND array::len((SELECT VALUE id FROM memories \
               WHERE user_id = $user)) >= $max { THROW 'no room'; }; \
             IF array::len($same) > 0 { \
               UPDATE memories SET sensitivity = math::max([sensitivity, $sensitivity]), \
                 revision += 1, updated_at = time::now() \
                 WHERE user_id = $user AND key = $key RETURN NONE; \
             } ELSE { \
               CREATE memories SET id = $id, user_id = $user, text = $text, key = $key, \
                 sensitivity = $sensitivity, created_by = $by, \
                 source_conversation_id = $source, revision = 1, \
                 created_at = time::now(), updated_at = time::now() RETURN NONE; \
             }; \
             COMMIT TRANSACTION;",
        )
        .bind(bindings)
        .await?;
    if response.check().is_err() {
        // Explain the refusal from the state after it.
        let count: Vec<String> = db
            .query("SELECT VALUE record::id(id) FROM memories WHERE user_id = $user")
            .bind(json!({ "user": user }))
            .await?
            .check()?
            .take(0)?;
        return Err(if count.len() >= MAX_FACTS {
            MemoryError::Full
        } else {
            MemoryError::Database
        });
    }
    by_key(db, user, &key)
        .await?
        .ok_or(MemoryError::InvalidData)
}

/// Replace the text and score of the owner's fact `id` at revision
/// `expected`, and bump it. [`MemoryError::Duplicate`] when another fact
/// already says the same. The score is stored as given (missing or invalid
/// is 10), so the caller decides who may lower one: per the owner's rule
/// Jarvis may only raise a score unless the owner asks in that turn.
pub async fn update(
    db: &Database,
    user: &str,
    id: &str,
    expected: u64,
    text: &str,
    score: Option<i64>,
) -> Result<Fact> {
    let text = check(text)?;
    let key = key(&text)?;
    // The same lock as create, so its de-duplication sees this key.
    let _creating = CREATE.lock().await;
    let response = db
        .query(
            "BEGIN TRANSACTION; \
             LET $done = (UPDATE memories SET text = $text, key = $key, \
               sensitivity = $sensitivity, revision += 1, updated_at = time::now() \
               WHERE user_id = $user AND record::id(id) = $id AND revision = $expected \
               RETURN AFTER); \
             IF array::len($done) != 1 { THROW 'fact unavailable'; }; \
             COMMIT TRANSACTION;",
        )
        .bind(json!({
            "user": user,
            "id": id,
            "expected": expected,
            "text": text,
            "key": key,
            "sensitivity": sensitivity(score),
        }))
        .await?;
    if response.check().is_err() {
        return Err(match get(db, user, id).await? {
            None => MemoryError::NotFound,
            Some(fact) if fact.revision != expected => MemoryError::Conflict,
            Some(_)
                if by_key(db, user, &key)
                    .await?
                    .is_some_and(|other| other.id != id) =>
            {
                MemoryError::Duplicate
            }
            Some(_) => MemoryError::Database,
        });
    }
    get(db, user, id).await?.ok_or(MemoryError::InvalidData)
}

/// Remove the owner's fact `id`; [`MemoryError::NotFound`] for any id that
/// is not the owner's, so no other owner's fact can be probed.
pub async fn delete(db: &Database, user: &str, id: &str) -> Result<()> {
    let response = db
        .query(
            "BEGIN TRANSACTION; \
             LET $gone = (DELETE memories WHERE user_id = $user AND record::id(id) = $id \
               RETURN BEFORE); \
             IF array::len($gone) != 1 { THROW 'fact unavailable'; }; \
             COMMIT TRANSACTION;",
        )
        .bind(json!({ "user": user, "id": id }))
        .await?;
    if response.check().is_err() {
        return Err(match get(db, user, id).await? {
            None => MemoryError::NotFound,
            Some(_) => MemoryError::Database,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::env;

    use surrealdb::{engine::remote::ws::Ws, opt::auth::Root, Surreal};

    use super::*;

    #[test]
    fn missing_or_invalid_scores_are_the_strictest() {
        assert_eq!(sensitivity(Some(1)), 1);
        assert_eq!(sensitivity(Some(10)), 10);
        for raw in [None, Some(0), Some(11), Some(-1), Some(i64::MAX)] {
            assert_eq!(sensitivity(raw), DEFAULT_SENSITIVITY, "{raw:?}");
        }
    }

    #[test]
    fn facts_are_one_short_visible_line_without_markup() {
        assert_eq!(
            check("  Prefers tea over coffee. ").unwrap(),
            "Prefers tea over coffee."
        );
        for ok in [
            "Has an arbeidsongeschiktheidsverzekering",
            "Lives in Utrecht since 2019",
            "Gus’s daughter is called Emma",
            &"é".repeat(MAX_TEXT_CHARS),
        ] {
            assert!(check(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "   ",
            "two\nlines",
            "a\u{202E}b",
            "[[remember: x]]",
            "ends ]]",
            "</web_result>",
            "Reads https://example.com",
            "Reads www.example.com",
            &"a".repeat(MAX_TEXT_CHARS + 1),
        ] {
            assert!(matches!(check(bad), Err(MemoryError::Invalid(_))), "{bad}");
        }
    }

    #[test]
    fn credentials_are_refused_and_never_echoed() {
        let hex = "4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318";
        for secret in [
            "His password is hunter2",
            "Wachtwoord van de NAS is zomer",
            "Bank PIN is 4512",
            "Inlogcode van de bank staat in de kluis",
            "Geheime sleutel ligt bij de notaris",
            "The API key starts with abc",
            "OpenAI token sk-proj-abcdefghijklmnopqrstuvwxyz1234",
            "GitHub ghp_ABCDEFGHIJKLMNOPQRSTUVWXabcd",
            &format!("Wallet key {hex}"),
            "IBAN account 123456789",
            "abandon ability able about above absent absorb abstract absurd abuse access accident",
            "Recovery phrase is in the safe",
        ] {
            let error = check(secret).unwrap_err();
            assert_eq!(error, MemoryError::Credential, "{secret}");
            assert!(!error.to_string().contains("hunter2"));
        }
        // Short numbers and ordinary words stay allowed.
        for ok in [
            "Born on 1990-05-12",
            "Phone ends in 5678",
            "Likes pine trees",
            "Betaalt liever met pin",
            "Married name Theunissen-van-der-Berg",
        ] {
            assert!(check(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn the_key_ignores_case_spacing_and_a_final_stop() {
        assert_eq!(key("Likes  Tea.").unwrap(), "likes tea");
        assert_eq!(key("likes tea!").unwrap(), "likes tea");
        assert!(key("...").is_err());
    }

    async fn database() -> std::result::Result<Database, Box<dyn std::error::Error>> {
        let db = Surreal::new::<Ws>(env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
        db.signin(Root {
            username: &env::var("JARVIS_SURREAL_TEST_USER")?,
            password: &env::var("JARVIS_SURREAL_TEST_PASS")?,
        })
        .await?;
        db.use_ns(format!("memory_{}", uuid::Uuid::now_v7().simple()))
            .use_db("core")
            .await?;
        jarvis_store::apply_baseline_schema(&db).await?;
        Ok(db)
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn facts_are_per_owner_deduplicated_updated_and_deleted(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = database().await?;
        let chat = uuid::Uuid::now_v7();
        let tea = create(
            &db,
            "gus",
            "Likes tea.",
            None,
            CreatedBy::Jarvis,
            Some(chat),
        )
        .await?;
        assert_eq!(tea.sensitivity, DEFAULT_SENSITIVITY);
        assert_eq!((tea.revision, tea.created_by), (1, CreatedBy::Jarvis));
        assert_eq!(tea.source_conversation_id, Some(chat.to_string()));
        // The same fact again bumps the stored one and never lowers its score.
        let again = create(&db, "gus", "likes  TEA", Some(2), CreatedBy::Jarvis, None).await?;
        assert_eq!(
            (again.id.as_str(), again.text.as_str()),
            (tea.id.as_str(), "Likes tea.")
        );
        assert_eq!(
            (again.revision, again.sensitivity),
            (2, DEFAULT_SENSITIVITY)
        );
        let city = create(
            &db,
            "gus",
            "Lives in Utrecht",
            Some(3),
            CreatedBy::Owner,
            None,
        )
        .await?;
        assert_eq!(city.source_conversation_id, None);
        // A credential never reaches the table.
        assert_eq!(
            create(
                &db,
                "gus",
                "Password is hunter2",
                Some(10),
                CreatedBy::Jarvis,
                None
            )
            .await
            .unwrap_err(),
            MemoryError::Credential
        );
        // The strict facts drop out of a cloud read.
        let ids = |facts: Vec<Fact>| facts.into_iter().map(|f| f.id).collect::<Vec<_>>();
        assert_eq!(
            ids(list(&db, "gus", 10, 50).await?),
            [city.id.as_str(), tea.id.as_str()]
        );
        assert_eq!(
            ids(list(&db, "gus", CLOUD_MAX_SENSITIVITY, 50).await?),
            [city.id.as_str()]
        );
        assert_eq!(ids(list(&db, "gus", 10, 1).await?), [city.id.as_str()]);
        // Another owner sees, changes and removes none of them.
        assert!(list(&db, "eve", 10, 50).await?.is_empty());
        assert_eq!(
            update(&db, "eve", &tea.id, 2, "Likes coffee", Some(1))
                .await
                .unwrap_err(),
            MemoryError::NotFound
        );
        assert_eq!(
            delete(&db, "eve", &tea.id).await.unwrap_err(),
            MemoryError::NotFound
        );
        let theirs = create(&db, "eve", "Likes tea", Some(1), CreatedBy::Jarvis, None).await?;
        assert_ne!(theirs.id, tea.id);
        // An update names the revision it builds on.
        let coffee = update(&db, "gus", &tea.id, 2, "Likes coffee", Some(4)).await?;
        assert_eq!(
            (coffee.text.as_str(), coffee.sensitivity, coffee.revision),
            ("Likes coffee", 4, 3)
        );
        assert!(coffee.updated_at >= tea.updated_at);
        assert_eq!(coffee.created_at, tea.created_at);
        assert_eq!(
            update(&db, "gus", &tea.id, 2, "Likes water", None)
                .await
                .unwrap_err(),
            MemoryError::Conflict
        );
        assert_eq!(
            update(&db, "gus", &tea.id, 3, "lives in utrecht.", None)
                .await
                .unwrap_err(),
            MemoryError::Duplicate
        );
        assert_eq!(
            update(
                &db,
                "gus",
                &tea.id,
                3,
                "Key is sk-abcdefghijklmnopqrstuvwxyz1234",
                None
            )
            .await
            .unwrap_err(),
            MemoryError::Credential
        );
        let water = update(&db, "gus", &tea.id, 3, "Likes water", Some(0)).await?;
        assert_eq!(water.sensitivity, DEFAULT_SENSITIVITY);
        // Delete is owner-only and once.
        delete(&db, "gus", &tea.id).await?;
        assert_eq!(
            delete(&db, "gus", &tea.id).await.unwrap_err(),
            MemoryError::NotFound
        );
        assert_eq!(
            update(&db, "gus", &tea.id, 4, "Likes tea", None)
                .await
                .unwrap_err(),
            MemoryError::NotFound
        );
        assert_eq!(ids(list(&db, "gus", 10, 50).await?), [city.id]);
        assert_eq!(ids(list(&db, "eve", 10, 50).await?), [theirs.id]);
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn an_owner_keeps_at_most_a_thousand_facts(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = database().await?;
        let rows: Vec<_> = (1..MAX_FACTS)
            .map(|n| json!({ "id": uuid::Uuid::now_v7().to_string(), "text": format!("Fact {n}") }))
            .collect();
        db.query(
            "FOR $r IN $rows { CREATE memories SET id = $r.id, user_id = 'gus', text = $r.text, \
               key = string::lowercase($r.text), sensitivity = 5, created_by = 'jarvis', \
               revision = 1, created_at = time::now(), updated_at = time::now() RETURN NONE; };",
        )
        .bind(json!({ "rows": rows }))
        .await?
        .check()?;
        // Two at once for the last place: exactly one fits.
        let (a, b) = tokio::join!(
            create(&db, "gus", "Likes tea", Some(1), CreatedBy::Jarvis, None),
            create(&db, "gus", "Likes coffee", Some(1), CreatedBy::Jarvis, None),
        );
        assert_eq!(a.is_ok() as usize + b.is_ok() as usize, 1);
        assert_eq!(a.err().or(b.err()), Some(MemoryError::Full));
        assert_eq!(list(&db, "gus", 10, usize::MAX).await?.len(), MAX_FACTS);
        assert_eq!(
            create(&db, "gus", "Likes water", Some(1), CreatedBy::Owner, None)
                .await
                .unwrap_err(),
            MemoryError::Full
        );
        // A fact saved already still bumps, and other owners have room.
        assert_eq!(
            create(&db, "gus", "fact 1", Some(1), CreatedBy::Jarvis, None)
                .await?
                .revision,
            2
        );
        create(&db, "eve", "Likes water", Some(1), CreatedBy::Jarvis, None).await?;
        // A delete makes room again.
        let oldest = list(&db, "gus", 10, MAX_FACTS)
            .await?
            .pop()
            .ok_or("no fact")?;
        delete(&db, "gus", &oldest.id).await?;
        create(&db, "gus", "Likes water", Some(1), CreatedBy::Jarvis, None).await?;
        Ok(())
    }
}
