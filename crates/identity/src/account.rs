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
        IF time::now() >= time::from::unix($expires) { THROW 'activation expired'; }; \
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
        .map_err(|_| IdentityError::AuthFailed)?;
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
    if device.user_id != user_id
        || device.status != "active"
        || (action == AccountAction::PasswordSet && (target != user_id || verifier.is_none()))
        || (action == AccountAction::DeviceRevoke && verifier.is_some())
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
    if action == AccountAction::PasswordSet {
        StoredPassword::from_storage(row.verifier.clone().ok_or(IdentityError::AuthFailed)?)
            .map_err(|_| IdentityError::AuthFailed)?;
    }
    let challenge = approval(id, row)?;
    let message = account_approval_message(&challenge).map_err(|_| IdentityError::AuthFailed)?;
    verify_device_signature(db, user, device, &message, signature).await?;
    let mutation = match action {
        AccountAction::PasswordSet => "UPSERT type::thing('account_passwords', $user) SET user_id = $user, \
            verifier = $claim[0].verifier, revision = $id, updated_at = time::now(); \
            UPDATE sessions SET revoked_at = time::now() WHERE user_id = $user AND revoked_at IS NONE; \
            DELETE account_actions WHERE user_id = $user;",
        AccountAction::DeviceRevoke => "UPDATE devices SET status = 'revoked', updated_at = time::now() \
            WHERE record::id(id) = $target AND user_id = $user; \
            UPDATE device_keys SET revoked_at = time::now() WHERE device_id = $target AND revoked_at IS NONE; \
            UPDATE sessions SET revoked_at = time::now() WHERE device_id = $target AND user_id = $user AND revoked_at IS NONE;",
    };
    let query = format!("BEGIN TRANSACTION; \
        LET $active = SELECT id FROM devices WHERE record::id(id) = $device AND user_id = $user AND status = 'active'; \
        IF array::len($active) != 1 {{ THROW 'account action unavailable'; }}; \
        LET $claim = UPDATE account_actions SET consumed_at = time::now() \
        WHERE record::id(id) = $id AND user_id = $user AND device_id = $device \
        AND action = $action AND target = $target AND consumed_at IS NONE AND expires_at > time::now() RETURN AFTER; \
        IF array::len($claim) != 1 {{ THROW 'account action unavailable'; }}; \
        {mutation} DELETE account_actions WHERE record::id(id) = $id; COMMIT TRANSACTION;");
    execute(
        db,
        &query,
        json!({"id":id.to_string(), "user":user.to_string(), "device":device.to_string(),
        "action":action.as_str(), "target":challenge.target.to_string()}),
    )
    .await?;
    Ok(challenge)
}

#[cfg(test)]
#[path = "account/tests.rs"]
mod tests;
