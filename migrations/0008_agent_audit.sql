-- Agentic execution audit log (ADR-029). Append-only: one row per attempted
-- action, whether it ran, was denied, or errored. The owner can read back
-- everything Jarvis' hands ever did.
CREATE TABLE IF NOT EXISTS agent_audit (
    id          BIGSERIAL PRIMARY KEY,
    ts          TIMESTAMPTZ NOT NULL DEFAULT now(),
    device_id   UUID,
    action_type TEXT NOT NULL,
    detail      TEXT,
    risk_class  TEXT NOT NULL,          -- auto | needs_approval | denied
    outcome     TEXT NOT NULL,          -- ok | denied | error
    note        TEXT
);

CREATE INDEX IF NOT EXISTS agent_audit_ts_idx ON agent_audit (ts);
