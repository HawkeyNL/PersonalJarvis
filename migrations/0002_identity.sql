-- Identity: the single Jarvis user and their trusted devices.
--
-- Device PRIVATE keys never leave the device (OS keychain). Only PUBLIC keys
-- are stored here, used later for device-bound sessions and approvals
-- (JAR-101 / JAR-104). No passwords or provider secrets live in this schema.

create table users (
    id           uuid        primary key,
    display_name text        not null,
    status       text        not null default 'active'
                             check (status in ('active', 'suspended')),
    created_at   timestamptz not null default now(),
    updated_at   timestamptz not null default now()
);

create table devices (
    id           uuid        primary key,
    user_id      uuid        not null references users (id) on delete cascade,
    name         text        not null,
    platform     text        not null
                             check (platform in ('macos', 'ios', 'windows', 'linux', 'android')),
    status       text        not null default 'active'
                             check (status in ('active', 'revoked')),
    created_at   timestamptz not null default now(),
    updated_at   timestamptz not null default now(),
    last_seen_at timestamptz
);

create index devices_user_id_idx on devices (user_id);

create table device_keys (
    id         uuid        primary key,
    device_id  uuid        not null references devices (id) on delete cascade,
    algorithm  text        not null default 'ed25519'
                           check (algorithm in ('ed25519')),
    public_key bytea       not null,
    created_at timestamptz not null default now(),
    revoked_at timestamptz,
    unique (device_id, public_key)
);

create index device_keys_device_id_idx on device_keys (device_id);
