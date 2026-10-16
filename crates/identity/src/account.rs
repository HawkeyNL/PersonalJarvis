//! Backend-only account state and one-use signed administration transactions.
use super::*;
use crate::password::{AccountPassword, PasswordService, StoredPassword};
use jarvis_client_core::account::{account_approval_message, AccountAction, AccountApproval};

#[derive(serde::Serialize)]
struct BootstrapBindings {
    #[serde(flatten)]
    fields: serde_json::Value,
    #[serde(with = "serde_bytes")]
    public_key: Vec<u8>,
}

/// The record id is bound from Rust: `type::thing` is gone in SurrealDB 3.x
/// and `type::record` takes different arguments in 2.6.5 and 3.x.
#[derive(serde::Serialize)]
struct ApprovalBindings {
    #[serde(flatten)]
    fields: serde_json::Value,
    password_record: surrealdb::RecordId,
}

/// Called only after the API verifies a locally provisioned activation code
/// and LAN scope. The one-use global latch and all first-account records are
/// committed together; revoking all devices never reopens bootstrap.
pub async fn bootstrap_account(
    db: &Database,
    name: &str,
    platform: Platform,
    public_key: &[u8],
    stored: StoredPassword,
    expires_at: i64,
) -> Result<(User, Device), IdentityError> {
    let key: [u8; 32] = public_key
        .try_into()
        .map_err(|_| IdentityError::AuthFailed)?;
    ed25519_dalek::VerifyingKey::from_bytes(&key).map_err(|_| IdentityError::AuthFailed)?;
    let existing = first_user(db).await?;
    let user = existing.as_ref().map_or_else(Uuid::now_v7, |u| u.id);
    let device = Uuid::now_v7();
    execute(db, "BEGIN TRANSACTION; \
        IF time::unix(time::now()) >= $expires { THROW 'activation expired'; }; \
        LET $active = SELECT id FROM devices WHERE status = 'active'; \
        IF array::len($active) != 0 { THROW 'activation unavailable'; }; \
        CREATE ONLY bootstrap_state:owner SET id = 'owner', claimed_at = time::now(), device_id = $device; \
        IF $new_user { CREATE users SET id = $user, display_name = 'Jarvis owner', status = 'active', \
            created_at = time::now(), updated_at = time::now(); }; \
        CREATE account_passwords SET user_id = $user, verifier = $verifier, revision = $revision, updated_at = time::now(); \
        CREATE devices SET id = $device, user_id = $user, name = $name, platform = $platform, status = 'active', \
            created_at = time::now(), updated_at = time::now(); \
        CREATE device_keys SET id = $key_id, device_id = $device, algorithm = 'ed25519', \
            public_key = <bytes>$public_key, created_at = time::now(), revoked_at = NONE; \
        COMMIT TRANSACTION;",
        BootstrapBindings { fields: json!({"expires":expires_at, "new_user":existing.is_none(), "user":user.to_string(), "device":device.to_string(),
            "verifier":stored.as_storage_str(), "revision":Uuid::now_v7().to_string(), "name":name,
            "platform":platform.as_str(), "key_id":Uuid::now_v7().to_string()}), public_key: public_key.to_vec() }).await?;
    Ok((
        get_user(db, user)
            .await?
            .ok_or(IdentityError::DatabaseSurreal)?,
        get_device(db, device)
            .await?
            .ok_or(IdentityError::DatabaseSurreal)?,
    ))
}

#[derive(serde::Deserialize)]
struct Credential {
    verifier: String,
    revision: String,
}

/// No trusted devices is not sufficient to reopen first-device activation:
/// the permanent latch survives revocation of the last device.
pub async fn bootstrap_available(db: &Database) -> Result<bool, IdentityError> {
    #[derive(serde::Deserialize)]
    struct Claim {
        claimed: bool,
    }
    let claimed: Option<Claim> = one(
        db,
        "SELECT true AS claimed FROM bootstrap_state:owner",
        json!({}),
    )
    .await?;
    if claimed.is_some_and(|claim| claim.claimed) {
        return Ok(false);
    }
    Ok(active_device_count(db).await? == 0)
}

