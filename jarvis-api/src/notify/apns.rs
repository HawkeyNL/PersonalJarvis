//! Content-free approval pushes straight to the Jarvis iOS app through APNs
//! with token-based (.p8) authentication (docs/NOTIFICATIONS.md).
//!
//! A push says only "Approval waiting"; the app fetches the request over the
//! authenticated API and approval still needs the device signature. Tokens
//! and device ids never reach the log.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jsonwebtoken::{Algorithm, EncodingKey, Header};
use reqwest::header::{HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::json;

use super::{Audience, NotifyEvent, APP_PUSH_BUDGET, MAX_ATTEMPTS};

pub const TEAM_ID_ENV: &str = "JARVIS_APNS_TEAM_ID";
pub const KEY_ID_ENV: &str = "JARVIS_APNS_KEY_ID";
pub const TOPIC_ENV: &str = "JARVIS_APNS_TOPIC";
pub const ENVIRONMENT_ENV: &str = "JARVIS_APNS_ENVIRONMENT";
/// Base64 body of the AuthKey_<key id>.p8 file, from a root-managed secrets
/// `EnvironmentFile`.
pub const KEY_ENV: &str = "JARVIS_APNS_KEY_P8";

/// Pushes per event: the owner has a handful of devices.
const MAX_TARGETS: usize = 8;
/// Apple accepts a provider token for 60 minutes and refuses refreshing more
/// often than every 20 (`TooManyProviderTokenUpdates`).
const JWT_LIFETIME_SECS: u64 = 45 * 60;
const MIN_REFRESH_SECS: u64 = 20 * 60;
/// An approval request expires after two minutes; a later push is useless.
const EXPIRATION_SECS: u64 = 120;
const MAX_KEY_BYTES: usize = 1024;

pub struct Config {
    team_id: String,
    key_id: String,
    topic: String,
    environment: &'static str,
    key: EncodingKey,
}

impl Config {
    /// `Ok(None)` when APNs is not configured at all. `Err` is a stable,
    /// non-sensitive reason; the caller leaves app push off.
    pub fn from_env() -> Result<Option<Self>, &'static str> {
        Self::from_vars(|name| std::env::var(name).ok())
    }

    fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Option<Self>, &'static str> {
        let var = |name| var(name).filter(|value| !value.trim().is_empty());
        let names = [TEAM_ID_ENV, KEY_ID_ENV, TOPIC_ENV, ENVIRONMENT_ENV, KEY_ENV];
        if names.iter().all(|name| var(name).is_none()) {
            return Ok(None);
        }
        let team_id = apple_id(var(TEAM_ID_ENV)).ok_or("APNs team id is invalid")?;
        let key_id = apple_id(var(KEY_ID_ENV)).ok_or("APNs key id is invalid")?;
        let topic = var(TOPIC_ENV)
            .map(|value| value.trim().to_string())
            .filter(|value| valid_topic(value))
            .ok_or("APNs topic is invalid")?;
        let environment = match var(ENVIRONMENT_ENV).as_deref().map(str::trim) {
            Some("sandbox") => "sandbox",
            Some("production") => "production",
            _ => return Err("APNs environment must be sandbox or production"),
        };
        let key = var(KEY_ENV)
            .and_then(|raw| signing_key(&raw))
            .ok_or("APNs key is invalid")?;
        let config = Self {
            team_id,
            key_id,
            topic,
            environment,
            key,
        };
        // Fail at startup, not on the first approval.
        config
            .provider_token(unix_now())
            .ok_or("APNs key is invalid")?;
        Ok(Some(config))
    }

    fn base_url(&self) -> &'static str {
        if self.environment == "production" {
            "https://api.push.apple.com"
        } else {
            "https://api.sandbox.push.apple.com"
        }
    }

    /// ES256 provider token: `kid` = key id, `iss` = team id, `iat` = now.
    fn provider_token(&self, now: u64) -> Option<HeaderValue> {
        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some(self.key_id.clone());
        let jwt = jsonwebtoken::encode(
            &header,
            &json!({ "iss": self.team_id, "iat": now }),
            &self.key,
        )
        .ok()?;
        let mut value = HeaderValue::from_str(&format!("bearer {jwt}")).ok()?;
        value.set_sensitive(true);
        Some(value)
    }
}

/// Ten upper-case letters or digits, as Apple issues team and key ids.
fn apple_id(raw: Option<String>) -> Option<String> {
    let raw = raw?.trim().to_string();
    (raw.len() == 10
        && raw
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()))
    .then_some(raw)
}

