//! The narrow persistence boundary for Jarvis Core.
//!
//! Core connects to a remote SurrealDB instance over a private websocket.  It
//! deliberately does not expose generic query execution to domain callers:
//! identity, approvals and audit code own their typed repository operations.
//! This module only owns connection setup and the versioned baseline schema.

use surrealdb::{engine::remote::ws::Ws, opt::auth::Database as DatabaseAuth, Surreal};

#[derive(serde::Deserialize)]
struct SchemaVersion {
    version: i64,
}

/// Database failures are intentionally opaque to HTTP callers.  The API logs
/// the underlying error, while external responses remain generic.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database connection failed")]
    Connect(#[source] surrealdb::Error),
    #[error("database authentication failed")]
    Authenticate(#[source] surrealdb::Error),
    #[error("database schema migration failed")]
    Schema(#[source] surrealdb::Error),
    #[error("unsupported Jarvis schema version {0}")]
    UnsupportedSchema(i64),
}

/// Private, authenticated SurrealDB connection used by Jarvis Core.
pub type Database = Surreal<surrealdb::engine::remote::ws::Client>;

/// Connect to the configured private SurrealDB websocket endpoint and select
/// the supplied namespace/database. Production provisioning creates a
/// database-scoped principal; Core must never receive the SurrealDB root
/// credential.
pub async fn connect(
    endpoint: &str,
    namespace: &str,
    database: &str,
    username: &str,
    password: &str,
) -> Result<Database, StoreError> {
    let db = Surreal::new::<Ws>(endpoint)
        .await
        .map_err(StoreError::Connect)?;
    db.signin(DatabaseAuth {
        namespace,
        database,
        username,
        password,
    })
    .await
    .map_err(StoreError::Authenticate)?;
    db.use_ns(namespace)
        .use_db(database)
        .await
        .map_err(StoreError::Connect)?;
    Ok(db)
}

/// Apply the checked-in schema as a single database transaction. A failure
/// aborts startup; continuing with a partly-defined security schema would be
/// unsafe. This is an idempotent baseline for the empty, pre-production Home
/// Node only. Later schema changes must be explicit, versioned migrations.
pub async fn apply_baseline_schema(db: &Database) -> Result<(), StoreError> {
    let mut version = db
        .query("SELECT version FROM schema_version:baseline")
        .await
        .map_err(StoreError::Schema)?;
    let current: Option<SchemaVersion> = version.take(0).map_err(StoreError::Schema)?;
    if let Some(current) = current {
        if current.version == 1 {
            return Ok(());
        }
        // An unknown schema must never be silently overwritten or downgraded.
        return Err(StoreError::UnsupportedSchema(current.version));
    }

    db.query(include_str!("../../../schema/surreal/0001_baseline.surql"))
        .await
        .map_err(StoreError::Schema)?
        .check()
        .map_err(StoreError::Schema)?;
    Ok(())
}