pub async fn password_required(db: &Database, user_id: Uuid) -> Result<bool, IdentityError> {
    #[derive(serde::Deserialize)]
    struct Revision {
        revision: String,
    }
    let row: Option<Revision> = one(
        db,
        "SELECT revision FROM account_passwords WHERE user_id = $user LIMIT 1",
        json!({"user": user_id.to_string()}),
    )
    .await?;
    Ok(row.is_some_and(|r| !r.revision.is_empty()))
}

/// Returns the verified revision, not a login capability. The session minting
/// transaction must recheck this revision to avoid a password-change race.
pub async fn verify_password(
    db: &Database,
    service: &PasswordService,
    user_id: Uuid,
    password: Option<AccountPassword>,
) -> Result<Option<String>, IdentityError> {
    let row: Option<Credential> = one(
        db,
        "SELECT verifier, revision FROM account_passwords WHERE user_id = $user LIMIT 1",
        json!({"user": user_id.to_string()}),
    )
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let stored =
        StoredPassword::from_storage(row.verifier).map_err(|_| IdentityError::AuthFailed)?;
    service
        .verify(password.ok_or(IdentityError::AuthFailed)?, stored)
        .await
        .map_err(|error| match error {
            crate::password::PasswordError::Busy => IdentityError::Busy,
            crate::password::PasswordError::AuthenticationFailed => IdentityError::PasswordRejected,
            _ => IdentityError::AuthFailed,
        })?;
    Ok(Some(row.revision))
}

#[derive(serde::Deserialize)]
struct ActionRow {
    user_id: String,
    device_id: String,
    action: AccountAction,
    target: String,
    #[serde(with = "serde_bytes")]
    nonce: Vec<u8>,
    verifier: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
}

#[derive(serde::Serialize)]
struct ActionBindings {
    id: String,
    user: String,
    device: String,
    action: String,
    target: String,
    #[serde(with = "serde_bytes")]
    nonce: Vec<u8>,
    verifier: Option<String>,
}

/// Preparing a request never changes authorization. An active device must
/// subsequently sign the exact stored, expiring challenge.
pub async fn request_action(
    db: &Database,
    user_id: Uuid,
    device_id: Uuid,
    action: AccountAction,
    target: Uuid,
    verifier: Option<StoredPassword>,
) -> Result<AccountApproval, IdentityError> {
    let device = get_device(db, device_id)
        .await?
        .ok_or(IdentityError::AuthFailed)?;
    let sets_password = matches!(
        action,
        AccountAction::PasswordSet | AccountAction::PasswordReset
    );
    if device.user_id != user_id
        || device.status != "active"
        || (sets_password && (target != user_id || verifier.is_none()))
        || (action == AccountAction::DeviceRevoke && verifier.is_some())
        || (action == AccountAction::PasswordReset
            && !APPROVER_PLATFORMS.contains(&device.platform.as_str()))
    {
        return Err(IdentityError::AuthFailed);
    }
    if action == AccountAction::DeviceRevoke {
        let target = get_device(db, target)
            .await?
            .ok_or(IdentityError::AuthFailed)?;
        if target.user_id != user_id || target.status != "active" {
            return Err(IdentityError::AuthFailed);
        }
        ensure_other_approver(db, user_id, target.id).await?;
    }
    let id = Uuid::now_v7();
    let mut nonce = vec![0_u8; 32];
    rand::RngCore::try_fill_bytes(&mut rand::rngs::OsRng, &mut nonce)
        .map_err(|_| IdentityError::AuthFailed)?;
    // One live request per initiating device prevents unbounded pending secrets.
    execute(
        db,
        "BEGIN TRANSACTION; \
        DELETE account_actions WHERE expires_at <= time::now() OR consumed_at IS NOT NONE; \
        DELETE account_actions WHERE user_id = $user AND device_id = $device; \
        CREATE account_actions SET id = $id, user_id = $user, device_id = $device, \
        action = $action, target = $target, nonce = <bytes>$nonce, verifier = $verifier, \
        created_at = time::now(), expires_at = time::now() + 5m, consumed_at = NONE; \
        COMMIT TRANSACTION;",
        ActionBindings {
            id: id.to_string(),
            user: user_id.to_string(),
            device: device_id.to_string(),
            action: action.as_str().to_owned(),
            target: target.to_string(),
            nonce,
            verifier: verifier.map(|v| v.as_storage_str().to_owned()),
        },
    )
    .await?;
    let row = load_action(db, id, user_id, device_id).await?;
    approval(id, row)
}

