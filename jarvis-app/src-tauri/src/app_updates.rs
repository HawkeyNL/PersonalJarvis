//! Native-only private application updater.
//!
//! The bearer token and signing public key never enter the webview. Tauri owns
//! download signature verification and installation; the Home Node is only an
//! authenticated mirror.

use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::{ipc::Channel, AppHandle, State};
use tauri_plugin_updater::{Update, UpdaterExt};

use super::load_secure_auth;

pub(crate) struct PendingUpdate(pub Mutex<Option<Update>>);

pub(crate) struct UpdateRuntime {
    pub enabled: bool,
}

pub(crate) fn updater_public_key() -> Option<&'static str> {
    option_env!("JARVIS_TAURI_UPDATER_PUBKEY").filter(|value| !value.trim().is_empty())
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UpdateState {
    Ready,
    Unconfigured,
    Unauthenticated,
    Unsupported,
    UpToDate,
    Available,
    Installed,
}

#[derive(Serialize)]
pub(crate) struct UpdateStatus {
    state: UpdateState,
    current_version: String,
    version: Option<String>,
    notes: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub(crate) enum DownloadEvent {
    Started { content_length: Option<u64> },
    Progress { chunk_length: usize },
    Finished,
}

fn status(app: &AppHandle, state: UpdateState) -> UpdateStatus {
    UpdateStatus {
        state,
        current_version: app.package_info().version.to_string(),
        version: None,
        notes: None,
    }
}

fn endpoint_and_token(app: &AppHandle) -> Result<(String, String), UpdateStatus> {
    let auth = load_secure_auth(app).map_err(|_| status(app, UpdateState::Unsupported))?;
    let endpoint = auth
        .metadata
        .update_endpoint
        .ok_or_else(|| status(app, UpdateState::Unconfigured))?;
    let token = auth
        .token
        .filter(|value| !value.is_empty())
        .ok_or_else(|| status(app, UpdateState::Unauthenticated))?;
    Ok((endpoint, token))
}

#[tauri::command]
pub(crate) fn app_update_status(app: AppHandle, runtime: State<'_, UpdateRuntime>) -> UpdateStatus {
    if !runtime.enabled {
        return status(&app, UpdateState::Unsupported);
    }
    match endpoint_and_token(&app) {
        Ok(_) => status(&app, UpdateState::Ready),
        Err(state) => state,
    }
}

#[tauri::command]
pub(crate) async fn app_update_check(
    app: AppHandle,
    runtime: State<'_, UpdateRuntime>,
    pending: State<'_, PendingUpdate>,
) -> Result<UpdateStatus, String> {
    if !runtime.enabled {
        return Ok(status(&app, UpdateState::Unsupported));
    }
    let (endpoint, token) = match endpoint_and_token(&app) {
        Ok(values) => values,
        Err(state) => return Ok(state),
    };
    let endpoint =
        url::Url::parse(&endpoint).map_err(|_| "stored update endpoint is invalid".to_string())?;
    let updater = app
        .updater_builder()
        .endpoints(vec![endpoint])
        .map_err(|_| "update configuration failed".to_string())?
        .header("Authorization", format!("Bearer {token}"))
        .map_err(|_| "update authentication failed".to_string())?
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| "update service is unavailable".to_string())?;
    let update = updater
        .check()
        .await
        .map_err(|_| "update check failed".to_string())?;
    let Some(update) = update else {
        *pending
            .0
            .lock()
            .map_err(|_| "update state is unavailable".to_string())? = None;
        return Ok(status(&app, UpdateState::UpToDate));
    };
    let result = UpdateStatus {
        state: UpdateState::Available,
        current_version: update.current_version.clone(),
        version: Some(update.version.clone()),
        notes: update.body.clone(),
    };
    *pending
        .0
        .lock()
        .map_err(|_| "update state is unavailable".to_string())? = Some(update);
    Ok(result)
}

#[tauri::command]
pub(crate) async fn app_update_install(
    app: AppHandle,
    pending: State<'_, PendingUpdate>,
    on_event: Channel<DownloadEvent>,
) -> Result<UpdateStatus, String> {
    let update = pending
        .0
        .lock()
        .map_err(|_| "update state is unavailable".to_string())?
        .take()
        .ok_or_else(|| "there is no verified pending update".to_string())?;
    let mut started = false;
    update
        .download_and_install(
            |chunk_length, content_length| {
                if !started {
                    let _ = on_event.send(DownloadEvent::Started { content_length });
                    started = true;
                }
                let _ = on_event.send(DownloadEvent::Progress { chunk_length });
            },
            || {
                let _ = on_event.send(DownloadEvent::Finished);
            },
        )
        .await
        .map_err(|_| "update download, verification, or installation failed".to_string())?;
    Ok(status(&app, UpdateState::Installed))
}

#[tauri::command]
pub(crate) fn app_update_restart(app: AppHandle) {
    app.restart();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AuthMetadata;

    #[test]
    fn status_serialization_never_contains_endpoint_or_token_fields() {
        let encoded = serde_json::to_string(&UpdateStatus {
            state: UpdateState::Available,
            current_version: "1.0.0".to_string(),
            version: Some("1.1.0".to_string()),
            notes: Some("notes".to_string()),
        })
        .unwrap();
        assert!(!encoded.contains("token"));
        assert!(!encoded.contains("endpoint"));
        assert!(!encoded.contains("signature"));
    }

    #[test]
    fn update_endpoint_is_ordinary_metadata_only() {
        let metadata = AuthMetadata {
            device_id: Some("device".to_string()),
            update_endpoint: Some("https://home.invalid/v1/app-updates/stable".to_string()),
        };
        let encoded = serde_json::to_string(&metadata).unwrap();
        assert!(encoded.contains("update_endpoint"));
        assert!(!encoded.contains("Bearer"));
    }
}
