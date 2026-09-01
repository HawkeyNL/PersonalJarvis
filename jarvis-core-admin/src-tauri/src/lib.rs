#![cfg_attr(not(feature = "desktop"), allow(dead_code, unused_imports))]

mod admin;
mod logs;
mod session;

#[cfg(feature = "desktop")]
use std::sync::Arc;

#[cfg(feature = "desktop")]
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|_| "administration worker stopped unexpectedly".to_owned())?
}

#[cfg(feature = "desktop")]
async fn with_session<T: Send + 'static>(
    session: tauri::State<'_, Arc<session::SessionManager>>,
    work: impl FnOnce(Arc<session::SessionManager>) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let session = Arc::clone(session.inner());
    blocking(move || work(session)).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn session_authenticate(
    session: tauri::State<'_, Arc<session::SessionManager>>,
) -> Result<session::SessionStatus, String> {
    with_session(session, |session| session.authenticate()).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn session_touch(
    session: tauri::State<'_, Arc<session::SessionManager>>,
) -> Result<session::SessionStatus, String> {
    with_session(session, |session| session.touch()).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn session_lock(
    session: tauri::State<'_, Arc<session::SessionManager>>,
) -> Result<session::SessionStatus, String> {
    with_session(session, |session| session.lock()).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn overview(
    session: tauri::State<'_, Arc<session::SessionManager>>,
) -> Result<admin::OverviewResponse, String> {
    with_session(session, |session| admin::overview(&session)).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn health(
    session: tauri::State<'_, Arc<session::SessionManager>>,
    run_verification: bool,
) -> Result<admin::HealthResponse, String> {
    with_session(session, move |session| {
        admin::health(&session, run_verification)
    })
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn services(
    session: tauri::State<'_, Arc<session::SessionManager>>,
) -> Result<Vec<admin::ServiceRecord>, String> {
    with_session(session, |session| admin::services(&session)).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn update_status(
    session: tauri::State<'_, Arc<session::SessionManager>>,
    check: bool,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    with_session(session, move |session| {
        admin::update_status(&session, check)
    })
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn update_mutation(
    session: tauri::State<'_, Arc<session::SessionManager>>,
    request: admin::UpdateMutation,
) -> Result<admin::OperationResult, String> {
    with_session(session, move |session| {
        admin::update_mutation(&session, request)
    })
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn agents(
    session: tauri::State<'_, Arc<session::SessionManager>>,
) -> Result<admin::AgentsResponse, String> {
    with_session(session, |session| admin::agents(&session)).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn agent_action(
    session: tauri::State<'_, Arc<session::SessionManager>>,
    update: bool,
) -> Result<admin::OperationResult, String> {
    with_session(session, move |session| {
        admin::agent_action(&session, update)
    })
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn models(
    session: tauri::State<'_, Arc<session::SessionManager>>,
) -> Result<Vec<admin::ModelRecord>, String> {
    with_session(session, |session| admin::models(&session)).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn usage(
    session: tauri::State<'_, Arc<session::SessionManager>>,
) -> Result<admin::UsageReport, String> {
    with_session(session, |session| admin::usage(&session)).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn model_mutation(
    session: tauri::State<'_, Arc<session::SessionManager>>,
    request: admin::ModelMutation,
) -> Result<admin::OperationResult, String> {
    with_session(session, move |session| {
        admin::model_mutation(&session, request)
    })
    .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn credentials(
    session: tauri::State<'_, Arc<session::SessionManager>>,
) -> Result<Vec<admin::CredentialRecord>, String> {
    with_session(session, |session| admin::credentials(&session)).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn logs(
    session: tauri::State<'_, Arc<session::SessionManager>>,
    query: admin::LogQuery,
) -> Result<admin::LogResponse, String> {
    with_session(session, move |session| admin::logs(&session, query)).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn system(
    session: tauri::State<'_, Arc<session::SessionManager>>,
) -> Result<admin::SystemResponse, String> {
    with_session(session, |session| {
        session.require_active()?;
        admin::system()
    })
    .await
}

#[cfg(feature = "desktop")]
pub fn run() {
    if let Err(error) = admin::root_guard() {
        eprintln!("jarvis-core-admin: {error}");
        std::process::exit(1);
    }
    tauri::Builder::default()
        .manage(Arc::new(session::SessionManager::new()))
        .invoke_handler(tauri::generate_handler![
            session_authenticate,
            session_touch,
            session_lock,
            overview,
            health,
            services,
            update_status,
            update_mutation,
            agents,
            agent_action,
            models,
            usage,
            model_mutation,
            credentials,
            logs,
            system
        ])
        .run(tauri::generate_context!())
        .expect("error while running Jarvis Core Administration");
}

pub fn broker_requested() -> bool {
    session::broker_requested()
}

pub fn component_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub fn frontend_mode() -> &'static str {
    if cfg!(feature = "custom-protocol") {
        "production"
    } else {
        "development"
    }
}

pub fn run_broker() -> Result<(), String> {
    session::run_broker()
}