async fn load_action(
    db: &Database,
    id: Uuid,
    user: Uuid,
    device: Uuid,
) -> Result<ActionRow, IdentityError> {
    one(
        db,
        "SELECT user_id, device_id, action, target, nonce, verifier, expires_at \
        FROM account_actions WHERE record::id(id) = $id AND user_id = $user \
        AND device_id = $device AND consumed_at IS NONE AND expires_at > time::now() LIMIT 1",
        json!({"id":id.to_string(), "user":user.to_string(), "device":device.to_string()}),
    )
    .await?
    .ok_or(IdentityError::AuthFailed)
}

fn approval(id: Uuid, row: ActionRow) -> Result<AccountApproval, IdentityError> {
    Ok(AccountApproval {
        request_id: id,
        user_id: row.user_id.parse().map_err(|_| IdentityError::AuthFailed)?,
        device_id: row
            .device_id
            .parse()
            .map_err(|_| IdentityError::AuthFailed)?,
        action: row.action,
        target: row.target.parse().map_err(|_| IdentityError::AuthFailed)?,
        nonce: hex::encode(row.nonce),
        expires_at: row.expires_at.unix_timestamp(),
    })
}

pub async fn approve_action(
    db: &Database,
    id: Uuid,
    user: Uuid,
    device: Uuid,
    signature: &[u8],
) -> Result<AccountApproval, IdentityError> {
    let row = load_action(db, id, user, device).await?;
    let action = row.action;
    if matches!(
        action,
        AccountAction::PasswordSet | AccountAction::PasswordReset
    ) {
        StoredPassword::from_storage(row.verifier.clone().ok_or(IdentityError::AuthFailed)?)
            .map_err(|_| IdentityError::AuthFailed)?;
    }
    let challenge = approval(id, row)?;
    let message = account_approval_message(&challenge).map_err(|_| IdentityError::AuthFailed)?;
    verify_device_signature(db, user, device, &message, signature).await?;
    let recovery = record_recovery();
    // A password change or a revoked signing device makes a pending recovery stale.
    let cancel_recovery = match action {
        AccountAction::PasswordSet => format!("{CANCEL_RECOVERIES} WHERE user_id = $user AND status = 'pending' RETURN NONE;"),
        AccountAction::DeviceRevoke => format!(
            "{CANCEL_RECOVERIES} WHERE user_id = $user AND device_id = $target AND status = 'pending' RETURN NONE;"
        ),
        AccountAction::PasswordReset => String::new(),
    };
    let mutation = match action {
        // Bootstrap stores the first verifier under a generated record id:
        // replace the owner's row wherever it lives (the owner index is UNIQUE).
        AccountAction::PasswordSet => "LET $current = SELECT id FROM account_passwords WHERE user_id = $user; \
            IF array::len($current) = 0 { \
                CREATE $password_record SET user_id = $user, verifier = $claim[0].verifier, \
                revision = $id, updated_at = time::now(); \
            } ELSE { \
                UPDATE account_passwords SET verifier = $claim[0].verifier, revision = $id, \
                updated_at = time::now() WHERE user_id = $user; \
            }; \
            UPDATE sessions SET revoked_at = time::now() WHERE user_id = $user AND revoked_at IS NONE; \
            DELETE account_actions WHERE user_id = $user;",
        // Concurrent revokes of two different approvers write disjoint device
        // rows, so snapshot isolation alone would let both pass the count
        // (write skew). Writing the owner row too makes them conflict.
        AccountAction::DeviceRevoke => "LET $owner = UPDATE users SET updated_at = time::now() \
            WHERE record::id(id) = $user RETURN AFTER; \
            IF array::len($owner) != 1 { THROW 'account action unavailable'; }; \
            LET $approvers = SELECT VALUE record::id(id) FROM devices WHERE user_id = $user \
            AND status = 'active' AND platform IN $approver_platforms; \
            IF $target IN $approvers AND array::len($approvers) < 2 { THROW 'last approver device'; }; \
            UPDATE devices SET status = 'revoked', updated_at = time::now() \
            WHERE record::id(id) = $target AND user_id = $user; \
            UPDATE device_keys SET revoked_at = time::now() WHERE device_id = $target AND revoked_at IS NONE; \
            UPDATE sessions SET revoked_at = time::now() WHERE device_id = $target AND user_id = $user AND revoked_at IS NONE;",
        // Signing only records the recovery; nothing changes until local root
        // confirms it (`confirm_recovery`).
        AccountAction::PasswordReset => &recovery,
    };
    let query = format!("BEGIN TRANSACTION; \
        LET $active = SELECT id FROM devices WHERE record::id(id) = $device AND user_id = $user AND status = 'active'; \
        IF array::len($active) != 1 {{ THROW 'account action unavailable'; }}; \
        LET $claim = UPDATE account_actions SET consumed_at = time::now() \
        WHERE record::id(id) = $id AND user_id = $user AND device_id = $device \
        AND action = $action AND target = $target AND consumed_at IS NONE AND expires_at > time::now() RETURN AFTER; \
        IF array::len($claim) != 1 {{ THROW 'account action unavailable'; }}; \
        {mutation} {cancel_recovery} DELETE account_actions WHERE record::id(id) = $id; COMMIT TRANSACTION;");
    let result = execute(
        db,
        &query,
        ApprovalBindings {
            fields: json!({"id":id.to_string(), "user":user.to_string(), "device":device.to_string(),
            "action":action.as_str(), "target":challenge.target.to_string(),
            "approver_platforms":APPROVER_PLATFORMS}),
            password_record: surrealdb::RecordId::from_table_key(
                "account_passwords",
                user.to_string(),
            ),
        },
    )
    .await;
    match result {
        Err(error) if action == AccountAction::DeviceRevoke => {
            Err(revoke_failure(db, id, user, device, challenge.target, error).await)
        }
        result => result.map(|()| challenge),
    }
}

