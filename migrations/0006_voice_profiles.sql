-- Server-side speaker verification: one voice embedding per user.
--
-- The profile lives here (the server is the source of truth), so it is inherently
-- shared across all of that user's devices — enroll once, verify anywhere. The
-- embedding is a fixed-length float vector stored as little-endian f32 bytes.

create table voice_profiles (
    user_id    uuid        primary key references users (id) on delete cascade,
    embedding  bytea       not null,
    dims       integer     not null,
    engine     text        not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);
