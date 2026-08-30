//! Native-only private application updater.
//!
//! The bearer token and signing public key never enter the webview. Tauri owns
//! download signature verification and installation; the Home Node is only an
//! authenticated mirror.

use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{ipc::Channel, AppHandle, State};
use tauri_plugin_updater::{Update, UpdaterExt};

use super::{load_secure_auth, normalize_home_node_origin};

pub(crate) const CURRENT_DESKTOP_UPDATE_PROTOCOL: u32 = 1;
const MAX_CAPABILITY_BYTES: usize = 64 * 1024;

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
    Incompatible,
    Unavailable,
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

fn status_with_notes(app: &AppHandle, state: UpdateState, notes: String) -> UpdateStatus {
    let mut result = status(app, state);
    result.notes = Some(notes);
    result
}

fn origin_and_token(app: &AppHandle) -> Result<(String, String), UpdateStatus> {
    let auth = load_secure_auth(app).map_err(|_| status(app, UpdateState::Unsupported))?;
    let origin = auth
        .metadata
        .home_node_origin
        .ok_or_else(|| status(app, UpdateState::Unconfigured))?;
    let token = auth
        .token
        .filter(|value| !value.is_empty())
        .ok_or_else(|| status(app, UpdateState::Unauthenticated))?;
    Ok((origin, token))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateCapability {
    schema_version: u32,
    channel: String,
    check_endpoint: String,
    minimum_client_protocol: u32,
}

fn validate_capability(
    capability: UpdateCapability,
    enrolled_origin: &str,
    allow_local_http: bool,
) -> Result<String, &'static str> {
    if capability.schema_version != 1 || capability.channel != "stable" {
        return Err("update capability is incompatible");
    }
    if capability.minimum_client_protocol == 0 {
        return Err("update capability protocol is invalid");
    }
    if capability.minimum_client_protocol > CURRENT_DESKTOP_UPDATE_PROTOCOL {
        return Err("release requires a newer updater protocol");
    }
    for placeholder in ["{{target}}", "{{arch}}", "{{current_version}}"] {
        if !capability.check_endpoint.contains(placeholder) {
            return Err("update capability endpoint is incomplete");
        }
    }
    let parsed = url::Url::parse(&capability.check_endpoint)
        .map_err(|_| "update capability endpoint is invalid")?;
    let origin = parsed.origin().ascii_serialization();
    let origin = normalize_home_node_origin(&origin, allow_local_http)
        .map_err(|_| "update capability endpoint is insecure")?;
    let enrolled_origin = normalize_home_node_origin(enrolled_origin, allow_local_http)
        .map_err(|_| "enrolled Home Node origin is invalid")?;
    if origin != enrolled_origin {
        return Err("update capability endpoint changed origin");
    }
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err("update capability endpoint contains credentials");
    }
    Ok(capability.check_endpoint)
}

async fn discover_endpoint(
    app: &AppHandle,
    origin: &str,
    token: &str,
) -> Result<String, UpdateStatus> {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| status(app, UpdateState::Unsupported))?;
    let response = client
        .get(format!("{origin}/v1/app-updates/capability"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| {
            status_with_notes(
                app,
                UpdateState::Unavailable,
                "Updateservice niet bereikbaar".into(),
            )
        })?;
    if !response.status().is_success() {
        return Err(status_with_notes(
            app,
            UpdateState::Unavailable,
            "Updateservice niet beschikbaar".into(),
        ));
    }
    if response
        .content_length()
        .is_some_and(|size| size > MAX_CAPABILITY_BYTES as u64)
    {
        return Err(status(app, UpdateState::Unsupported));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| status(app, UpdateState::Unsupported))?;
    if bytes.len() > MAX_CAPABILITY_BYTES {
        return Err(status(app, UpdateState::Unsupported));
    }
    let capability: UpdateCapability =
        serde_json::from_slice(&bytes).map_err(|_| status(app, UpdateState::Unsupported))?;
    match validate_capability(capability, origin, cfg!(debug_assertions)) {
        Ok(endpoint) => Ok(endpoint),
        Err("release requires a newer updater protocol") => Err(status_with_notes(
            app,
            UpdateState::Incompatible,
            format!(
                "Deze release vereist een nieuwer updateprotocol dan versie {}",
                CURRENT_DESKTOP_UPDATE_PROTOCOL
            ),
        )),
        Err(_) => Err(status(app, UpdateState::Unsupported)),
    }
}

