//! Client tests against an in-process fake of the IBKR OAuth endpoints. The
//! fake checks the RSA and HMAC signatures independently of the client state,
//! so a wrongly signed request fails with 401. The RSA key under `testdata/`
//! was generated for these tests only and is not used anywhere else.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex as StdMutex,
    },
};

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use percent_encoding::percent_decode_str;
use ring::signature::{UnparsedPublicKey, RSA_PKCS1_2048_8192_SHA256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

const CONSUMER_KEY: &str = "TESTCONS";
const ACCESS_TOKEN: &str = "6f531f8fd316915af53f";
const SECRET: &[u8] = b"test-access-token-secret";

const TEST_KEY_PKCS8: &str = include_str!("testdata/test_only_rsa.pkcs8.pem");
const TEST_KEY_PKCS1: &str = include_str!("testdata/test_only_rsa.pkcs1.pem");

fn test_key() -> RsaKeyPair {
    signing_key(TEST_KEY_PKCS8).unwrap()
}

fn credentials() -> OAuthCredentials {
    OAuthCredentials {
        consumer_key: Zeroizing::new(CONSUMER_KEY.into()),
        access_token: Zeroizing::new(ACCESS_TOKEN.into()),
        secret: Zeroizing::new(SECRET.to_vec()),
        signing_key: test_key(),
        dh_prime: crypto::tests::prime(),
        realm: "limited_poa".into(),
    }
}

#[derive(Default)]
struct Fake {
    /// Status to return instead of the normal answer, per path suffix.
    fail: StdMutex<HashMap<&'static str, u16>>,
    bad_lst_signature: StdMutex<bool>,
    lst: StdMutex<Option<Vec<u8>>>,
    mints: AtomicUsize,
    requests: StdMutex<Vec<String>>,
    snapshot_calls: AtomicUsize,
}

struct Request {
    method: String,
    path: String,
    query: Vec<(String, String)>,
    headers: HashMap<String, String>,
    body: String,
}

fn oauth_params(header: &str) -> Vec<(String, String)> {
    header
        .strip_prefix("OAuth ")
        .unwrap()
        .split(", ")
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap();
            (key.to_string(), value.trim_matches('"').to_string())
        })
        .collect()
}

impl Fake {
    fn handle(&self, request: Request) -> (u16, String) {
        self.requests
            .lock()
            .unwrap()
            .push(format!("{} {}", request.method, request.path));
        let url = format!("http://{}{}", request.headers["host"], request.path);
        let mut params = oauth_params(&request.headers["authorization"]);
        assert_eq!(params[0], ("realm".into(), "limited_poa".into()));
        let signature = params
            .iter()
            .find(|(k, _)| k == "oauth_signature")
            .unwrap()
            .1
            .clone();
        params.retain(|(k, _)| k != "realm" && k != "oauth_signature");
        params.extend(request.query.iter().cloned());
        let pairs: Vec<(&str, &str)> = params
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let param = |name: &str| pairs.iter().find(|(k, _)| *k == name).map(|(_, v)| *v);
        assert_eq!(param("oauth_consumer_key"), Some(CONSUMER_KEY));
        assert_eq!(param("oauth_token"), Some(ACCESS_TOKEN));
        if let Some(status) = self
            .fail
            .lock()
            .unwrap()
            .iter()
            .find(|(suffix, _)| request.path.ends_with(*suffix))
            .map(|(_, status)| *status)
        {
            return (status, String::new());
        }

        if request.path.ends_with("/oauth/live_session_token") {
            assert_eq!(request.method, "POST");
            assert!(request.query.is_empty());
            assert_eq!(
                request.headers.get("content-length").map(String::as_str),
                Some("0")
            );
            assert_eq!(param("oauth_signature_method"), Some("RSA-SHA256"));
            let base = crypto::base_string(&hex::encode(SECRET), "POST", &url, &pairs);
            let raw = B64
                .decode(
                    percent_decode_str(&signature)
                        .decode_utf8()
                        .unwrap()
                        .as_bytes(),
                )
                .unwrap();
            UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, test_key().public().as_ref())
                .verify(base.as_bytes(), &raw)
                .expect("LST request signature");
            let p = crypto::tests::prime();
            let challenge = param("diffie_hellman_challenge").unwrap();
            let b = crypto::dh_private();
            let response = BigUint::from(2u8).modpow(&b, &p).to_str_radix(16);
            let challenge_back = BigUint::parse_bytes(challenge.as_bytes(), 16)
                .unwrap()
                .to_str_radix(16);
            let lst = crypto::live_session_token(&challenge_back, &b, &p, SECRET).unwrap();
            let mut mac = <hmac::Hmac<sha1::Sha1> as hmac::Mac>::new_from_slice(&lst).unwrap();
            hmac::Mac::update(&mut mac, CONSUMER_KEY.as_bytes());
            let mut lst_signature = hex::encode(hmac::Mac::finalize(mac).into_bytes());
            if *self.bad_lst_signature.lock().unwrap() {
                lst_signature.replace_range(
                    0..1,
                    if lst_signature.starts_with('0') {
                        "1"
                    } else {
                        "0"
                    },
                );
            } else {
                *self.lst.lock().unwrap() = Some(lst.to_vec());
            }
            self.mints.fetch_add(1, Ordering::SeqCst);
            let expires = (unix_now() + Duration::from_secs(86_400)).as_millis();
            return (
                200,
                format!(
                    r#"{{"diffie_hellman_response":"{response}","live_session_token_signature":"{lst_signature}","live_session_token_expiration":{expires}}}"#
                ),
            );
        }