/// A bundle identifier: dot-separated labels of letters, digits and hyphens.
fn valid_topic(topic: &str) -> bool {
    topic.len() <= 155
        && topic.contains('.')
        && topic.split('.').all(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

/// Accept the .p8 file's base64 body on one line (optionally still armored).
fn signing_key(raw: &str) -> Option<EncodingKey> {
    if raw.len() > MAX_KEY_BYTES {
        return None;
    }
    let body: String = raw
        .replace("-----BEGIN PRIVATE KEY-----", "")
        .replace("-----END PRIVATE KEY-----", "")
        .replace("\\n", "")
        .split_whitespace()
        .collect();
    if body.is_empty()
        || !body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
    {
        return None;
    }
    let pem = format!("-----BEGIN PRIVATE KEY-----\n{body}\n-----END PRIVATE KEY-----\n");
    EncodingKey::from_ec_pem(pem.as_bytes()).ok()
}

/// An APNs device token as registered: 64 to 200 lowercase hex characters.
/// Checked again here because it becomes part of the request path.
fn valid_token(token: &str) -> bool {
    (64..=200).contains(&token.len())
        && token
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Sent,
    /// The token is gone for good (410, or 400 BadDeviceToken): forget it.
    Unregistered,
    /// Transport error, 429 or 5xx.
    Retry,
    /// The provider token was refused as expired: mint a new one and retry.
    RefreshToken,
    Failed,
}

fn classify(status: u16, reason: &str) -> Outcome {
    match (status, reason) {
        (200, _) => Outcome::Sent,
        (410, _) | (400, "BadDeviceToken") => Outcome::Unregistered,
        (403, "ExpiredProviderToken") => Outcome::RefreshToken,
        (429, _) | (500..=599, _) => Outcome::Retry,
        _ => Outcome::Failed,
    }
}

pub struct Apns {
    config: Config,
    base_url: String,
    db: jarvis_store::Database,
    client: reqwest::Client,
    /// The cached provider token and its `iat` (Unix seconds).
    token: Option<(HeaderValue, u64)>,
    /// `APP_PUSH_BUDGET`; tests shorten it.
    budget: Duration,
}

impl Apns {
    pub fn new(
        config: Config,
        db: jarvis_store::Database,
        timeout: Duration,
    ) -> Result<Self, &'static str> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .http2_prior_knowledge()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(|_| "APNs client unavailable")?;
        Ok(Self {
            base_url: config.base_url().to_string(),
            config,
            db,
            client,
            token: None,
            budget: APP_PUSH_BUDGET,
        })
    }

    fn provider_token(&mut self) -> Option<HeaderValue> {
        let now = unix_now();
        if let Some((token, minted)) = &self.token {
            if now.saturating_sub(*minted) < JWT_LIFETIME_SECS {
                return Some(token.clone());
            }
        }
        let token = self.config.provider_token(now)?;
        self.token = Some((token.clone(), now));
        Some(token)
    }

    /// Whether Apple would accept a replacement for the cached token.
    fn token_replaceable(&self) -> bool {
        self.token
            .as_ref()
            .is_none_or(|(_, minted)| unix_now().saturating_sub(*minted) >= MIN_REFRESH_SECS)
    }

    /// Push `event` to the audience's active devices in the configured
    /// environment, skipping the requesting device, within the time budget.
    pub(super) async fn send(&mut self, event: NotifyEvent, audience: Audience, backoff: Duration) {
        if tokio::time::timeout(self.budget, self.send_unbounded(event, audience, backoff))
            .await
            .is_err()
        {
            tracing::warn!(
                kind = event.kind(),
                "app push gave up after its time budget"
            );
        }
    }

    async fn send_unbounded(&mut self, event: NotifyEvent, audience: Audience, backoff: Duration) {
        let targets = match jarvis_identity::apns_targets(
            &self.db,
            audience.user,
            audience.except,
            self.config.environment,
            MAX_TARGETS,
        )
        .await
        {
            Ok(targets) => targets,
            Err(_) => {
                tracing::warn!(kind = event.kind(), "app push targets unavailable");
                return;
            }
        };
        for target in targets.iter().filter(|target| valid_token(target)) {
            self.deliver(event, target, backoff).await;
        }
    }

    async fn deliver(&mut self, event: NotifyEvent, target: &str, backoff: Duration) {
        let mut delay = backoff;
        for attempt in 1..=MAX_ATTEMPTS {
            let outcome = match self.provider_token() {
                Some(token) => self.post(&token, event, target).await,
                None => Outcome::Failed,
            };
            match outcome {
                Outcome::Sent => {
                    tracing::info!(kind = event.kind(), "app push sent");
                    return;
                }
                Outcome::Unregistered => {
                    let removed = jarvis_identity::delete_apns_target(&self.db, target)
                        .await
                        .is_ok();
                    tracing::info!(kind = event.kind(), removed, "app push target unregistered");
                    return;
                }
                // Refused although younger than 20 minutes (e.g. clock skew):
                // a new token now would only draw TooManyProviderTokenUpdates.
                Outcome::RefreshToken if !self.token_replaceable() => {
                    tracing::warn!(
                        kind = event.kind(),
                        "APNs refused a provider token too new to replace"
                    );
                    return;
                }
                Outcome::Retry | Outcome::RefreshToken if attempt < MAX_ATTEMPTS => {
                    if outcome == Outcome::RefreshToken {
                        self.token = None;
                    }
                    tokio::time::sleep(delay).await;
                    delay *= 2;
                }
                _ => {
                    tracing::warn!(kind = event.kind(), attempt, "app push failed");
                    return;
                }
            }
        }
    }

    async fn post(&self, token: &HeaderValue, event: NotifyEvent, target: &str) -> Outcome {
        let response = request(
            &self.client,
            &self.base_url,
            &self.config.topic,
            token,
            event,
            target,
            unix_now(),
        )
        .send()
        .await;
        match response {
            Ok(response) => {
                let status = response.status().as_u16();
                // APNs answers errors with a small `{"reason": "..."}` body.
                let reason = if status == 200 {
                    String::new()
                } else {
                    response
                        .json::<serde_json::Value>()
                        .await
                        .ok()
                        .and_then(|body| body["reason"].as_str().map(str::to_owned))
                        .filter(|reason| {
                            reason.len() <= 64 && reason.bytes().all(|b| b.is_ascii_alphanumeric())
                        })
                        .unwrap_or_default()
                };
                let outcome = classify(status, &reason);
                if outcome != Outcome::Sent {
                    tracing::warn!(kind = event.kind(), status, reason, "APNs refused app push");
                }
                outcome
            }
            Err(error) => {
                // `without_url`: the device token is part of the path.
                tracing::warn!(kind = event.kind(), error = %error.without_url(), "APNs unreachable");
                Outcome::Retry
            }
        }
    }
}

/// One alert push with a static body. `link` is the deep link the app opens
/// when the notification is tapped; it names the approval kind only.
fn request(
    client: &reqwest::Client,
    base_url: &str,
    topic: &str,
    token: &HeaderValue,
    event: NotifyEvent,
    target: &str,
    now: u64,
) -> reqwest::RequestBuilder {
    let body = json!({
        "aps": { "alert": { "title": "Jarvis", "body": "Approval waiting" }, "sound": "default" },
        "link": event.click(),
    });
    client
        .post(format!("{base_url}/3/device/{target}"))
        .header(AUTHORIZATION, token.clone())
        .header(CONTENT_TYPE, "application/json")
        .header("apns-push-type", "alert")
        .header("apns-priority", "10")
        .header("apns-expiration", (now + EXPIRATION_SECS).to_string())
        .header("apns-topic", topic)
        .header("apns-collapse-id", event.kind())
        .body(body.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::{
        rand::SystemRandom,
        signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING},
    };

    /// A fresh P-256 key per test run, as the .p8 file's base64 body, and
    /// its public point.
    fn fixture_key() -> (String, Vec<u8>) {
        let pkcs8 =
            EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &SystemRandom::new())
                .unwrap();
        let pair = EcdsaKeyPair::from_pkcs8(
            &ECDSA_P256_SHA256_FIXED_SIGNING,
            pkcs8.as_ref(),
            &SystemRandom::new(),
        )
        .unwrap();
        (base64(pkcs8.as_ref()), pair.public_key().as_ref().to_vec())
    }

    fn base64(der: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in der.chunks(3) {
            let n = chunk
                .iter()
                .enumerate()
                .fold(0_u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    fn vars(key: &str) -> impl Fn(&str) -> Option<String> + '_ {
        move |name| {
            Some(
                match name {
                    TEAM_ID_ENV => "TEAM123456",
                    KEY_ID_ENV => "KEY1234567",
                    TOPIC_ENV => "com.example.jarvis.TEAM123456",
                    ENVIRONMENT_ENV => "sandbox",
                    KEY_ENV => key,
                    _ => return None,
                }
                .to_string(),
            )
        }
    }

    #[test]
    fn config_is_off_when_unset_and_refused_when_partial_or_malformed() {
        assert!(Config::from_vars(|_| None).unwrap().is_none());
        assert!(Config::from_vars(|_| Some(" ".into())).unwrap().is_none());
        let (key, _) = fixture_key();
        let config = Config::from_vars(vars(&key)).unwrap().unwrap();
        assert_eq!(config.base_url(), "https://api.sandbox.push.apple.com");
        // Also accepted: the armored file content with escaped newlines.
        let armored = format!("-----BEGIN PRIVATE KEY-----\\n{key}\\n-----END PRIVATE KEY-----");
        assert!(Config::from_vars(vars(&armored)).unwrap().is_some());
        for (name, bad) in [
            (TEAM_ID_ENV, "team123456"),
            (TEAM_ID_ENV, "TEAM12345"),
            (KEY_ID_ENV, "KEY-123456"),
            (TOPIC_ENV, "jarvis"),
            (TOPIC_ENV, "com.example/jarvis"),
            (TOPIC_ENV, "com..jarvis"),
            (ENVIRONMENT_ENV, "development"),
            (KEY_ENV, "bm90IGEga2V5"),
            (KEY_ENV, "not base64!"),
        ] {
            let result = Config::from_vars(|asked| {
                if asked == name {
                    Some(bad.to_string())
                } else {
                    vars(&key)(asked)
                }
            });
            assert!(result.is_err(), "{name}={bad}");
        }
        let missing =
            Config::from_vars(|asked| (asked != KEY_ENV).then(|| vars(&key)(asked)).flatten());
        assert_eq!(missing.err(), Some("APNs key is invalid"));
        let production = Config::from_vars(|asked| {
            if asked == ENVIRONMENT_ENV {
                Some("production".into())
            } else {
                vars(&key)(asked)
            }
        });
        assert_eq!(
            production.unwrap().unwrap().base_url(),
            "https://api.push.apple.com"
        );
    }

    #[test]
    fn provider_token_is_an_es256_jwt_for_the_team_and_key() {
        let (key, public) = fixture_key();
        let config = Config::from_vars(vars(&key)).unwrap().unwrap();
        let value = config.provider_token(1_700_000_000).unwrap();
        assert!(value.is_sensitive());
        let jwt = value.to_str().unwrap().strip_prefix("bearer ").unwrap();
        let mut validation = jsonwebtoken::Validation::new(Algorithm::ES256);
        validation.set_required_spec_claims::<&str>(&[]);
        validation.validate_exp = false;
        let decoded = jsonwebtoken::decode::<serde_json::Value>(
            jwt,
            &jsonwebtoken::DecodingKey::from_ec_der(&public),
            &validation,
        )
        .unwrap();
        assert_eq!(decoded.header.kid.as_deref(), Some("KEY1234567"));
        assert_eq!(
            decoded.claims,
            json!({ "iss": "TEAM123456", "iat": 1_700_000_000 })
        );
    }

    #[test]
    fn request_is_a_content_free_expiring_alert() {
        let client = reqwest::Client::new();
        let token = HeaderValue::from_static("bearer fixture");
        let target = "ab".repeat(32);
        let request = request(
            &client,
            "https://api.sandbox.push.apple.com",
            "com.example.jarvis",
            &token,
            NotifyEvent::UnlockRequested,
            &target,
            1_000,
        )
        .build()
        .unwrap();
        assert_eq!(
            request.url().as_str(),
            format!("https://api.sandbox.push.apple.com/3/device/{target}")
        );
        let headers = request.headers();
        for (name, value) in [
            ("authorization", "bearer fixture"),
            ("apns-push-type", "alert"),
            ("apns-priority", "10"),
            ("apns-expiration", "1120"),
            ("apns-topic", "com.example.jarvis"),
            ("apns-collapse-id", "unlock"),
        ] {
            assert_eq!(headers[name], value, "{name}");
        }
        let body: serde_json::Value =
            serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(
            body,
            json!({
                "aps": { "alert": { "title": "Jarvis", "body": "Approval waiting" }, "sound": "default" },
                "link": "jarvis://approvals/unlock",
            })
        );
    }

    #[test]
    fn answers_are_classified_for_retry_and_cleanup() {
        assert_eq!(classify(200, ""), Outcome::Sent);
        assert_eq!(classify(410, "Unregistered"), Outcome::Unregistered);
        assert_eq!(classify(400, "BadDeviceToken"), Outcome::Unregistered);
        // A topic or environment mistake must not wipe every target.
        assert_eq!(classify(400, "DeviceTokenNotForTopic"), Outcome::Failed);
        assert_eq!(classify(400, "BadTopic"), Outcome::Failed);
        assert_eq!(classify(403, "ExpiredProviderToken"), Outcome::RefreshToken);
        assert_eq!(classify(403, "InvalidProviderToken"), Outcome::Failed);
        assert_eq!(classify(429, "TooManyRequests"), Outcome::Retry);
        assert_eq!(classify(503, "ServiceUnavailable"), Outcome::Retry);
    }

    #[test]
    fn only_lowercase_hex_tokens_of_a_sane_length_are_used() {
        assert!(valid_token(&"0a".repeat(32)));
        assert!(valid_token(&"f".repeat(200)));
        for bad in [
            "0a".repeat(31) + "0",
            "f".repeat(201),
            "AB".repeat(32),
            format!("{}/..", "a".repeat(64)),
        ] {
            assert!(!valid_token(&bad), "{bad}");
        }
    }

    type Seen = std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>;

    /// Loopback h2c stand-in for APNs. Records (device token, authorization)
    /// per request and answers `reply`'s status and reason; `None` never
    /// answers.
    async fn mock_apns(reply: fn(&str, &str) -> Option<(u16, &'static str)>) -> (String, Seen) {
        use axum::{
            extract::{Path, State},
            http::{HeaderMap, StatusCode},
            routing::post,
            Router,
        };
        let seen = Seen::default();
        let app = Router::new()
            .route(
                "/3/device/{token}",
                post(
                    move |State(seen): State<Seen>,
                          Path(token): Path<String>,
                          headers: HeaderMap| async move {
                        let auth = headers
                            .get(AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or_default()
                            .to_string();
                        seen.lock().unwrap().push((token.clone(), auth.clone()));
                        let Some((status, reason)) = reply(&token, &auth) else {
                            return std::future::pending().await;
                        };
                        (
                            StatusCode::from_u16(status).unwrap(),
                            json!({ "reason": reason }).to_string(),
                        )
                    },
                ),
            )
            .with_state(seen.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });
        (url, seen)
    }

    /// An `Apns` sending to `url` (h2c, prior knowledge) with a fresh key.
    fn fixture_apns(db: jarvis_store::Database, url: String, timeout: Duration) -> (Apns, Vec<u8>) {
        let (key, public) = fixture_key();
        let config = Config::from_vars(vars(&key)).unwrap().unwrap();
        let mut apns = Apns::new(config, db, timeout).unwrap();
        apns.base_url = url;
        (apns, public)
    }

    async fn fixture_db() -> Result<jarvis_store::Database, Box<dyn std::error::Error>> {
        use surrealdb::{engine::remote::ws::Ws, opt::auth::Root, Surreal};
        let db = Surreal::new::<Ws>(std::env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
        db.signin(Root {
            username: &std::env::var("JARVIS_SURREAL_TEST_USER")?,
            password: &std::env::var("JARVIS_SURREAL_TEST_PASS")?,
        })
        .await?;
        db.use_ns(format!("apns_fixture_{}", uuid::Uuid::now_v7().simple()))
            .use_db("fixture")
            .await?;
        jarvis_store::apply_baseline_schema(&db).await?;
        Ok(db)
    }

    /// A fresh owner with one active iOS device per sandbox token.
    async fn owner_with_targets(
        db: &jarvis_store::Database,
        tokens: &[&str],
    ) -> Result<uuid::Uuid, Box<dyn std::error::Error>> {
        let owner = jarvis_identity::create_user(db, "fixture owner").await?.id;
        for token in tokens {
            let key = ed25519_dalek::SigningKey::from_bytes(&rand::random());
            let (device, _) = jarvis_identity::register_device(
                db,
                owner,
                "fixture phone",
                jarvis_identity::Platform::Ios,
                "ed25519",
                &key.verifying_key().to_bytes(),
            )
            .await?;
            jarvis_identity::set_push_target(db, device.id, token, "sandbox").await?;
        }
        Ok(owner)
    }

    #[tokio::test]
    async fn an_expired_provider_token_is_minted_again_and_retried() {
        let (url, seen) = mock_apns(|_, auth| {
            Some(if auth == "bearer stale" {
                (403, "ExpiredProviderToken")
            } else {
                (200, "")
            })
        })
        .await;
        // Never connected: this path needs no database.
        let (mut apns, public) =
            fixture_apns(surrealdb::Surreal::init(), url, Duration::from_secs(5));
        // Old enough that Apple accepts a replacement.
        apns.token = Some((
            HeaderValue::from_static("bearer stale"),
            unix_now() - MIN_REFRESH_SECS,
        ));
        let target = "0b".repeat(32);
        apns.deliver(
            NotifyEvent::UnlockRequested,
            &target,
            Duration::from_millis(10),
        )
        .await;
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert_eq!(seen[0], (target.clone(), "bearer stale".to_string()));
        let jwt = seen[1].1.strip_prefix("bearer ").unwrap();
        let mut validation = jsonwebtoken::Validation::new(Algorithm::ES256);
        validation.set_required_spec_claims::<&str>(&[]);
        validation.validate_exp = false;
        let fresh = jsonwebtoken::decode::<serde_json::Value>(
            jwt,
            &jsonwebtoken::DecodingKey::from_ec_der(&public),
            &validation,
        )
        .unwrap();
        assert_eq!(fresh.claims["iss"], "TEAM123456");
        // The new token is cached for the next push.
        assert_eq!(apns.token.as_ref().unwrap().0, seen[1].1.as_str());
    }

    #[tokio::test]
    async fn a_provider_token_younger_than_twenty_minutes_is_not_replaced() {
        let (url, seen) = mock_apns(|_, _| Some((403, "ExpiredProviderToken"))).await;
        let (mut apns, _) = fixture_apns(surrealdb::Surreal::init(), url, Duration::from_secs(5));
        let minted = unix_now() - MIN_REFRESH_SECS + 60;
        apns.token = Some((HeaderValue::from_static("bearer recent"), minted));
        apns.deliver(
            NotifyEvent::UnlockRequested,
            &"0b".repeat(32),
            Duration::from_millis(10),
        )
        .await;
        // One attempt, no new token: Apple would answer a replacement with
        // TooManyProviderTokenUpdates.
        assert_eq!(seen.lock().unwrap().len(), 1);
        let (token, kept) = apns.token.as_ref().unwrap();
        assert_eq!((token.to_str().unwrap(), *kept), ("bearer recent", minted));
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn an_unregistered_token_is_forgotten_without_retry(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let db = fixture_db().await?;
        let (gone, live) = ("0a".repeat(32), "0c".repeat(32));
        let owner = owner_with_targets(&db, &[&gone, &live]).await?;
        let (url, seen) = mock_apns(|token, _| {
            Some(if token.starts_with("0a") {
                (410, "Unregistered")
            } else {
                (200, "")
            })
        })
        .await;
        let (mut apns, _) = fixture_apns(db.clone(), url, Duration::from_secs(5));
        let audience = Audience {
            user: owner,
            except: None,
        };
        apns.send(
            NotifyEvent::UnlockRequested,
            audience,
            Duration::from_millis(10),
        )
        .await;
        let mut tokens: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|(t, _)| t.clone())
            .collect();
        tokens.sort();
        assert_eq!(tokens, [gone, live.clone()]);
        assert_eq!(
            jarvis_identity::apns_targets(&db, owner, None, "sandbox", 8).await?,
            [live]
        );
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn the_time_budget_cancels_a_hanging_push() -> Result<(), Box<dyn std::error::Error>> {
        let db = fixture_db().await?;
        let owner = owner_with_targets(&db, &[&"0d".repeat(32)]).await?;
        let (url, seen) = mock_apns(|_, _| None).await;
        // Only the budget can end this push: the request timeout is an hour.
        let (mut apns, _) = fixture_apns(db, url, Duration::from_secs(3600));
        apns.budget = Duration::from_millis(500);
        let started = std::time::Instant::now();
        let audience = Audience {
            user: owner,
            except: None,
        };
        apns.send(
            NotifyEvent::UnlockRequested,
            audience,
            Duration::from_millis(10),
        )
        .await;
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(seen.lock().unwrap().len(), 1);
        Ok(())
    }
}