/// Reports the guard, not an opaque failure, when it is what refused a signed
/// revoke (directly or by losing a race to a concurrent revoke). Only a claim
/// that is still live can have reached the guard: the transaction rolled back
/// and left it unconsumed. A replayed, expired or already consumed request
/// keeps its original error, so it is audited as a failure.
async fn revoke_failure(
    db: &Database,
    id: Uuid,
    user: Uuid,
    device: Uuid,
    target: Uuid,
    error: IdentityError,
) -> IdentityError {
    if load_action(db, id, user, device).await.is_ok()
        && matches!(
            ensure_other_approver(db, user, target).await,
            Err(IdentityError::LastApprover)
        )
    {
        return IdentityError::LastApprover;
    }
    error
}

/// Forgotten-password recovery limits. They are enforced inside the
/// transactions that record and confirm a recovery, not only by callers.
pub const RECOVERY_TTL_MINUTES: u32 = 15;
pub const RECOVERY_COOLDOWN_DAYS: u32 = 7;
pub const RECOVERY_REQUESTS_PER_DAY: u32 = 3;

/// `slot` is the owner id while pending and the record id afterwards: the
/// UNIQUE index then allows one pending recovery per owner even when two
/// approvals race. Resolving always drops the stored verifier.
const CANCEL_RECOVERIES: &str =
    "UPDATE account_recoveries SET status = 'expired', verifier = NONE, \
    slot = record::id(id), resolved_at = time::now()";

