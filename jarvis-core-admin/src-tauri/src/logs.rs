use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct LogRecord {
    pub id: usize,
    pub timestamp: Option<String>,
    pub level: String,
    pub message: String,
    pub target: Option<String>,
    pub details: Vec<(String, String)>,
}

pub fn parse_lines(lines: &[String]) -> Vec<LogRecord> {
    lines
        .iter()
        .enumerate()
        .map(|(id, line)| parse_line(id, line))
        .collect()
}

fn parse_line(id: usize, line: &str) -> LogRecord {
    // Preserve JSON syntax while parsing, then retain and sanitize only the
    // explicit non-secret projection below. Plain text is redacted as a whole.
    let controlled = strip_controls(line, 16_384);
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&controlled) {
        if let Some(record) = structured(id, &value, None, None) {
            return record;
        }
    }

    if let Some((timestamp, remainder)) = journal_prefix(&controlled) {
        let (source, message) = remainder.split_once(": ").unwrap_or(("", remainder));
        let target = source
            .split_whitespace()
            .next_back()
            .and_then(|value| value.split('[').next())
            .filter(|value| !value.is_empty())
            .map(|value| sanitize(value, 128));
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(message) {
            if let Some(record) = structured(id, &value, Some(timestamp.clone()), target.clone()) {
                return record;
            }
        }
        let system = target.as_deref() == Some("systemd");
        return LogRecord {
            id,
            timestamp: Some(timestamp),
            level: if system { "SYSTEM" } else { "INFO" }.to_owned(),
            message: sanitize(message, 16_384),
            target,
            details: Vec::new(),
        };
    }

    LogRecord {
        id,
        timestamp: None,
        level: "INFO".to_owned(),
        message: sanitize(&controlled, 16_384),
        target: None,
        details: Vec::new(),
    }
}

fn structured(
    id: usize,
    value: &serde_json::Value,
    fallback_timestamp: Option<String>,
    fallback_target: Option<String>,
) -> Option<LogRecord> {
    let object = value.as_object()?;
    let message = object
        .get("message")
        .or_else(|| object.get("MESSAGE"))?
        .as_str()?;
    let timestamp = fallback_timestamp.or_else(|| {
        object
            .get("timestamp")
            .or_else(|| object.get("time"))
            .or_else(|| object.get("@timestamp"))
            .and_then(serde_json::Value::as_str)
            .and_then(compact_timestamp)
    });
    let target = object
        .get("target")
        .or_else(|| object.get("SYSLOG_IDENTIFIER"))
        .or_else(|| object.get("_SYSTEMD_UNIT"))
        .and_then(serde_json::Value::as_str)
        .map(|value| sanitize(value, 128))
        .or(fallback_target);
    if let Ok(nested) = serde_json::from_str::<serde_json::Value>(message) {
        if let Some(record) = structured(id, &nested, timestamp.clone(), target.clone()) {
            return Some(record);
        }
    }
    let system = target
        .as_deref()
        .is_some_and(|value| value.starts_with("systemd"));
    let level = if system {
        "SYSTEM".to_owned()
    } else {
        compact_level(
            object
                .get("level")
                .or_else(|| object.get("LEVEL"))
                .or_else(|| object.get("PRIORITY"))
                .and_then(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .or_else(|| value.as_u64().map(|v| v.to_string()))
                })
                .as_deref()
                .unwrap_or("INFO"),
        )
    };
    let details = ["module", "file", "line", "span", "thread"]
        .into_iter()
        .filter_map(|key| {
            object.get(key).and_then(|value| match value {
                serde_json::Value::String(value) => Some((key.to_owned(), sanitize(value, 512))),
                serde_json::Value::Number(value) => Some((key.to_owned(), value.to_string())),
                _ => None,
            })
        })
        .collect();
    Some(LogRecord {
        id,
        timestamp,
        level,
        message: sanitize(message, 16_384),
        target,
        details,
    })
}

fn compact_level(level: &str) -> String {
    match level.trim().to_ascii_uppercase().as_str() {
        "0" | "1" | "2" | "3" | "ERROR" | "ERR" | "CRITICAL" | "CRIT" => "ERROR",
        "4" | "WARN" | "WARNING" => "WARN",
        "7" | "TRACE" | "DEBUG" => "DEBUG",
        "SYSTEM" => "SYSTEM",
        _ => "INFO",
    }
    .to_owned()
}

fn journal_prefix(line: &str) -> Option<(String, &str)> {
    let (timestamp, remainder) = line.split_once(' ')?;
    Some((compact_timestamp(timestamp)?, remainder.trim_start()))
}

fn compact_timestamp(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    (0..bytes.len().saturating_sub(7)).find_map(|start| {
        let candidate = bytes.get(start..start + 8)?;
        (candidate[0].is_ascii_digit()
            && candidate[1].is_ascii_digit()
            && candidate[2] == b':'
            && candidate[3].is_ascii_digit()
            && candidate[4].is_ascii_digit()
            && candidate[5] == b':'
            && candidate[6].is_ascii_digit()
            && candidate[7].is_ascii_digit())
        .then(|| String::from_utf8_lossy(candidate).into_owned())
    })
}

pub fn sanitize(value: &str, limit: usize) -> String {
    let text = strip_controls(value, limit);
    let lower = text.to_ascii_lowercase();
    if [
        "api_key=",
        "api_key:",
        "\"api_key\"",
        "authorization:",
        "bearer ",
        "token=",
        "\"token\"",
        "password=",
        "\"password\"",
        "secret=",
        "\"secret\"",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        "[potentially secret-bearing log content omitted]".to_owned()
    } else {
        text
    }
}

fn strip_controls(value: &str, limit: usize) -> String {
    value
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        .take(limit)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_safe_json_and_discards_secret_fields() {
        let records = parse_lines(&[r#"{"timestamp":"2026-08-30T12:08:11Z","level":"warn","message":"pricing unavailable","target":"jarvis_usage","api_key":"never"}"#.to_owned()]);
        assert_eq!(records[0].timestamp.as_deref(), Some("12:08:11"));
        assert_eq!(records[0].level, "WARN");
        assert_eq!(records[0].message, "pricing unavailable");
        assert!(!format!("{:?}", records[0]).contains("never"));
    }

    #[test]
    fn strips_control_sequences_and_redacts_secret_assignments() {
        assert_eq!(sanitize("safe\u{1b}[31m", 100), "safe[31m");
        assert_eq!(
            sanitize("failed token=never", 100),
            "[potentially secret-bearing log content omitted]"
        );
    }
}
