-- Jarvis Core baseline for an empty pre-production SurrealDB instance.
-- Tables are closed by default: Core is the only database caller, and all
-- externally supplied access remains behind Core's device-bound auth layer.
BEGIN TRANSACTION;

DEFINE TABLE IF NOT EXISTS schema_version SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD IF NOT EXISTS version ON schema_version TYPE int;
DEFINE FIELD IF NOT EXISTS applied_at ON schema_version TYPE datetime;
DEFINE INDEX IF NOT EXISTS schema_version_unique ON schema_version FIELDS version UNIQUE;

DEFINE TABLE IF NOT EXISTS users SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON users TYPE string;
DEFINE FIELD display_name ON users TYPE string;
DEFINE FIELD status ON users TYPE string ASSERT $value IN ['active', 'suspended'];
DEFINE FIELD created_at ON users TYPE datetime;
DEFINE FIELD updated_at ON users TYPE datetime;
DEFINE INDEX users_id_unique ON users FIELDS id UNIQUE;

DEFINE TABLE devices SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON devices TYPE string;
DEFINE FIELD user_id ON devices TYPE string;
DEFINE FIELD name ON devices TYPE string;
DEFINE FIELD platform ON devices TYPE string ASSERT $value IN ['macos', 'ios', 'windows', 'linux', 'android'];
DEFINE FIELD status ON devices TYPE string ASSERT $value IN ['active', 'revoked'];
DEFINE FIELD created_at ON devices TYPE datetime;
DEFINE FIELD updated_at ON devices TYPE datetime;
DEFINE FIELD last_seen_at ON devices TYPE option<datetime>;
DEFINE INDEX devices_id_unique ON devices FIELDS id UNIQUE;
DEFINE INDEX devices_user_idx ON devices FIELDS user_id;

DEFINE TABLE device_keys SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON device_keys TYPE string;
DEFINE FIELD device_id ON device_keys TYPE string;
DEFINE FIELD algorithm ON device_keys TYPE string ASSERT $value = 'ed25519';
DEFINE FIELD public_key ON device_keys TYPE bytes;
DEFINE FIELD created_at ON device_keys TYPE datetime;
DEFINE FIELD revoked_at ON device_keys TYPE option<datetime>;
DEFINE INDEX device_keys_id_unique ON device_keys FIELDS id UNIQUE;
DEFINE INDEX device_keys_device_key_unique ON device_keys FIELDS device_id, public_key UNIQUE;

DEFINE TABLE auth_challenges SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON auth_challenges TYPE string;
DEFINE FIELD device_id ON auth_challenges TYPE string;
DEFINE FIELD nonce ON auth_challenges TYPE bytes;
DEFINE FIELD created_at ON auth_challenges TYPE datetime;
DEFINE FIELD expires_at ON auth_challenges TYPE datetime;
DEFINE FIELD consumed_at ON auth_challenges TYPE option<datetime>;
DEFINE INDEX auth_challenges_id_unique ON auth_challenges FIELDS id UNIQUE;
DEFINE INDEX auth_challenges_device_idx ON auth_challenges FIELDS device_id;

DEFINE TABLE sessions SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON sessions TYPE string;
DEFINE FIELD user_id ON sessions TYPE string;
DEFINE FIELD device_id ON sessions TYPE string;
DEFINE FIELD token_hash ON sessions TYPE bytes;
DEFINE FIELD created_at ON sessions TYPE datetime;
DEFINE FIELD expires_at ON sessions TYPE datetime;
DEFINE FIELD last_used_at ON sessions TYPE option<datetime>;
DEFINE FIELD revoked_at ON sessions TYPE option<datetime>;
DEFINE INDEX sessions_id_unique ON sessions FIELDS id UNIQUE;
DEFINE INDEX sessions_token_hash_unique ON sessions FIELDS token_hash UNIQUE;
DEFINE INDEX sessions_user_idx ON sessions FIELDS user_id;
DEFINE INDEX sessions_device_idx ON sessions FIELDS device_id;

DEFINE TABLE holdings SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON holdings TYPE string;
DEFINE FIELD user_id ON holdings TYPE string;
DEFINE FIELD symbol ON holdings TYPE string;
-- Decimal values are strings, never floating point.  Domain code parses them
-- with rust_decimal before arithmetic.
DEFINE FIELD quantity ON holdings TYPE string;
DEFINE FIELD avg_cost ON holdings TYPE string;
DEFINE FIELD currency ON holdings TYPE string;
DEFINE FIELD created_at ON holdings TYPE datetime;
DEFINE FIELD updated_at ON holdings TYPE datetime;
DEFINE INDEX holdings_id_unique ON holdings FIELDS id UNIQUE;
DEFINE INDEX holdings_user_idx ON holdings FIELDS user_id;

DEFINE TABLE unlock_requests SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON unlock_requests TYPE string;
DEFINE FIELD user_id ON unlock_requests TYPE string;
DEFINE FIELD requesting_device_id ON unlock_requests TYPE string;
DEFINE FIELD nonce ON unlock_requests TYPE bytes;
DEFINE FIELD status ON unlock_requests TYPE string ASSERT $value IN ['pending', 'approved', 'denied'];
DEFINE FIELD approved_by_device_id ON unlock_requests TYPE option<string>;
DEFINE FIELD created_at ON unlock_requests TYPE datetime;
DEFINE FIELD expires_at ON unlock_requests TYPE datetime;
DEFINE FIELD resolved_at ON unlock_requests TYPE option<datetime>;
DEFINE INDEX unlock_requests_id_unique ON unlock_requests FIELDS id UNIQUE;
DEFINE INDEX unlock_requests_user_status_idx ON unlock_requests FIELDS user_id, status;