/// Run by every recovery operation, so an expired verifier does not linger.
const EXPIRE_RECOVERIES: &str = "UPDATE account_recoveries SET status = 'expired', verifier = NONE, \
    slot = record::id(id), resolved_at = time::now() WHERE status = 'pending' AND expires_at <= time::now() RETURN NONE;";

/// Throws unless the owner has a password, no recovery is pending, the daily
/// request limit is not reached and none completed within the cooldown.
fn recovery_guard() -> String {
    format!(
        "IF array::len(SELECT id FROM account_passwords WHERE user_id = $user) != 1 {{ THROW 'recovery unavailable'; }}; \
        IF array::len(SELECT id FROM account_recoveries WHERE user_id = $user AND status = 'pending' \
            AND expires_at > time::now()) != 0 {{ THROW 'recovery unavailable'; }}; \
        IF array::len(SELECT id FROM account_recoveries WHERE user_id = $user \
            AND created_at > time::now() - 1d) >= {RECOVERY_REQUESTS_PER_DAY} {{ THROW 'recovery unavailable'; }}; \
        IF array::len(SELECT id FROM account_recoveries WHERE user_id = $user AND status = 'confirmed' \
            AND resolved_at > time::now() - {RECOVERY_COOLDOWN_DAYS}d) != 0 {{ THROW 'recovery unavailable'; }};"
    )
}

fn record_recovery() -> String {
    format!(
        "{EXPIRE_RECOVERIES} DELETE account_recoveries WHERE created_at < time::now() - {}d; {} \
        CREATE account_recoveries SET id = $id, user_id = $user, device_id = $device, slot = $user, \
        verifier = $claim[0].verifier, status = 'pending', created_at = time::now(), \
        expires_at = time::now() + {RECOVERY_TTL_MINUTES}m, resolved_at = NONE RETURN NONE;",
        RECOVERY_COOLDOWN_DAYS + 1,
        recovery_guard()
    )
}

/// A cheap pre-check so a refused recovery costs no password hashing. The
/// signed approval and the root confirmation enforce the same limits again.
pub async fn recovery_available(db: &Database, user_id: Uuid) -> Result<(), IdentityError> {
    execute(
        db,
        &format!("{EXPIRE_RECOVERIES} {}", recovery_guard()),
        json!({"user": user_id.to_string()}),
    )
    .await
}

#[derive(serde::Deserialize)]
struct RecoveryRow {
    #[serde(with = "uuid::serde::hyphenated")]
    id: Uuid,
    #[serde(with = "uuid::serde::hyphenated")]
    device_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
}

/// What local root sees before confirming. Never the verifier or a nonce.
#[derive(Debug, serde::Serialize)]
pub struct PendingRecovery {
    pub id: Uuid,
    pub device_id: Uuid,
    pub device_name: String,
    pub platform: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}

/// Callers outside this crate must go through `local_devices`, which requires
/// a kernel-verified root peer.
pub(crate) async fn pending_recoveries(
    db: &Database,
    user_id: Uuid,
) -> Result<Vec<PendingRecovery>, IdentityError> {
    execute(db, EXPIRE_RECOVERIES, json!({})).await?;
    let rows: Vec<RecoveryRow> = many(
        db,
        "SELECT record::id(id) AS id, device_id, created_at, expires_at FROM account_recoveries \
        WHERE user_id = $user AND status = 'pending' AND expires_at > time::now() ORDER BY created_at",
        json!({"user": user_id.to_string()}),
    )
    .await?;
    let mut pending = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(device) = get_device(db, row.device_id).await? else {
            continue;
        };
        if device.user_id != user_id || device.status != "active" {
            continue;
        }
        pending.push(PendingRecovery {
            id: row.id,
            device_id: row.device_id,
            device_name: device.name,
            platform: device.platform,
            created_at: row.created_at,
            expires_at: row.expires_at,
        });
    }
    Ok(pending)
}

