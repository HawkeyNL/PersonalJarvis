-- LLM usage & cost tracking (ADR-027 stage 2). One row per metered API call;
-- the monthly SUM(cost_eur) feeds the hard budget the router enforces.
CREATE TABLE IF NOT EXISTS llm_usage (
    id                 BIGSERIAL PRIMARY KEY,
    ts                 TIMESTAMPTZ NOT NULL DEFAULT now(),
    backend            TEXT        NOT NULL,
    model              TEXT        NOT NULL,
    input_tokens       INTEGER     NOT NULL DEFAULT 0,
    output_tokens      INTEGER     NOT NULL DEFAULT 0,
    cache_read_tokens  INTEGER     NOT NULL DEFAULT 0,
    cache_write_tokens INTEGER     NOT NULL DEFAULT 0,
    cost_eur           DOUBLE PRECISION NOT NULL DEFAULT 0
);

-- The budget query filters on the current month, so index the timestamp.
CREATE INDEX IF NOT EXISTS llm_usage_ts_idx ON llm_usage (ts);
