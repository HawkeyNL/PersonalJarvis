//! Jarvis client — native (Rust) side.
//!
//! Holds the device key material and does the signing. The private key is
//! generated here and stored in the app's private data directory; it is never
//! exposed to the webview/JS. (A follow-up will move it into the OS keychain.)

use ed25519_dalek::{Signer, SigningKey};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

/// Locally persisted auth material. Private to the app sandbox.
#[derive(Debug, Default, Serialize, Deserialize)]
struct AuthStore {
    /// Hex-encoded 32-byte Ed25519 seed (the device private key).
    private_key: Option<String>,
    /// The device id assigned by the server at enrollment.
    device_id: Option<String>,
    /// The current session token.
    token: Option<String>,
}

fn store_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    Ok(dir.join("auth.json"))
}

fn load_store(app: &AppHandle) -> AuthStore {
    store_path(app)
        .ok()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_store(app: &AppHandle, store: &AuthStore) -> Result<(), String> {
    let path = store_path(app)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(store).map_err(|e| e.to_string())?;
    std::fs::write(path, bytes).map_err(|e| e.to_string())
}

const KEY_SERVICE: &str = "com.hawkeynl.jarvis";
const KEY_ACCOUNT: &str = "device-private-key";

/// Read the device private key (hex), preferring the OS keychain and falling
/// back to the app-private file (also used to migrate older installs).
fn load_private_key(app: &AppHandle) -> Option<String> {
    if let Ok(entry) = keyring::Entry::new(KEY_SERVICE, KEY_ACCOUNT) {
        if let Ok(key) = entry.get_password() {
            return Some(key);
        }
    }
    load_store(app).private_key
}

/// Persist the device private key, preferring the OS keychain. Falls back to
/// the app-private file when the keychain is unavailable.
fn save_private_key(app: &AppHandle, key_hex: &str) -> Result<(), String> {
    if let Ok(entry) = keyring::Entry::new(KEY_SERVICE, KEY_ACCOUNT) {
        if entry.set_password(key_hex).is_ok() {
            // Now in the keychain: drop any copy left in the file.
            let mut store = load_store(app);
            if store.private_key.take().is_some() {
                let _ = save_store(app, &store);
            }
            return Ok(());
        }
    }
    let mut store = load_store(app);
    store.private_key = Some(key_hex.to_string());
    save_store(app, &store)
}

/// Load the device signing key, generating and persisting one on first use.
fn get_or_create_signing_key(app: &AppHandle) -> Result<SigningKey, String> {
    let key_hex = match load_private_key(app) {
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
    let seed: [u8; 32] = seed.try_into().map_err(|_| "invalid stored key".to_string())?;
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
    Ok(hex::encode(key.sign(&nonce).to_bytes()))
}

/// Persist the device id and session token after a successful login.
#[tauri::command]
fn auth_save(app: AppHandle, device_id: String, token: String) -> Result<(), String> {
    let mut store = load_store(&app);
    store.device_id = Some(device_id);
    store.token = Some(token);
    save_store(&app, &store)
}

/// Report the current auth state (without exposing the private key).
#[tauri::command]
fn auth_session(app: AppHandle) -> serde_json::Value {
    let store = load_store(&app);
    serde_json::json!({
        "device_id": store.device_id,
        "token": store.token,
        "has_key": load_private_key(&app).is_some(),
    })
}

/// Clear the session token (keeps the device key and id).
#[tauri::command]
fn auth_logout(app: AppHandle) -> Result<(), String> {
    let mut store = load_store(&app);
    store.token = None;
    save_store(&app, &store)
}

/// Fully deregister this device locally: wipe the device key (keychain + file),
/// the device id, and the session token. The next `login()` enrolls a fresh
/// device. Pair with a server-side `DELETE /v1/devices/{id}`.
#[tauri::command]
fn auth_reset(app: AppHandle) -> Result<(), String> {
    if let Ok(entry) = keyring::Entry::new(KEY_SERVICE, KEY_ACCOUNT) {
        let _ = entry.delete_credential(); // ignore "not found"
    }
    save_store(&app, &AuthStore::default())
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
        apple: reason.as_str(),
        windows: WindowsText::new("Jarvis", &reason)
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
        .invoke_handler(tauri::generate_handler![
            device_info,
            auth_public_key,
            auth_sign,
            auth_save,
            auth_session,
            auth_logout,
            auth_reset,
            biometric_unlock,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