#[tauri::command]
pub(crate) fn app_update_status(app: AppHandle, runtime: State<'_, UpdateRuntime>) -> UpdateStatus {
    if !runtime.enabled {
        return status(&app, UpdateState::Unsupported);
    }
    match origin_and_token(&app) {
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
    let (origin, token) = match origin_and_token(&app) {
        Ok(values) => values,
        Err(state) => return Ok(state),
    };
    let endpoint = match discover_endpoint(&app, &origin, &token).await {
        Ok(endpoint) => endpoint,
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
        // Home Node update routes do not redirect. Refusing all redirects
        // keeps the native bearer credential pinned to the enrolled origin.
        .configure_client(|client| client.redirect(reqwest::redirect::Policy::none()))
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
    let download_origin = update.download_url.origin().ascii_serialization();
    let download_origin = normalize_home_node_origin(&download_origin, cfg!(debug_assertions))
        .map_err(|_| "update download endpoint is insecure".to_string())?;
    if download_origin != origin
        || !update.download_url.username().is_empty()
        || update.download_url.password().is_some()
    {
        return Ok(status_with_notes(
            &app,
            UpdateState::Unsupported,
            "Update-downloadadres hoort niet bij de gekoppelde Home Node".into(),
        ));
    }
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
    fn home_node_origin_is_ordinary_metadata_only() {
        let metadata = AuthMetadata {
            device_id: Some("device".to_string()),
            home_node_origin: Some("https://home.invalid".to_string()),
        };
        let encoded = serde_json::to_string(&metadata).unwrap();
        assert!(encoded.contains("home_node_origin"));
        assert!(!encoded.contains("Bearer"));
    }

    fn capability(minimum_client_protocol: u32, endpoint: &str) -> UpdateCapability {
        UpdateCapability {
            schema_version: 1,
            channel: "stable".into(),
            check_endpoint: endpoint.into(),
            minimum_client_protocol,
        }
    }

    #[test]
    fn capability_enforces_desktop_protocol_compatibility() {
        let endpoint =
            "https://home.invalid/v1/app-updates/stable/{{target}}/{{arch}}/{{current_version}}";
        assert_eq!(
            validate_capability(capability(1, endpoint), "https://home.invalid", false).unwrap(),
            endpoint
        );
        assert_eq!(
            validate_capability(capability(2, endpoint), "https://home.invalid", false),
            Err("release requires a newer updater protocol")
        );
        assert!(
            validate_capability(capability(0, endpoint), "https://home.invalid", false).is_err()
        );
    }

    #[test]
    fn capability_rejects_insecure_or_incomplete_endpoints() {
        assert!(validate_capability(
            capability(
                1,
                "http://home.invalid/v1/{{target}}/{{arch}}/{{current_version}}"
            ),
            "https://home.invalid",
            false
        )
        .is_err());
        assert!(validate_capability(
            capability(1, "https://home.invalid/v1/{{target}}/{{arch}}"),
            "https://home.invalid",
            false
        )
        .is_err());
        assert!(validate_capability(
            capability(
                1,
                "https://token@home.invalid/v1/{{target}}/{{arch}}/{{current_version}}"
            ),
            "https://home.invalid",
            false
        )
        .is_err());
    }

    #[test]
    fn capability_cannot_move_native_authorization_to_another_origin() {
        for endpoint in [
            "https://other.invalid/v1/{{target}}/{{arch}}/{{current_version}}",
            "https://home.invalid:444/v1/{{target}}/{{arch}}/{{current_version}}",
            "http://home.invalid/v1/{{target}}/{{arch}}/{{current_version}}",
        ] {
            assert_eq!(
                validate_capability(capability(1, endpoint), "https://home.invalid", false),
                Err(if endpoint.starts_with("http:") {
                    "update capability endpoint is insecure"
                } else {
                    "update capability endpoint changed origin"
                })
            );
        }
    }

    #[test]
    fn update_download_origin_must_match_enrolled_origin() {
        let enrolled = normalize_home_node_origin("https://home.invalid", false).unwrap();
        for value in [
            "https://other.invalid/update.tar.gz",
            "https://home.invalid:444/update.tar.gz",
            "http://home.invalid/update.tar.gz",
        ] {
            let url = url::Url::parse(value).unwrap();
            let candidate = normalize_home_node_origin(&url.origin().ascii_serialization(), false);
            assert!(candidate.is_err() || candidate.unwrap() != enrolled);
        }
    }
}
