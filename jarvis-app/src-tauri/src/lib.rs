//! Jarvis client — native (Rust) side.
//!
//! Holds device key material and does the signing. Private keys and session
//! tokens live in the operating system credential store and never enter the
//! app's ordinary metadata file. The private key is never exposed to JS.

use ed25519_dalek::{Signer, SigningKey};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;
use tauri::{AppHandle, Manager};

#[cfg(desktop)]
mod app_updates;

/// Legacy desktop auth file. It is read only to migrate existing installs.
#[derive(Debug, Default, Serialize, Deserialize)]
struct LegacyAuthStore {
    private_key: Option<String>,
    device_id: Option<String>,
    token: Option<String>,
    #[serde(default)]
    update_endpoint: Option<String>,
    #[serde(default)]
    home_node_origin: Option<String>,
}

/// Ordinary, non-secret metadata that may remain in the app data directory.
#[derive(Debug, Default, Serialize, Deserialize)]
struct AuthMetadata {
    device_id: Option<String>,
    #[serde(default)]
    home_node_origin: Option<String>,
}

#[derive(Debug, Serialize)]
struct HomeNodeConfig {
    origin: Option<String>,
    configured: bool,
}

fn store_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    Ok(dir.join("auth.json"))
}

fn load_legacy_store(app: &AppHandle) -> Result<LegacyAuthStore, String> {
    let path = store_path(app)?;
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LegacyAuthStore::default())
        }
        Err(_) => return Err("authentication metadata is unreadable".to_string()),
    };
    serde_json::from_slice(&bytes).map_err(|_| "authentication metadata is corrupt".to_string())
}

fn metadata_bytes(metadata: &AuthMetadata) -> Result<Vec<u8>, String> {
    serde_json::to_vec_pretty(metadata).map_err(|e| e.to_string())
}

fn load_metadata(app: &AppHandle) -> Result<AuthMetadata, String> {
    let legacy = load_legacy_store(app)?;
    let home_node_origin = validated_stored_origin(&legacy);
    Ok(AuthMetadata {
        device_id: legacy.device_id,
        home_node_origin,
    })
}

fn validated_stored_origin(legacy: &LegacyAuthStore) -> Option<String> {
    legacy
        .home_node_origin
        .as_deref()
        .and_then(|origin| normalize_home_node_origin(origin, cfg!(debug_assertions)).ok())
        .or_else(|| {
            legacy
                .update_endpoint
                .as_deref()
                .and_then(origin_from_legacy_update_endpoint)
        })
}

fn save_metadata(app: &AppHandle, metadata: &AuthMetadata) -> Result<(), String> {
    let path = store_path(app)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let bytes = metadata_bytes(metadata)?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(temporary, path).map_err(|e| e.to_string())
}

const KEY_SERVICE: &str = "com.hawkeynl.jarvis";
const KEY_ACCOUNT: &str = "device-private-key";
const TOKEN_ACCOUNT: &str = "session-token";

fn credential(account: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(KEY_SERVICE, account)
        .map_err(|_| "secure credential storage is unavailable".to_string())
}

fn load_credential(account: &str) -> Result<Option<String>, String> {
    match credential(account)?.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(_) => Err("secure credential storage is unavailable".to_string()),
    }
}

fn save_credential(account: &str, value: &str) -> Result<(), String> {
    credential(account)?
        .set_password(value)
        .map_err(|_| "secure credential storage is unavailable".to_string())
}

fn delete_credential(account: &str) -> Result<(), String> {
    match credential(account)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(_) => Err("secure credential storage is unavailable".to_string()),
    }
}

struct SecureAuth {
    metadata: AuthMetadata,
    private_key: Option<String>,
    token: Option<String>,
}

/// Migrate both legacy secrets as one operation. The plaintext file is only
/// rewritten after every present secret is safely in the OS credential store,
/// so a partial migration can never discard the device identity.
fn load_secure_auth(app: &AppHandle) -> Result<SecureAuth, String> {
    let legacy = load_legacy_store(app)?;
    let home_node_origin = validated_stored_origin(&legacy);
    let metadata = AuthMetadata {
        device_id: legacy.device_id.clone(),
        home_node_origin,
    };
    let had_legacy_secrets = legacy.private_key.is_some() || legacy.token.is_some();
    let mut private_key = load_credential(KEY_ACCOUNT)?;
    let mut token = load_credential(TOKEN_ACCOUNT)?;
    if private_key.is_none() {
        if let Some(value) = legacy.private_key {
            save_credential(KEY_ACCOUNT, &value)?;
            private_key = Some(value);
        }
    }
    if token.is_none() {
        if let Some(value) = legacy.token {
            save_credential(TOKEN_ACCOUNT, &value)?;
            token = Some(value);
        }
    }
    if had_legacy_secrets {
        save_metadata(app, &metadata)?;
    }
    Ok(SecureAuth {
        metadata,
        private_key,
        token,
    })
}

