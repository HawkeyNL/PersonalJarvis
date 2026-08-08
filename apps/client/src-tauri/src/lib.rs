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

/// Load the device signing key, generating and persisting one on first use.
fn signing_key(store: &mut AuthStore) -> Result<SigningKey, String> {
    if store.private_key.is_none() {
        let mut seed = [0u8; 32];
        OsRng.fill_bytes(&mut seed);
        store.private_key = Some(hex::encode(seed));
    }
    let seed = hex::decode(store.private_key.as_ref().unwrap()).map_err(|e| e.to_string())?;
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
    let mut store = load_store(&app);
    let key = signing_key(&mut store)?;
    save_store(&app, &store)?;
    Ok(hex::encode(key.verifying_key().to_bytes()))
}

/// Sign a hex-encoded challenge nonce; returns the hex signature.
#[tauri::command]
fn auth_sign(app: AppHandle, nonce_hex: String) -> Result<String, String> {
    let mut store = load_store(&app);
    let key = signing_key(&mut store)?;
    save_store(&app, &store)?;
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
        "has_key": store.private_key.is_some(),
    })
}

/// Clear the session token (keeps the device key and id).
#[tauri::command]
fn auth_logout(app: AppHandle) -> Result<(), String> {
    let mut store = load_store(&app);
    store.token = None;
    save_store(&app, &store)
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
