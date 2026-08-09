-- Cross-device unlock approval.
--
-- A locked desktop app requests approval to unlock; a trusted phone (another
-- active device of the same user) approves by signing a random nonce with its
-- device key — the same Ed25519 keys used for login. The desktop polls until
-- the request flips to 'approved'. No password ever crosses the wire.

create table unlock_requests (
    id                    uuid        primary key,
    user_id               uuid        not null references users (id) on delete cascade,
    requesting_device_id  uuid        not null references devices (id) on delete cascade,
    nonce                 bytea       not null,
    status                text        not null default 'pending', -- pending|approved|denied
    approved_by_device_id uuid        references devices (id) on delete set null,
    created_at            timestamptz not null default now(),
    expires_at            timestamptz not null,
    resolved_at           timestamptz
);

create index unlock_requests_user_status_idx on unlock_requests (user_id, status);
create index unlock_requests_requesting_device_idx on unlock_requests (requesting_device_id);