fn load_private_key(app: &AppHandle) -> Result<Option<String>, String> {
    Ok(load_secure_auth(app)?.private_key)
}

fn save_private_key(app: &AppHandle, key_hex: &str) -> Result<(), String> {
    save_credential(KEY_ACCOUNT, key_hex)?;
    save_metadata(app, &load_metadata(app)?)
}

/// Load the device signing key, generating and persisting one on first use.
fn get_or_create_signing_key(app: &AppHandle) -> Result<SigningKey, String> {
    let key_hex = match load_private_key(app)? {
        Some(key) => key,
        None => {
            let mut seed = [0u8; 32];
            OsRng.fill_bytes(&mut seed);
            let key_hex = hex::encode(seed);
            save_private_key(app, &key_hex)?;
            key_hex
        }
    };
    let seed = hex::decode(&key_hex).map_err(|e| e.to_string())?;
    let seed: [u8; 32] = seed
        .try_into()
        .map_err(|_| "invalid stored key".to_string())?;
    Ok(SigningKey::from_bytes(&seed))
}

#[tauri::command]
fn device_info() -> serde_json::Value {
    let platform = if cfg!(target_os = "ios") {
        "ios"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "android") {
        "android"
    } else {
        "linux"
    };
    serde_json::json!({ "platform": platform, "name": format!("Jarvis ({platform})") })
}

/// Return the device public key (hex), generating a keypair on first call.
#[tauri::command]
fn auth_public_key(app: AppHandle) -> Result<String, String> {
    let key = get_or_create_signing_key(&app)?;
    Ok(hex::encode(key.verifying_key().to_bytes()))
}

/// Sign a hex-encoded challenge nonce; returns the hex signature.
#[tauri::command]
fn auth_sign(app: AppHandle, nonce_hex: String) -> Result<String, String> {
    let key = get_or_create_signing_key(&app)?;
    let nonce = hex::decode(nonce_hex).map_err(|e| e.to_string())?;
    if nonce.len() != 32 {
        return Err("invalid challenge".to_string());
    }
    Ok(hex::encode(key.sign(&nonce).to_bytes()))
}

/// Sign only the canonical device-pairing approval protocol. Keeping this
/// separate from generic challenge signing prevents a UI bug from turning an
/// arbitrary server string into a privileged enrollment approval.
#[tauri::command]
// Tauri exposes these as separately named IPC fields; wrapping them would
// silently change the command contract used by the Vue approval flow.
#[allow(clippy::too_many_arguments)]
fn auth_sign_pairing_approval(
    app: AppHandle,
    candidate_name: String,
    request_id: String,
    nonce_hex: String,
    candidate_public_key_hex: String,
    user_id: String,
    approver_device_id: String,
    expires_at: i64,
) -> Result<String, String> {
    if candidate_name.trim().is_empty() || candidate_name.len() > 128 {
        return Err("invalid device name".to_string());
    }
    let request_id = uuid::Uuid::parse_str(&request_id).map_err(|e| e.to_string())?;
    let user_id = uuid::Uuid::parse_str(&user_id).map_err(|e| e.to_string())?;
    let approver_device_id =
        uuid::Uuid::parse_str(&approver_device_id).map_err(|e| e.to_string())?;
    let nonce = hex::decode(nonce_hex).map_err(|e| e.to_string())?;
    let public_key = hex::decode(candidate_public_key_hex).map_err(|e| e.to_string())?;
    if nonce.len() != 32 || public_key.len() != 32 {
        return Err("invalid pairing payload".to_string());
    }
    if expires_at <= time::OffsetDateTime::now_utc().unix_timestamp() {
        return Err("pairing request expired".to_string());
    }
    let local_device_id = load_metadata(&app)?
        .device_id
        .ok_or_else(|| "device is not enrolled".to_string())?;
    if local_device_id != approver_device_id.to_string() {
        return Err("pairing approver does not match this device".to_string());
    }
    authenticate_owner(&format!("{} koppelen", candidate_name.trim()), true)?;
    let expires_at = time::OffsetDateTime::from_unix_timestamp(expires_at)
        .map_err(|_| "invalid pairing expiry".to_string())?;
    let message = jarvis_client_core::pairing_approval_message(
        request_id,
        &nonce,
        &public_key,
        user_id,
        approver_device_id,
        expires_at,
    )
    .map_err(|_| "invalid pairing payload".to_string())?;
    let key = get_or_create_signing_key(&app)?;
    Ok(hex::encode(key.sign(&message).to_bytes()))
}

