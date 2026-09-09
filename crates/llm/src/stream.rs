use crate::{LlmError, Usage};
use serde_json::Value;
use std::sync::Arc;

/// Synchronous, nonblocking presentation sink; never a second inference.
pub type TextDeltaSink = Arc<dyn Fn(&str) + Send + Sync>;

const MAX_LINE: usize = 64 * 1024;
const MAX_TEXT: usize = 128 * 1024;

#[derive(Default)]
pub(crate) struct OpenAiStream {
    pending: Vec<u8>,
    data: String,
    pub text: String,
    pub finish: Option<String>,
    pub usage: Option<Usage>,
    pub done: bool,
}

impl OpenAiStream {
    pub fn push(&mut self, bytes: &[u8], sink: &TextDeltaSink) -> Result<(), LlmError> {
        for byte in bytes {
            if *byte == b'\n' {
                let line = std::str::from_utf8(&self.pending)
                    .map_err(|_| LlmError::Empty)?
                    .trim_end_matches('\r');
                if line.is_empty() {
                    self.decode(sink)?;
                    self.data.clear();
                } else if let Some(data) = line.strip_prefix("data:") {
                    self.data.push_str(data.trim_start());
                    if self.data.len() > MAX_LINE {
                        return Err(LlmError::Empty);
                    }
                }
                self.pending.clear();
            } else {
                self.pending.push(*byte);
                if self.pending.len() > MAX_LINE {
                    return Err(LlmError::Empty);
                }
            }
        }
        Ok(())
    }

    fn decode(&mut self, sink: &TextDeltaSink) -> Result<(), LlmError> {
        if self.data.is_empty() {
            return Ok(());
        }
        if self.data == "[DONE]" {
            self.done = true;
            return Ok(());
        }
        if self.done {
            return Err(LlmError::Empty);
        }
        let v: Value = serde_json::from_str(&self.data).map_err(|_| LlmError::Empty)?;
        if v.get("error").is_some() {
            return Err(LlmError::Empty);
        }
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            let n = |key| {
                u.get(key)
                    .and_then(Value::as_u64)
                    .filter(|n| *n <= i32::MAX as u64)
                    .map(|n| n as u32)
            };
            // Missing/malformed usage is unknown, never free. The accounting
            // layer applies its conservative fallback instead. Keep values
            // representable in the signed database accounting columns.
            self.usage = n("prompt_tokens").zip(n("completion_tokens")).map(
                |(input_tokens, output_tokens)| Usage {
                    input_tokens,
                    output_tokens,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                },
            );
        }
        if let Some(choice) = v
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
        {
            // Explicit content-only extraction: reasoning_content, tool_calls,
            // system prompts and any future unrecognized fields are ignored.
            if let Some(text) = choice
                .get("delta")
                .and_then(|d| d.get("content"))
                .and_then(Value::as_str)
            {
                if self.text.len().saturating_add(text.len()) > MAX_TEXT {
                    return Err(LlmError::Empty);
                }
                self.text.push_str(text);
                if !text.is_empty() {
                    sink(text);
                }
            }
            if let Some(finish) = choice.get("finish_reason").and_then(Value::as_str) {
                self.finish = Some(finish.to_owned());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    #[test]
    fn split_utf8_frames_emit_content_but_not_reasoning() {
        let seen = Arc::new(Mutex::new(String::new()));
        let copy = seen.clone();
        let sink: TextDeltaSink = Arc::new(move |text| copy.lock().unwrap().push_str(text));
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"Café.\",\"reasoning_content\":\"hidden\"},\"finish_reason\":null}]}\r\n\r\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":2}}\n\ndata: [DONE]\n\n";
        let mut parser = OpenAiStream::default();
        for byte in body.as_bytes() {
            parser.push(&[*byte], &sink).unwrap();
        }
        assert_eq!(parser.text, "Café.");
        assert_eq!(*seen.lock().unwrap(), parser.text);
        assert!(parser.done);
        assert_eq!(parser.usage.unwrap().output_tokens, 2);
    }

    #[test]
    fn oversized_frame_is_rejected() {
        let mut parser = OpenAiStream::default();
        let sink: TextDeltaSink = Arc::new(|_| {});
        assert!(parser.push(&vec![b'x'; MAX_LINE + 1], &sink).is_err());
    }

    #[test]
    fn incomplete_or_unrepresentable_usage_is_unknown_not_zero() {
        let sink: TextDeltaSink = Arc::new(|_| {});
        for usage in [
            serde_json::json!({"completion_tokens":2}),
            serde_json::json!({"prompt_tokens":-1,"completion_tokens":2}),
            serde_json::json!({"prompt_tokens":4294967295u64,"completion_tokens":2}),
        ] {
            let mut parser = OpenAiStream::default();
            parser
                .push(
                    format!("data: {}\n\n", serde_json::json!({"usage":usage})).as_bytes(),
                    &sink,
                )
                .unwrap();
            assert!(parser.usage.is_none());
        }
    }
}
