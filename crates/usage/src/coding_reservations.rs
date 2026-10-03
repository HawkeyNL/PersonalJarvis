//! Server-issued subscription execution reservations. These records never
//! enter the EUR API ledger and a caller-provided UUID is never authority.
use jarvis_store::{Database, StoreError};
use serde_json::{json, Value};
use uuid::Uuid;

pub const MAX_RUNTIME_SECS: u32 = 600;
// The current reviewed relay authorizes one provider operation. Raise this
// only together with an audited iterative tool protocol and its tests.
pub const MAX_PROVIDER_TURNS: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeasedReservation {
    pub id: Uuid,
    pub run_id: Uuid,
    pub max_runtime_secs: u32,
    pub max_provider_turns: u32,
}

/// Core calls this only after authenticating the owner and checking the
/// logical coding session. The TTL is intentionally short: the device must
/// sign the exact returned ID before the broker can lease it.
pub async fn reserve(db: &Database, user_id: Uuid, session_id: Uuid) -> Result<Uuid, StoreError> {
    let id = Uuid::now_v7();
    db.query("CREATE coding_reservations SET id=$id,user_id=$user,coding_session_id=$coding_session_key,purpose='codex_coding_run',compute_class='subscription',api_spend_cents=0,execution_units=1,max_runtime_secs=$runtime,max_provider_turns=$turns,status='reserved',run_id=NONE,created_at=time::now(),updated_at=time::now(),expires_at=time::now()+5m RETURN NONE")
        .bind(json!({"id":id.to_string(),"user":user_id.to_string(),"coding_session_key":session_id.to_string(),"runtime":MAX_RUNTIME_SECS,"turns":MAX_PROVIDER_TURNS}))
        .await.map_err(StoreError::schema)?.check().map_err(StoreError::schema)?;
    Ok(id)
}

/// A conditional database update is the single-use lease point. All binding
/// fields are checked in the same statement, so two brokers cannot each
/// consume the same reservation. A released record can never be re-leased.
pub async fn lease(
    db: &Database,
    id: Uuid,
    user_id: Uuid,
    session_id: Uuid,
    run_id: Uuid,
    requested_runtime_secs: u64,
) -> Result<Option<LeasedReservation>, StoreError> {
    let mut response = db.query("UPDATE coding_reservations SET status='leased',run_id=$run,updated_at=time::now() WHERE record::id(id)=$id AND user_id=$user AND coding_session_id=$coding_session_key AND purpose='codex_coding_run' AND compute_class='subscription' AND api_spend_cents=0 AND execution_units=1 AND status='reserved' AND expires_at>time::now() AND max_runtime_secs >= $runtime RETURN record::id(id) AS id,max_runtime_secs,max_provider_turns")
        .bind(json!({"id":id.to_string(),"user":user_id.to_string(),"coding_session_key":session_id.to_string(),"run":run_id.to_string(),"runtime":requested_runtime_secs}))
        .await.map_err(StoreError::schema)?.check().map_err(StoreError::schema)?;
    let rows: Vec<Value> = response.take(0).map_err(StoreError::schema)?;
    let Some(row) = rows.into_iter().next() else {
        return Ok(None);
    };
    let runtime = row
        .get("max_runtime_secs")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok());
    let turns = row
        .get("max_provider_turns")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok());
    match (runtime, turns) {
        (Some(max_runtime_secs), Some(max_provider_turns)) => Ok(Some(LeasedReservation {
            id,
            run_id,
            max_runtime_secs,
            max_provider_turns,
        })),
        _ => Ok(None),
    }
}

/// A terminal subscription run settles at exactly zero API cents. Failures,
/// cancellations and timeouts release the slot without making it reusable.
pub async fn finish(
    db: &Database,
    id: Uuid,
    run_id: Uuid,
    success: bool,
) -> Result<bool, StoreError> {
    let mut response = db.query("UPDATE coding_reservations SET status=$status,updated_at=time::now() WHERE record::id(id)=$id AND run_id=$run AND status='leased' AND compute_class='subscription' AND api_spend_cents=0 RETURN record::id(id) AS id")
        .bind(json!({"id":id.to_string(),"run":run_id.to_string(),"status":if success {"settled"} else {"released"}}))
        .await.map_err(StoreError::schema)?.check().map_err(StoreError::schema)?;
    let rows: Vec<Value> = response.take(0).map_err(StoreError::schema)?;
    Ok(!rows.is_empty())
}