/// Persist the device id and session token after a successful login.
fn save_auth(app: &AppHandle, device_id: String, token: &str) -> Result<(), String> {
    load_secure_auth(app)?;
    save_credential(TOKEN_ACCOUNT, token)?;
    let current = load_metadata(app)?;
    let metadata = AuthMetadata {
        device_id: Some(device_id),
        home_node_origin: current.home_node_origin,
    };
    if let Err(error) = save_metadata(app, &metadata) {
        let _ = delete_credential(TOKEN_ACCOUNT);
        return Err(error);
    }
    Ok(())
}

fn native_http_client() -> Result<reqwest::Client, String> {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|_| "native HTTP client is unavailable".to_string())
}

fn authenticated_api_path(path: &str) -> bool {
    if path.len() > 2048
        || !path.starts_with("/v1/")
        || path.starts_with("//")
        || path.contains(['\\', '#'])
        || path.split('/').any(|part| part == "..")
    {
        return false;
    }
    let route = path.split('?').next().unwrap_or(path);
    [
        "/v1/assistant",
        "/v1/conversations",
        "/v1/devices",
        "/v1/auth/logout",
        "/v1/auth/me",
        "/v1/auth/pairing/requests",
        "/v1/auth/unlock",
        "/v1/system",
        "/v1/holdings",
        "/v1/broker",
        "/v1/voice",
    ]
    .iter()
    .any(|prefix| {
        route == *prefix
            || route
                .strip_prefix(prefix)
                .is_some_and(|remainder| remainder.starts_with('/'))
    })
}

fn api_url(app: &AppHandle, path: &str) -> Result<String, String> {
    if !authenticated_api_path(path) {
        return Err("authenticated API path is invalid".to_string());
    }
    let origin = load_metadata(app)?
        .home_node_origin
        .ok_or_else(|| "Home Node is not configured".to_string())?;
    Ok(format!("{origin}{path}"))
}

fn contains_secret_response_field(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(fields) => fields.iter().any(|(key, value)| {
            matches!(
                key.to_ascii_lowercase().as_str(),
                "token" | "access_token" | "refresh_token" | "private_key"
            ) || contains_secret_response_field(value)
        }),
        serde_json::Value::Array(values) => values.iter().any(contains_secret_response_field),
        _ => false,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginResponse {
    token: String,
}

/// Complete device login and persist the returned bearer without ever
/// serializing it through Tauri IPC or the webview.
#[tauri::command]
async fn auth_complete_login(
    app: AppHandle,
    device_id: String,
    challenge_id: String,
    signature: String,
) -> Result<(), String> {
    uuid::Uuid::parse_str(&device_id).map_err(|_| "login device id is invalid".to_string())?;
    uuid::Uuid::parse_str(&challenge_id)
        .map_err(|_| "login challenge id is invalid".to_string())?;
    if signature.len() != 128 || !signature.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("login signature is invalid".to_string());
    }
    let origin = load_metadata(&app)?
        .home_node_origin
        .ok_or_else(|| "Home Node is not configured".to_string())?;
    let response = native_http_client()?
        .post(format!("{origin}/v1/auth/login"))
        .json(&serde_json::json!({
            "device_id": device_id,
            "challenge_id": challenge_id,
            "signature": signature,
        }))
        .send()
        .await
        .map_err(|_| "Home Node login is unavailable".to_string())?;
    if !response.status().is_success() {
        return Err("Home Node rejected the device login".to_string());
    }
    if response.content_length().is_some_and(|length| length > 16 * 1024) {
        return Err("Home Node login response is invalid".to_string());
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| "Home Node login response is invalid".to_string())?;
    if bytes.len() > 16 * 1024 {
        return Err("Home Node login response is invalid".to_string());
    }
    let result: LoginResponse = serde_json::from_slice(&bytes)
        .map_err(|_| "Home Node login response is invalid".to_string())?;
    if result.token.is_empty()
        || result.token.len() > 4096
        || result.token.chars().any(char::is_whitespace)
    {
        return Err("Home Node login response is invalid".to_string());
    }
    save_auth(&app, device_id, &result.token)
}