        assert_eq!(param("oauth_signature_method"), Some("HMAC-SHA256"));
        let Some(lst) = self.lst.lock().unwrap().clone() else {
            return (401, String::new());
        };
        let base = crypto::base_string("", &request.method, &url, &pairs);
        if crypto::hmac_sha256_signature(&lst, &base) != signature {
            return (401, String::new());
        }
        let path = request.path.trim_start_matches("/v1/api");
        let body = match (request.method.as_str(), path) {
            ("GET", "/portfolio/accounts") => r#"[{"accountId":"DU1234567"}]"#,
            ("GET", "/portfolio/DU1234567/positions/0") => {
                r#"[{"conid":265598,"position":"10"},{"conid":8314,"position":2.5}]"#
            }
            ("GET", "/portfolio/DU1234567/positions/1") => "[]",
            ("GET", "/portfolio/DU1234567/ledger") => r#"{"BASE":{"cashbalance":12.5}}"#,
            ("POST", "/iserver/auth/ssodh/init") => {
                assert_eq!(request.body, SSODH_INIT_BODY);
                assert_eq!(request.headers["content-type"], "application/json");
                r#"{"authenticated":true,"competing":false,"connected":true}"#
            }
            ("GET", "/iserver/accounts") => r#"{"accounts":["DU1234567"]}"#,
            ("GET", "/iserver/marketdata/snapshot") => {
                assert_eq!(param("conids"), Some("265598"));
                if self.snapshot_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    r#"[{"conid":265598,"_updated":1}]"#
                } else {
                    r#"[{"conid":265598,"_updated":2,"31":"190.5","6509":"RpB"}]"#
                }
            }
            ("GET", "/iserver/marketdata/history") => {
                r#"{"symbol":"AAPL","data":[{"o":1,"c":2,"h":3,"l":0.5,"v":10,"t":1}]}"#
            }
            ("POST", "/tickle")
                if request.headers.get("content-length").map(String::as_str) == Some("0") =>
            {
                r#"{"session":"s","iserver":{"authStatus":{"authenticated":false}}}"#
            }
            _ => return (404, String::new()),
        };
        (200, body.to_string())
    }

    fn count(&self, path: &str) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.ends_with(path))
            .count()
    }
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> Option<Request> {
    let mut raw = Vec::new();
    let mut buf = [0; 4096];
    let head_end = loop {
        let n = socket.read(&mut buf).await.ok()?;
        if n == 0 {
            return None;
        }
        raw.extend_from_slice(&buf[..n]);
        if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let head = String::from_utf8(raw[..head_end].to_vec()).ok()?;
    let mut lines = head.split("\r\n");
    let mut start = lines.next()?.split(' ');
    let method = start.next()?.to_string();
    let target = start.next()?;
    let headers: HashMap<String, String> = lines
        .filter_map(|line| line.split_once(": "))
        .map(|(k, v)| (k.to_ascii_lowercase(), v.to_string()))
        .collect();
    let length: usize = headers
        .get("content-length")
        .map_or(0, |v| v.parse().unwrap());
    let mut body = raw[head_end..].to_vec();
    while body.len() < length {
        let n = socket.read(&mut buf).await.ok()?;
        body.extend_from_slice(&buf[..n]);
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let query = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap();
            (k.to_string(), v.to_string())
        })
        .collect();
    Some(Request {
        method,
        path: path.to_string(),
        query,
        headers,
        body: String::from_utf8(body).ok()?,
    })
}