DEFINE TABLE voice_profiles SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD user_id ON voice_profiles TYPE string;
DEFINE FIELD embedding ON voice_profiles TYPE bytes;
DEFINE FIELD dims ON voice_profiles TYPE int;
DEFINE FIELD engine ON voice_profiles TYPE string;
DEFINE FIELD created_at ON voice_profiles TYPE datetime;
DEFINE FIELD updated_at ON voice_profiles TYPE datetime;
DEFINE INDEX voice_profiles_user_unique ON voice_profiles FIELDS user_id UNIQUE;

DEFINE TABLE llm_usage SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON llm_usage TYPE string;
DEFINE FIELD ts ON llm_usage TYPE datetime;
DEFINE FIELD backend ON llm_usage TYPE string;
DEFINE FIELD model ON llm_usage TYPE string;
DEFINE FIELD input_tokens ON llm_usage TYPE int;
DEFINE FIELD output_tokens ON llm_usage TYPE int;
DEFINE FIELD cache_read_tokens ON llm_usage TYPE int;
DEFINE FIELD cache_write_tokens ON llm_usage TYPE int;
DEFINE FIELD cost_eur ON llm_usage TYPE float;
DEFINE INDEX llm_usage_id_unique ON llm_usage FIELDS id UNIQUE;
DEFINE INDEX llm_usage_ts_idx ON llm_usage FIELDS ts;

DEFINE TABLE agent_audit SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON agent_audit TYPE string;
DEFINE FIELD ts ON agent_audit TYPE datetime;
DEFINE FIELD device_id ON agent_audit TYPE option<string>;
DEFINE FIELD action_type ON agent_audit TYPE string;
DEFINE FIELD detail ON agent_audit TYPE option<string>;
DEFINE FIELD risk_class ON agent_audit TYPE string;
DEFINE FIELD outcome ON agent_audit TYPE string;
DEFINE FIELD note ON agent_audit TYPE option<string>;
DEFINE INDEX agent_audit_id_unique ON agent_audit FIELDS id UNIQUE;
DEFINE INDEX agent_audit_ts_idx ON agent_audit FIELDS ts;

DEFINE TABLE agent_pending_actions SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON agent_pending_actions TYPE string;
DEFINE FIELD user_id ON agent_pending_actions TYPE string;
DEFINE FIELD requesting_device_id ON agent_pending_actions TYPE string;
DEFINE FIELD action_type ON agent_pending_actions TYPE string;
DEFINE FIELD action ON agent_pending_actions TYPE string;
DEFINE FIELD preview ON agent_pending_actions TYPE string;
DEFINE FIELD nonce ON agent_pending_actions TYPE bytes;
DEFINE FIELD status ON agent_pending_actions TYPE string ASSERT $value IN ['pending', 'approved', 'denied', 'executed', 'expired'];
DEFINE FIELD created_at ON agent_pending_actions TYPE datetime;
DEFINE FIELD expires_at ON agent_pending_actions TYPE datetime;
DEFINE FIELD resolved_at ON agent_pending_actions TYPE option<datetime>;
DEFINE FIELD approved_by_device_id ON agent_pending_actions TYPE option<string>;
DEFINE INDEX agent_pending_actions_id_unique ON agent_pending_actions FIELDS id UNIQUE;
DEFINE INDEX agent_pending_actions_user_status_idx ON agent_pending_actions FIELDS user_id, status;

DEFINE TABLE conversations SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON conversations TYPE string;
DEFINE FIELD user_id ON conversations TYPE string;
DEFINE FIELD title ON conversations TYPE string;
DEFINE FIELD created_at ON conversations TYPE datetime;
DEFINE FIELD updated_at ON conversations TYPE datetime;
DEFINE INDEX conversations_id_unique ON conversations FIELDS id UNIQUE;
DEFINE INDEX conversations_user_updated_idx ON conversations FIELDS user_id, updated_at;

DEFINE TABLE chat_messages SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON chat_messages TYPE string;
DEFINE FIELD conversation_id ON chat_messages TYPE string;
DEFINE FIELD user_id ON chat_messages TYPE string;
DEFINE FIELD role ON chat_messages TYPE string ASSERT $value IN ['user', 'assistant'];
DEFINE FIELD content ON chat_messages TYPE string;
DEFINE FIELD model ON chat_messages TYPE option<string>;
DEFINE FIELD created_at ON chat_messages TYPE datetime;
DEFINE INDEX chat_messages_id_unique ON chat_messages FIELDS id UNIQUE;
DEFINE INDEX chat_messages_conversation_created_idx ON chat_messages FIELDS conversation_id, created_at;

DEFINE TABLE security_audit SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON security_audit TYPE string;
DEFINE FIELD ts ON security_audit TYPE datetime;
DEFINE FIELD device_id ON security_audit TYPE option<string>;
DEFINE FIELD event ON security_audit TYPE string;
DEFINE FIELD outcome ON security_audit TYPE string;
DEFINE FIELD detail ON security_audit TYPE option<string>;
DEFINE INDEX security_audit_id_unique ON security_audit FIELDS id UNIQUE;
DEFINE INDEX security_audit_ts_idx ON security_audit FIELDS ts;

UPSERT schema_version:baseline SET version = 1, applied_at = time::now();
COMMIT TRANSACTION;