#[derive(Serialize)]
struct NativeApiResponse {
    status: u16,
    body: Option<serde_json::Value>,
}

/// Proxy authenticated JSON calls through Rust so the webview never receives
/// or constructs a bearer credential. Only relative Jarvis API paths and the
/// small method set used by the desktop client are accepted.
#[tauri::command]
async fn auth_request(
    app: AppHandle,
    method: String,
    path: String,
    body: Option<serde_json::Value>,
) -> Result<NativeApiResponse, String> {
    let auth = load_secure_auth(&app)?;
    let token = auth
        .token
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "device is not authenticated".to_string())?;
    let method = match method.as_str() {
        "GET" => reqwest::Method::GET,
        "POST" => reqwest::Method::POST,
        "DELETE" => reqwest::Method::DELETE,
        _ => return Err("authenticated API method is invalid".to_string()),
    };
    if method != reqwest::Method::POST && body.is_some() {
        return Err("authenticated API body is invalid".to_string());
    }
    let mut request = native_http_client()?
        .request(method.clone(), api_url(&app, &path)?)
        .bearer_auth(token)
        .header(reqwest::header::ACCEPT, "application/json");
    if method == reqwest::Method::POST {
        request = request.json(&body.unwrap_or_else(|| serde_json::json!({})));
    }
    let response = request
        .send()
        .await
        .map_err(|_| "Home Node is unreachable".to_string())?;
    let status = response.status().as_u16();
    if response.content_length().is_some_and(|length| length > 16 * 1024 * 1024) {
        return Err("Home Node response is too large".to_string());
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| "Home Node response is invalid".to_string())?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err("Home Node response is too large".to_string());
    }
    let body = if bytes.is_empty() {
        None
    } else {
        let value = serde_json::from_slice(&bytes)
            .map_err(|_| "Home Node response is not valid JSON".to_string())?;
        if contains_secret_response_field(&value) {
            return Err("Home Node response contains a forbidden secret field".to_string());
        }
        Some(value)
    };
    Ok(NativeApiResponse { status, body })
}

/// Non-secret auth metadata for reactive UI. Use this instead of retaining a
/// bearer token in Vue state.
#[tauri::command]
fn auth_status(app: AppHandle) -> Result<serde_json::Value, String> {
    let auth = load_secure_auth(&app)?;
    Ok(serde_json::json!({
        "device_id": auth.metadata.device_id,
        "authenticated": auth.token.is_some(),
        "has_key": auth.private_key.is_some(),
    }))
}

/// Clear the session token (keeps the device key and id).
#[tauri::command]
fn auth_logout(app: AppHandle) -> Result<(), String> {
    let auth = load_secure_auth(&app)?;
    delete_credential(TOKEN_ACCOUNT)?;
    save_metadata(&app, &auth.metadata)
}

/// Fully deregister this device locally: wipe the device key (keychain + file),
/// the device id, and the session token. The next `login()` enrolls a fresh
/// device. Pair with a server-side `DELETE /v1/devices/{id}`.
#[tauri::command]
fn auth_reset(app: AppHandle) -> Result<(), String> {
    let home_node_origin = load_metadata(&app)?.home_node_origin;
    delete_credential(KEY_ACCOUNT)?;
    delete_credential(TOKEN_ACCOUNT)?;
    save_metadata(
        &app,
        &AuthMetadata {
            device_id: None,
            home_node_origin,
        },
    )
}

/// Return only ordinary, non-secret Home Node connection metadata.
#[tauri::command]
fn home_node_config(app: AppHandle) -> Result<HomeNodeConfig, String> {
    let origin = load_metadata(&app)?.home_node_origin;
    Ok(HomeNodeConfig {
        configured: origin.is_some(),
        origin,
    })
}

