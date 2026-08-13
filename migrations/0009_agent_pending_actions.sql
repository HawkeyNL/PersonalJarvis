-- Mutating agent actions awaiting a device-signed approval (ADR-029 phase 4b).
-- The action is stored server-side; the owner signs the nonce on a trusted
-- device to approve exactly this pending action, which is then executed once.
CREATE TABLE IF NOT EXISTS agent_pending_actions (
    id                   UUID        PRIMARY KEY,
    user_id              UUID        NOT NULL,
    requesting_device_id UUID        NOT NULL,
    action_type          TEXT        NOT NULL,
    action               TEXT        NOT NULL,  -- JSON-serialized agent::Action
    preview              TEXT        NOT NULL,
    nonce                BYTEA       NOT NULL,
    status               TEXT        NOT NULL DEFAULT 'pending'
                                     CHECK (status IN ('pending','approved','denied','executed','expired')),
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at           TIMESTAMPTZ NOT NULL,
    resolved_at          TIMESTAMPTZ,
    approved_by_device_id UUID
);

CREATE INDEX IF NOT EXISTS agent_pending_user_idx ON agent_pending_actions (user_id, status);
