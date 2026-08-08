-- Device-bound authentication: challenge-response login and sessions.
--
-- Login flow: the server issues a random nonce (auth_challenges); the device
-- signs it with its private key; the server verifies the signature against the
-- device's registered public key and issues a session token. Only the SHA-256
-- HASH of the token is stored, never the token itself.

create table auth_challenges (
    id          uuid        primary key,
    device_id   uuid        not null references devices (id) on delete cascade,
    nonce       bytea       not null,
    created_at  timestamptz not null default now(),
    expires_at  timestamptz not null,
    consumed_at timestamptz
);

create index auth_challenges_device_id_idx on auth_challenges (device_id);

create table sessions (
    id           uuid        primary key,
    user_id      uuid        not null references users (id) on delete cascade,
    device_id    uuid        not null references devices (id) on delete cascade,
    token_hash   bytea       not null unique,
    created_at   timestamptz not null default now(),
    expires_at   timestamptz not null,
    last_used_at timestamptz,
    revoked_at   timestamptz
);

create index sessions_user_id_idx on sessions (user_id);
create index sessions_device_id_idx on sessions (device_id);
