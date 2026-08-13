-- Persistent, auto-categorized chat history (ADR-030). Jarvis stores every
-- conversation server-side so the chat survives an app restart and follows the
-- owner across devices; a cheap classifier splits a new topic into its own
-- conversation ("tab").
CREATE TABLE IF NOT EXISTS conversations (
    id         UUID        PRIMARY KEY,
    user_id    UUID        NOT NULL,
    title      TEXT        NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Newest-active-first listing per user.
CREATE INDEX IF NOT EXISTS conversations_user_idx
    ON conversations (user_id, updated_at DESC);

CREATE TABLE IF NOT EXISTS chat_messages (
    id              UUID        PRIMARY KEY,
    conversation_id UUID        NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
    user_id         UUID        NOT NULL,
    role            TEXT        NOT NULL CHECK (role IN ('user', 'assistant')),
    content         TEXT        NOT NULL,
    model           TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Fetch a thread in order.
CREATE INDEX IF NOT EXISTS chat_messages_conv_idx
    ON chat_messages (conversation_id, created_at);
