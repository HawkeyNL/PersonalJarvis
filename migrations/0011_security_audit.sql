-- Security/auth audit trail (Priority 6 of the hardening brief). Append-only:
-- one row per security-relevant event (login, enrolment, logout, device and
-- unlock changes). Never stores secrets — only the actor device, the event,
-- the outcome, and a short non-sensitive detail.
CREATE TABLE IF NOT EXISTS security_audit (
    id         BIGSERIAL PRIMARY KEY,
    ts         TIMESTAMPTZ NOT NULL DEFAULT now(),
    device_id  UUID,
    event      TEXT NOT NULL,   -- auth.login | auth.enroll | auth.logout | device.revoke | unlock.approve | unlock.deny
    outcome    TEXT NOT NULL,   -- ok | fail
    detail     TEXT
);

CREATE INDEX IF NOT EXISTS security_audit_ts_idx ON security_audit (ts);