/// Persist a credential-free origin before enrollment. Release builds require
/// HTTPS; loopback HTTP is accepted only by debug builds for local development.
#[tauri::command]
fn home_node_configure(app: AppHandle, origin: String) -> Result<HomeNodeConfig, String> {
    let origin = normalize_home_node_origin(&origin, cfg!(debug_assertions))?;
    let mut metadata = load_metadata(&app)?;
    if origin_changed(metadata.home_node_origin.as_deref(), &origin) {
        // A bearer is scoped to the Home Node that minted it. Clear both the
        // token and server-side device id before persisting a different origin
        // so the old credential can never be sent to a newly entered host.
        delete_credential(TOKEN_ACCOUNT)?;
        metadata.device_id = None;
    }
    metadata.home_node_origin = Some(origin.clone());
    save_metadata(&app, &metadata)?;
    Ok(HomeNodeConfig {
        origin: Some(origin),
        configured: true,
    })
}

fn origin_changed(current: Option<&str>, requested: &str) -> bool {
    current != Some(requested)
}

fn normalize_home_node_origin(value: &str, allow_local_http: bool) -> Result<String, String> {
    let parsed =
        url::Url::parse(value.trim()).map_err(|_| "Home Node origin is ongeldig".to_string())?;
    let is_loopback = match parsed.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    };
    let secure_scheme = parsed.scheme() == "https"
        || (allow_local_http && parsed.scheme() == "http" && is_loopback);
    if !secure_scheme
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !matches!(parsed.path(), "" | "/")
    {
        return Err(
            "Home Node vereist een credential-vrije HTTPS-origin (lokale HTTP alleen in development)"
                .to_string(),
        );
    }
    Ok(parsed.origin().ascii_serialization())
}

fn origin_from_legacy_update_endpoint(value: &str) -> Option<String> {
    let parsed = url::Url::parse(value).ok()?;
    if !parsed.path().starts_with("/v1/app-updates/") {
        return None;
    }
    normalize_home_node_origin(
        &parsed.origin().ascii_serialization(),
        cfg!(debug_assertions),
    )
    .ok()
}

/// Prompt the OS for local verification (Touch ID / Face ID).
///
/// `allow_password` controls the device-password fallback. The desktop passes
/// `false` (biometrics-only): if biometrics fail, the app falls back to phone
/// approval rather than the desktop password. The phone passes `true`, so its
/// own biometrics fall back to the phone's passcode — the always-with-you route.
/// Returns `Ok(())` only on a successful verification; any failure,
/// cancellation, or lack of hardware is an `Err`.
#[tauri::command]
fn biometric_unlock(reason: String, allow_password: bool) -> Result<(), String> {
    authenticate_owner(&reason, allow_password)
}