async fn fake_client() -> (IbkrOAuthClient, Arc<Fake>) {
    let fake = Arc::new(Fake::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1/api", listener.local_addr().unwrap());
    let server = fake.clone();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let server = server.clone();
            tokio::spawn(async move {
                let Some(request) = read_request(&mut socket).await else {
                    return;
                };
                let (status, body) = server.handle(request);
                let response = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    let http = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .unwrap();
    (IbkrOAuthClient::with_parts(base, http, credentials()), fake)
}

#[tokio::test]
async fn mints_once_then_signs_paged_reads() {
    let (client, fake) = fake_client().await;
    assert_eq!(client.accounts().await.unwrap()[0].account_id, "DU1234567");
    let positions = client.positions("DU1234567").await.unwrap();
    assert_eq!(positions.len(), 2);
    assert_eq!(positions[1].position, Some("2.5".parse().unwrap()));
    assert_eq!(fake.count("/positions/0") + fake.count("/positions/1"), 2);
    let ledger = client.ledger("DU1234567").await.unwrap();
    assert_eq!(ledger["BASE"].cashbalance, Some("12.5".parse().unwrap()));
    assert_eq!(fake.mints.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn concurrent_callers_share_one_mint() {
    let (client, fake) = fake_client().await;
    let client = Arc::new(client);
    let calls: Vec<_> = (0..5)
        .map(|_| {
            let client = client.clone();
            tokio::spawn(async move { client.ledger("DU1234567").await })
        })
        .collect();
    for call in calls {
        call.await.unwrap().unwrap();
    }
    assert_eq!(fake.mints.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn refuses_an_lst_with_a_bad_signature() {
    let (client, fake) = fake_client().await;
    *fake.bad_lst_signature.lock().unwrap() = true;
    let error = client.ledger("DU1234567").await.unwrap_err();
    assert!(matches!(error, IbkrError::Handshake(_)), "{error:?}");
    assert_eq!(
        fake.count("/ledger"),
        0,
        "no request with an unvalidated LST"
    );
    *fake.bad_lst_signature.lock().unwrap() = false;
    // A failed mint is not retried at once: the cooldown answers locally.
    let error = client.ledger("DU1234567").await.unwrap_err();
    assert!(
        matches!(error, IbkrError::MintCooldown { retry_after_secs } if retry_after_secs <= 30)
    );
    assert_eq!(fake.mints.load(Ordering::SeqCst), 1);
    end_cooldown(&client).await;
    client.ledger("DU1234567").await.unwrap();
    assert_eq!(fake.mints.load(Ordering::SeqCst), 2);
}

async fn end_cooldown(client: &IbkrOAuthClient) {
    client.session.lock().await.last_mint = None;
}

#[tokio::test]
async fn unauthorized_forces_a_fresh_lst_and_brokerage_session() {
    let (client, fake) = fake_client().await;
    client.history(265598, "1y", "1d").await.unwrap();
    fake.fail.lock().unwrap().insert("/history", 401);
    let error = client.history(265598, "1y", "1d").await.unwrap_err();
    assert!(matches!(error, IbkrError::Unauthorized), "{error:?}");
    fake.fail.lock().unwrap().clear();
    end_cooldown(&client).await;
    let history = client.history(265598, "1y", "1d").await.unwrap();
    assert_eq!(history.bars[0].low, Some("0.5".parse().unwrap()));
    assert_eq!(fake.mints.load(Ordering::SeqCst), 2);
    assert_eq!(fake.count("/ssodh/init"), 2);
}

#[tokio::test]
async fn rate_limit_backs_off_without_sending() {
    let (client, fake) = fake_client().await;
    client.ledger("DU1234567").await.unwrap();
    fake.fail.lock().unwrap().insert("/ledger", 429);
    let error = client.ledger("DU1234567").await.unwrap_err();
    assert!(
        matches!(
            error,
            IbkrError::RateLimited {
                retry_after_secs: 600
            }
        ),
        "{error:?}"
    );
    let sent = fake.requests.lock().unwrap().len();
    let error = client.ledger("DU1234567").await.unwrap_err();
    assert!(matches!(error, IbkrError::RateLimited { retry_after_secs } if retry_after_secs > 590));
    assert_eq!(fake.requests.lock().unwrap().len(), sent);
}

#[tokio::test]
async fn snapshot_opens_brokerage_once_and_repeats_the_preflight() {
    let (client, fake) = fake_client().await;
    let snapshot = client.snapshot(&[265598]).await.unwrap();
    assert_eq!(snapshot[0].last, Some("190.5".parse().unwrap()));
    assert_eq!(
        snapshot[0].availability,
        Some(MarketDataAvailability::RealTime)
    );
    assert_eq!(fake.snapshot_calls.load(Ordering::SeqCst), 2);
    client.snapshot(&[265598]).await.unwrap();
    assert_eq!(fake.snapshot_calls.load(Ordering::SeqCst), 3);
    assert_eq!(fake.count("/ssodh/init"), 1);
    assert_eq!(fake.count("/iserver/accounts"), 1);
    let order = fake.requests.lock().unwrap().clone();
    let init = order
        .iter()
        .position(|r| r.ends_with("/ssodh/init"))
        .unwrap();
    let snap = order.iter().position(|r| r.ends_with("/snapshot")).unwrap();
    assert!(init < snap);
}

#[tokio::test]
async fn tickle_reports_a_lost_brokerage_session() {
    let (client, fake) = fake_client().await;
    client.search("AAPL").await.unwrap_err(); // fake has no search route: 404
    assert_eq!(fake.count("/ssodh/init"), 1);
    assert!(!client.tickle().await.unwrap().authenticated);
    client.history(265598, "30d", "1h").await.unwrap();
    assert_eq!(fake.count("/ssodh/init"), 2);
}

#[tokio::test]
async fn errors_do_not_leak_credentials_or_queries() {
    let (client, fake) = fake_client().await;
    fake.fail.lock().unwrap().insert("/history", 500);
    let error = client.history(4242424, "1y", "1d").await.unwrap_err();
    assert!(
        matches!(&error, IbkrError::Http(e) if e.is_status()),
        "{error:?}"
    );
    *fake.bad_lst_signature.lock().unwrap() = true;
    let (client2, fake2) = fake_client().await;
    *fake2.bad_lst_signature.lock().unwrap() = true;
    let handshake = client2.accounts().await.unwrap_err();
    for error in [error, handshake] {
        let text = format!("{error} {error:?}");
        for leaked in [
            "4242424",
            "period",
            "127.0.0.1",
            CONSUMER_KEY,
            ACCESS_TOKEN,
            &hex::encode(SECRET),
        ] {
            assert!(!text.contains(leaked), "{text}");
        }
    }
}

#[tokio::test]
async fn refuses_invalid_parameters_before_sending() {
    let (client, fake) = fake_client().await;
    let too_many = vec![1; MAX_SNAPSHOT_CONIDS + 1];
    let results = [
        client.positions("../iserver/account").await.err(),
        client.ledger("").await.err(),
        client.search("AAPL&x=1").await.err(),
        client.search("BRK B").await.err(),
        client.snapshot(&[]).await.err(),
        client.snapshot(&too_many).await.err(),
        client.snapshot(&[0]).await.err(),
        client.history(-1, "1y", "1d").await.err(),
        client.history(1, "0d", "1d").await.err(),
        client.history(1, "1y", "1y").await.err(),
        client.history(1, "12345d", "1d").await.err(),
        client.history(1, "1y&x", "1d").await.err(),
    ];
    for result in results {
        assert!(
            matches!(result, Some(IbkrError::InvalidInput(_))),
            "{result:?}"
        );
    }
    assert!(fake.requests.lock().unwrap().is_empty());
}

#[test]
fn pacer_spaces_requests_and_blocks_after_429() {
    let pacer = Pacer::new();
    let first = pacer.reserve(false).unwrap();
    assert_eq!(pacer.reserve(false).unwrap() - first, GLOBAL_GAP);
    let accounts = pacer.reserve(true).unwrap();
    assert_eq!(accounts - first, GLOBAL_GAP * 2);
    let next_accounts = pacer.reserve(true).unwrap();
    assert_eq!(next_accounts - accounts, ACCOUNTS_GAP);
    assert_eq!(pacer.reserve(false).unwrap() - next_accounts, GLOBAL_GAP);
    // Slots at +10.2 s .. +25.2 s fit the 30 s queue limit; +30.2 s does not.
    for _ in 0..4 {
        pacer.reserve(true).unwrap();
    }
    assert!(matches!(pacer.reserve(true), Err(IbkrError::Busy)));
    pacer.block();
    assert!(matches!(
        pacer.reserve(false),
        Err(IbkrError::RateLimited { retry_after_secs }) if retry_after_secs > 590
    ));
}

#[test]
fn loads_credential_files_and_redacts_debug() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, contents: &str| {
        let path = dir.path().join(name);
        std::fs::write(&path, contents).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        path
    };
    let files = OAuthCredentialFiles {
        consumer_key: write("consumer_key", "TESTCONS\n"),
        access_token: write("access_token", ACCESS_TOKEN),
        // What the setup step stores after decrypting the portal's ciphertext.
        access_token_secret_prepend_hex: write("prepend", &format!("{}\n", hex::encode(SECRET))),
        signature_key: write("sig.pem", TEST_KEY_PKCS8),
        dh_param: write(
            "dhparam.pem",
            &crypto::tests::dhparam_pem(&crypto::tests::prime(), 2, None),
        ),
    };
    let creds = OAuthCredentials::load(&files, "limited_poa").unwrap();
    assert_eq!(*creds.secret, SECRET);
    assert_eq!(creds.consumer_key.as_str(), CONSUMER_KEY);
    let debug = format!("{creds:?}");
    assert_eq!(debug, r#"OAuthCredentials { realm: "limited_poa", .. }"#);

    let lowercase = OAuthCredentialFiles {
        consumer_key: write("lower", "testcons"),
        ..files.clone()
    };
    let error = OAuthCredentials::load(&lowercase, "limited_poa").unwrap_err();
    assert!(matches!(error, IbkrError::Credential("consumer key")));
    assert!(!format!("{error} {error:?}").contains("testcons"));

    let missing = OAuthCredentialFiles {
        access_token: dir.path().join("absent"),
        ..files.clone()
    };
    let error = OAuthCredentials::load(&missing, "limited_poa").unwrap_err();
    assert!(matches!(error, IbkrError::Credential("access token")));
    assert!(!format!("{error:?}").contains("absent"));

    assert!(matches!(
        OAuthCredentials::load(&files, "Limited POA"),
        Err(IbkrError::Credential("realm"))
    ));
    // PKCS#1 (OpenSSL 1.x) is accepted as well as PKCS#8 (OpenSSL 3).
    let pkcs1 = OAuthCredentialFiles {
        signature_key: write("sig1.pem", TEST_KEY_PKCS1),
        ..files.clone()
    };
    OAuthCredentials::load(&pkcs1, "limited_poa").unwrap();
    let not_a_key = OAuthCredentialFiles {
        signature_key: write(
            "sig2.pem",
            "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n",
        ),
        ..files.clone()
    };
    assert!(matches!(
        OAuthCredentials::load(&not_a_key, "limited_poa"),
        Err(IbkrError::Credential("signature key"))
    ));
    for bad in ["", "abc", "zz", "6162 63"] {
        let prepend = OAuthCredentialFiles {
            access_token_secret_prepend_hex: write("bad", bad),
            ..files.clone()
        };
        let error = OAuthCredentials::load(&prepend, "limited_poa").unwrap_err();
        assert!(
            matches!(error, IbkrError::Credential("access token secret prepend")),
            "{bad}"
        );
    }
}

#[test]
fn refuses_unsafe_credential_files() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("consumer_key");
    std::fs::write(&path, "TESTCONS").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        read_secret(&path, "consumer key").unwrap().as_str(),
        "TESTCONS"
    );

    for mode in [0o640, 0o604, 0o660] {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        assert!(
            matches!(
                read_secret(&path, "consumer key"),
                Err(IbkrError::Credential("consumer key"))
            ),
            "{mode:o}"
        );
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(matches!(
        read_secret(&link, "consumer key"),
        Err(IbkrError::Credential(_))
    ));
    assert!(matches!(
        read_secret(dir.path(), "consumer key"),
        Err(IbkrError::Credential(_))
    ));

    let big = dir.path().join("big");
    std::fs::write(&big, vec![b'A'; MAX_CREDENTIAL_BYTES as usize + 1]).unwrap();
    std::fs::set_permissions(&big, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(matches!(
        read_secret(&big, "consumer key"),
        Err(IbkrError::Credential(_))
    ));
}

#[tokio::test]
async fn failed_refresh_keeps_the_still_valid_lst() {
    let (client, fake) = fake_client().await;
    client.ledger("DU1234567").await.unwrap();
    // Move into the refresh margin, past the mint cooldown.
    {
        let mut state = client.session.lock().await;
        state.current.as_mut().unwrap().expires_at = Instant::now() + Duration::from_secs(60);
        state.last_mint = None;
    }
    *fake.bad_lst_signature.lock().unwrap() = true;
    client.ledger("DU1234567").await.unwrap();
    assert_eq!(
        fake.mints.load(Ordering::SeqCst),
        2,
        "refresh was attempted"
    );
    // Within the cooldown the old LST keeps serving without another mint.
    client.ledger("DU1234567").await.unwrap();
    assert_eq!(fake.mints.load(Ordering::SeqCst), 2);
    // At its real expiry it is dropped.
    client
        .session
        .lock()
        .await
        .current
        .as_mut()
        .unwrap()
        .expires_at = Instant::now();
    let error = client.ledger("DU1234567").await.unwrap_err();
    assert!(matches!(error, IbkrError::MintCooldown { .. }), "{error:?}");
}
