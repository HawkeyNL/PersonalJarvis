//! Local host-owner authority is proved by the kernel, never by JSON, an HTTP
//! session, an environment variable, or a frontend Boolean approval flag.
use super::*;
use tokio::net::UnixStream;

/// Unforgeable outside this module; tied to the lifetime of the actual IPC
/// stream. Do not expose this proof through HTTP or serialize it.
pub struct LocalRootPeer<'a> {
    _stream: &'a UnixStream,
}

impl<'a> LocalRootPeer<'a> {
    pub fn authenticate(stream: &'a UnixStream) -> Result<Self, IdentityError> {
        if stream
            .peer_cred()
            .map_err(|_| IdentityError::AuthFailed)?
            .uid()
            != 0
        {
            return Err(IdentityError::AuthFailed);
        }
        Ok(Self { _stream: stream })
    }
}

pub async fn approve(
    db: &Database,
    _peer: &LocalRootPeer<'_>,
    request_id: Uuid,
    fingerprint: &str,
) -> Result<(Device, Vec<u8>), IdentityError> {
    let owner = first_user(db).await?.ok_or(IdentityError::AuthFailed)?;
    let request = pairing_request(db, request_id, owner.id)
        .await?
        .ok_or(IdentityError::AuthFailed)?;
    if request.status != "pending"
        || request.expires_at <= OffsetDateTime::now_utc()
        || request.candidate_fingerprint != fingerprint
    {
        return Err(IdentityError::AuthFailed);
    }
    // The root CLI pins exactly this key after checking it against the
    // owner-supplied fingerprint.
    let public_key = request.candidate_public_key.clone();
    let device = activate_pairing_request(db, request, None).await?;
    Ok((device, public_key))
}

pub async fn revoke(
    db: &Database,
    _peer: &LocalRootPeer<'_>,
    device_id: Uuid,
) -> Result<User, IdentityError> {
    let owner = first_user(db).await?.ok_or(IdentityError::AuthFailed)?;
    let device = get_device(db, device_id)
        .await?
        .ok_or(IdentityError::AuthFailed)?;
    if device.user_id != owner.id || device.status != "active" {
        return Err(IdentityError::AuthFailed);
    }
    revoke_device(db, device_id).await?;
    Ok(owner)
}

pub async fn deny(
    db: &Database,
    _peer: &LocalRootPeer<'_>,
    request_id: Uuid,
) -> Result<(), IdentityError> {
    let owner = first_user(db).await?.ok_or(IdentityError::AuthFailed)?;
    let claimed: Option<ClaimedRecord> = one(db,
        "UPDATE device_pairing_requests SET status = 'denied', approved_by_device_id = NONE, resolved_at = time::now() WHERE record::id(id) = $id AND user_id = $user AND status = 'pending' AND expires_at > time::now() RETURN record::id(id) AS id",
        json!({"id": request_id.to_string(), "user": owner.id.to_string()})).await?;
    if claimed.map(|claim| claim.id) != Some(request_id.to_string()) {
        return Err(IdentityError::AuthFailed);
    }
    Ok(())
}

pub async fn pending_recoveries(
    db: &Database,
    _peer: &LocalRootPeer<'_>,
) -> Result<Vec<account::PendingRecovery>, IdentityError> {
    let owner = first_user(db).await?.ok_or(IdentityError::AuthFailed)?;
    account::pending_recoveries(db, owner.id).await
}

/// The local root half of a forgotten-password recovery; the device-signed
/// half is `account::approve_action` with `AccountAction::PasswordReset`.
pub async fn confirm_recovery(
    db: &Database,
    _peer: &LocalRootPeer<'_>,
    request_id: Uuid,
) -> Result<User, IdentityError> {
    let owner = first_user(db).await?.ok_or(IdentityError::AuthFailed)?;
    account::confirm_recovery(db, owner.id, request_id).await?;
    Ok(owner)
}

pub async fn deny_recovery(
    db: &Database,
    _peer: &LocalRootPeer<'_>,
    request_id: Uuid,
) -> Result<(), IdentityError> {
    let owner = first_user(db).await?.ok_or(IdentityError::AuthFailed)?;
    account::deny_recovery(db, owner.id, request_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn authority_comes_from_kernel_peer_not_request_fields() {
        let (a, b) = UnixStream::pair().unwrap();
        let uid = a.peer_cred().unwrap().uid();
        assert_eq!(LocalRootPeer::authenticate(&a).is_ok(), uid == 0);
        assert_eq!(LocalRootPeer::authenticate(&b).is_ok(), uid == 0);
    }
}
