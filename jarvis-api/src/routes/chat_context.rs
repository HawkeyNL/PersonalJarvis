//! Canonical context for asynchronous runs. Client replicas are not history authority.
use jarvis_client_core::realtime::CanonicalMessage;
use jarvis_llm::ChatMessage;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

const MAX_CONTEXT_ROWS: usize = 32;
const MAX_CONTEXT_BYTES: usize = 128_000;

#[derive(Deserialize)]
struct Row {
    id: String,
    role: String,
    content: String,
}

pub(super) async fn load(
    db: &jarvis_store::Database,
    user: Uuid,
    newest: &CanonicalMessage,
) -> Result<Vec<ChatMessage>, ()> {
    let mut response = db.query(
        "SELECT record::id(id) AS id, role, content, created_at FROM chat_messages \
         WHERE conversation_id = $conversation AND user_id = $user \
         ORDER BY created_at DESC, id DESC LIMIT $limit"
    ).bind(json!({"conversation": newest.conversation_id.to_string(), "user": user.to_string(), "limit": MAX_CONTEXT_ROWS}))
        .await.map_err(|_| ())?;
    let rows: Vec<Row> = response.take(0).map_err(|_| ())?;
    bounded(rows, newest)
}

fn bounded(rows: Vec<Row>, newest: &CanonicalMessage) -> Result<Vec<ChatMessage>, ()> {
    // The already committed user message must be the newest row. Do not call
    // the model on missing/stale persistence or accidentally append it twice.
    let first = rows.first().ok_or(())?;
    if first.id != newest.id.to_string() || first.role != "user" || first.content != newest.content
    {
        return Err(());
    }
    let mut messages = Vec::new();
    let mut bytes = 0usize;
    for row in rows.into_iter().take(MAX_CONTEXT_ROWS) {
        if !matches!(row.role.as_str(), "user" | "assistant") {
            return Err(());
        }
        if row.content.len() > MAX_CONTEXT_BYTES.saturating_sub(bytes) {
            break;
        }
        bytes += row.content.len();
        messages.push(if row.role == "assistant" {
            ChatMessage::assistant(row.content)
        } else {
            ChatMessage::user(row.content)
        });
    }
    if messages.is_empty() {
        return Err(());
    }
    messages.reverse();
    Ok(messages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jarvis_client_core::realtime::MessageRole;
    fn newest() -> CanonicalMessage {
        CanonicalMessage {
            id: Uuid::nil(),
            conversation_id: Uuid::nil(),
            role: MessageRole::User,
            content: "Next question".into(),
            model: None,
            created_at: time::OffsetDateTime::UNIX_EPOCH,
        }
    }
    fn row(role: &str, content: &str) -> Row {
        Row {
            id: Uuid::nil().to_string(),
            role: role.into(),
            content: content.into(),
        }
    }
    #[test]
    fn context_uses_stored_turns_in_order_and_new_question_once() {
        let messages = bounded(
            vec![
                row("user", "Next question"),
                row("assistant", "Stored answer"),
                row("user", "Earlier question"),
            ],
            &newest(),
        )
        .unwrap();
        assert_eq!(
            messages
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>(),
            vec!["Earlier question", "Stored answer", "Next question"]
        );
    }
    #[test]
    fn context_is_bounded_without_truncating_message_content() {
        let messages = bounded(
            vec![
                row("user", "Next question"),
                row("assistant", &"x".repeat(MAX_CONTEXT_BYTES)),
            ],
            &newest(),
        )
        .unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "Next question");
    }
    #[test]
    fn missing_new_message_or_internal_role_fails_closed() {
        assert!(bounded(vec![], &newest()).is_err());
        assert!(bounded(vec![row("user", "Wrong question")], &newest()).is_err());
        assert!(bounded(
            vec![
                row("user", "Next question"),
                row("system", "Not conversation history")
            ],
            &newest()
        )
        .is_err());
    }
}
