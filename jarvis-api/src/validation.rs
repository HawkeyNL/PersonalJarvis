//! Request validation helpers for security-sensitive API inputs.

pub use jarvis_client_core::{
    ED25519_PUBLIC_KEY_HEX_LEN, ED25519_SIGNATURE_HEX_LEN, MAX_CHAT_CONTENT_LEN, MAX_CHAT_TURNS,
    MAX_DEVICE_NAME_LEN, MAX_PLATFORM_LEN,
};

// Generous upper bounds on free-text inputs so a single request cannot ship an
// unbounded string into the DB or an LLM prompt. Sized well above real use.
pub const MAX_SYMBOL_LEN: usize = 32;
pub const MAX_CURRENCY_LEN: usize = 8;
pub const MAX_FOCUS_LEN: usize = 500;
pub const MAX_TASK_LEN: usize = 8_000;
// Chat is the primary feature, so these are deliberately generous: they cap
// abuse (and runaway token spend) without touching real conversations.

pub fn bounded_text(value: &str, max_len: usize) -> bool {
    jarvis_client_core::bounded_text(value, max_len)
}

pub fn is_hex_of_len(value: &str, len: usize) -> bool {
    jarvis_client_core::is_hex_of_len(value, len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_text_rejects_empty_and_oversized() {
        assert!(bounded_text("iPhone", MAX_DEVICE_NAME_LEN));
        assert!(bounded_text(
            &"x".repeat(MAX_DEVICE_NAME_LEN),
            MAX_DEVICE_NAME_LEN
        ));
        assert!(!bounded_text("", MAX_DEVICE_NAME_LEN));
        assert!(!bounded_text("   ", MAX_DEVICE_NAME_LEN)); // whitespace-only
        assert!(!bounded_text(
            &"x".repeat(MAX_DEVICE_NAME_LEN + 1),
            MAX_DEVICE_NAME_LEN
        ));
    }

    #[test]
    fn is_hex_of_len_requires_exact_length_and_hex() {
        assert!(is_hex_of_len(
            &"a".repeat(ED25519_PUBLIC_KEY_HEX_LEN),
            ED25519_PUBLIC_KEY_HEX_LEN
        ));
        assert!(is_hex_of_len(
            &"0".repeat(ED25519_SIGNATURE_HEX_LEN),
            ED25519_SIGNATURE_HEX_LEN
        ));
        assert!(!is_hex_of_len("abcd", ED25519_PUBLIC_KEY_HEX_LEN)); // too short
        assert!(!is_hex_of_len(
            &"z".repeat(ED25519_PUBLIC_KEY_HEX_LEN),
            ED25519_PUBLIC_KEY_HEX_LEN
        )); // non-hex chars
    }
}