/// Sets the recovered password, ends every session and cancels pending
/// pairing, unlock and account requests in one transaction. Writing the
/// signing device's row makes a concurrent revocation conflict instead of
/// passing unnoticed. Refused when the
/// recovery expired, its device is no longer active, the password changed
/// after it was signed, or another recovery completed within the cooldown.
pub(crate) async fn confirm_recovery(
    db: &Database,
    user_id: Uuid,
    id: Uuid,
) -> Result<(), IdentityError> {
    execute(db, EXPIRE_RECOVERIES, json!({})).await?;
    execute(
        db,
        &format!(
            "BEGIN TRANSACTION; \
            IF array::len(SELECT id FROM account_recoveries WHERE user_id = $user AND status = 'confirmed' \
                AND resolved_at > time::now() - {RECOVERY_COOLDOWN_DAYS}d) != 0 {{ THROW 'recovery unavailable'; }}; \
            LET $claim = UPDATE account_recoveries SET status = 'confirmed', verifier = NONE, \
                slot = record::id(id), resolved_at = time::now() WHERE record::id(id) = $id AND user_id = $user \
                AND status = 'pending' AND expires_at > time::now() RETURN BEFORE; \
            IF array::len($claim) != 1 {{ THROW 'recovery unavailable'; }}; \
            LET $device = UPDATE devices SET updated_at = time::now() WHERE record::id(id) = $claim[0].device_id \
                AND user_id = $user AND status = 'active' RETURN VALUE record::id(id); \
            IF array::len($device) != 1 {{ THROW 'recovery unavailable'; }}; \
            IF array::len(SELECT id FROM account_passwords WHERE user_id = $user \
                AND updated_at > $claim[0].created_at) != 0 {{ THROW 'recovery unavailable'; }}; \
            LET $set = UPDATE account_passwords SET verifier = $claim[0].verifier, revision = $id, \
                updated_at = time::now() WHERE user_id = $user RETURN VALUE user_id; \
            IF array::len($set) != 1 {{ THROW 'recovery unavailable'; }}; \
            UPDATE sessions SET revoked_at = time::now() WHERE user_id = $user AND revoked_at IS NONE RETURN NONE; \
            UPDATE device_pairing_requests SET status = 'denied', resolved_at = time::now() \
                WHERE user_id = $user AND status = 'pending' RETURN NONE; \
            UPDATE unlock_requests SET status = 'denied', resolved_at = time::now() \
                WHERE user_id = $user AND status = 'pending' RETURN NONE; \
            DELETE account_actions WHERE user_id = $user; \
            COMMIT TRANSACTION;"
        ),
        json!({"id": id.to_string(), "user": user_id.to_string()}),
    )
    .await
}

pub(crate) async fn deny_recovery(
    db: &Database,
    user_id: Uuid,
    id: Uuid,
) -> Result<(), IdentityError> {
    execute(db, EXPIRE_RECOVERIES, json!({})).await?;
    let claimed: Option<ClaimedRecord> = one(
        db,
        "UPDATE account_recoveries SET status = 'denied', verifier = NONE, slot = record::id(id), \
        resolved_at = time::now() WHERE record::id(id) = $id AND user_id = $user AND status = 'pending' \
        RETURN record::id(id) AS id",
        json!({"id": id.to_string(), "user": user_id.to_string()}),
    )
    .await?;
    if claimed.map(|claim| claim.id) != Some(id.to_string()) {
        return Err(IdentityError::AuthFailed);
    }
    Ok(())
}

/// Signed approvals need an approver-platform device and bootstrap never
/// reopens, so the API refuses to revoke the user's last active one. The local
/// root path (`local_devices::revoke`) deliberately skips this: it is the
/// recovery route when no approver device is left or usable.
async fn ensure_other_approver(
    db: &Database,
    user: Uuid,
    target: Uuid,
) -> Result<(), IdentityError> {
    let approvers: Vec<Uuid> = list_active_devices(db, user)
        .await?
        .into_iter()
        .filter(|device| APPROVER_PLATFORMS.contains(&device.platform.as_str()))
        .map(|device| device.id)
        .collect();
    if approvers.contains(&target) && approvers.len() < 2 {
        return Err(IdentityError::LastApprover);
    }
    Ok(())
}

#[cfg(test)]
#[path = "account/tests.rs"]
mod tests;