fn authenticate_owner(reason: &str, allow_password: bool) -> Result<(), String> {
    use std::sync::mpsc;
    use std::time::Duration;

    use robius_authentication::{
        AndroidText, BiometricStrength, Context, PolicyBuilder, Text, WindowsText,
    };

    let policy = PolicyBuilder::new()
        .biometrics(Some(BiometricStrength::Strong))
        .password(allow_password)
        .companion(false)
        .build()
        .ok_or_else(|| "biometrics not supported".to_string())?;

    let text = Text {
        android: AndroidText {
            title: "Jarvis",
            subtitle: None,
            description: None,
        },
        apple: reason,
        windows: WindowsText::new("Jarvis", reason)
            .ok_or_else(|| "invalid prompt text".to_string())?,
    };

    // `authenticate` is callback-based (synchronous on Apple, async elsewhere);
    // bridge it to a blocking result so the command returns the verdict.
    let (tx, rx) = mpsc::channel();
    Context::new(())
        .authenticate(text, &policy, move |res| {
            let _ = tx.send(res.is_ok());
        })
        .map_err(|e| format!("{e:?}"))?;

    match rx.recv_timeout(Duration::from_secs(120)) {
        Ok(true) => Ok(()),
        Ok(false) => Err("authentication failed".into()),
        Err(_) => Err("authentication timed out".into()),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_http::init())
        .setup(|app| {
            #[cfg(desktop)]
            {
                use std::sync::Mutex;

                app.manage(app_updates::PendingUpdate(Mutex::new(None)));
                let enabled = app_updates::updater_public_key().is_some();
                app.manage(app_updates::UpdateRuntime { enabled });
                if let Some(public_key) = app_updates::updater_public_key() {
                    app.handle().plugin(
                        tauri_plugin_updater::Builder::new()
                            .pubkey(public_key)
                            .build(),
                    )?;
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            device_info,
            auth_public_key,
            auth_sign,
            auth_sign_pairing_approval,
            auth_complete_login,
            auth_request,
            auth_status,
            auth_logout,
            auth_reset,
            home_node_config,
            home_node_configure,
            biometric_unlock,
            #[cfg(desktop)]
            app_updates::app_update_status,
            #[cfg(desktop)]
            app_updates::app_update_check,
            #[cfg(desktop)]
            app_updates::app_update_install,
            #[cfg(desktop)]
            app_updates::app_update_restart,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_serialization_never_contains_legacy_secrets() {
        let legacy: LegacyAuthStore = serde_json::from_str(
            r#"{"private_key":"private","device_id":"device","token":"token"}"#,
        )
        .unwrap();
        let metadata = AuthMetadata {
            device_id: legacy.device_id,
            home_node_origin: None,
        };
        let encoded = String::from_utf8(metadata_bytes(&metadata).unwrap()).unwrap();
        assert!(encoded.contains("device"));
        assert!(!encoded.contains("private"));
        assert!(!encoded.contains("token"));
    }

    #[test]
    fn signing_key_round_trip_signs_without_exposing_seed() {
        let seed = [7_u8; 32];
        let signing_key = SigningKey::from_bytes(&seed);
        let signature = signing_key.sign(&[9_u8; 32]);
        assert!(signing_key
            .verifying_key()
            .verify_strict(&[9_u8; 32], &signature)
            .is_ok());
    }

    #[test]
    fn home_node_origin_has_no_production_localhost_fallback_or_credentials() {
        assert_eq!(
            normalize_home_node_origin("https://home.example/", false).unwrap(),
            "https://home.example"
        );
        assert!(normalize_home_node_origin("http://localhost:8080", false).is_err());
        assert!(normalize_home_node_origin("http://home.example", true).is_err());
        assert!(normalize_home_node_origin("https://token@home.example", false).is_err());
        assert!(normalize_home_node_origin("https://home.example/other", false).is_err());
        assert!(normalize_home_node_origin("https://home.example?token=x", false).is_err());
    }

    #[test]
    fn local_http_is_an_explicit_debug_only_option() {
        assert_eq!(
            normalize_home_node_origin("http://127.0.0.1:8080", true).unwrap(),
            "http://127.0.0.1:8080"
        );
        assert_eq!(
            normalize_home_node_origin("http://[::1]:8080", true).unwrap(),
            "http://[::1]:8080"
        );
    }

    #[test]
    fn legacy_updater_endpoint_migrates_only_its_origin() {
        assert_eq!(
            origin_from_legacy_update_endpoint(
                "https://home.example/v1/app-updates/stable/{{target}}/{{arch}}/{{current_version}}"
            ),
            Some("https://home.example".to_string())
        );
        assert_eq!(
            origin_from_legacy_update_endpoint("https://home.example/not-updates"),
            None
        );
    }

    #[test]
    fn changing_home_node_requires_a_fresh_server_binding() {
        assert!(!origin_changed(
            Some("https://home.example"),
            "https://home.example"
        ));
        assert!(origin_changed(
            Some("https://old.example"),
            "https://new.example"
        ));
        assert!(origin_changed(None, "https://home.example"));
    }

    #[test]
    fn stored_home_node_origin_is_revalidated_before_native_use() {
        let legacy = LegacyAuthStore {
            home_node_origin: Some("https://token@home.example".to_string()),
            ..LegacyAuthStore::default()
        };
        assert_eq!(validated_stored_origin(&legacy), None);
    }

    #[test]
    fn native_authenticated_proxy_is_bounded_and_excludes_updater_routes() {
        assert!(authenticated_api_path("/v1/conversations"));
        assert!(authenticated_api_path("/v1/auth/unlock/pending?wait=20"));
        assert!(!authenticated_api_path("/v1/auth/login"));
        assert!(!authenticated_api_path("/v1/app-updates/capability"));
        assert!(!authenticated_api_path("https://other.example/v1/devices"));
        assert!(!authenticated_api_path("/v1/devices/../auth/login"));
    }

    #[test]
    fn native_proxy_refuses_secret_response_fields() {
        assert!(contains_secret_response_field(
            &serde_json::json!({"token": "secret"})
        ));
        assert!(contains_secret_response_field(
            &serde_json::json!({"nested": [{"private_key": "secret"}]})
        ));
        assert!(!contains_secret_response_field(
            &serde_json::json!({"authenticated": true, "device_id": "id"})
        ));
    }
}
