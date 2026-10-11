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

// Pairing candidate details shown to the approving owner.
pub const MAX_USER_AGENT_LEN: usize = 256;
pub const MAX_APP_VERSION_LEN: usize = 32;
pub const MAX_DEVICE_MODEL_LEN: usize = 64;

/// Semver-ish: starts with a digit, then only `[0-9A-Za-z.+-]`.
pub fn app_version(value: &str) -> bool {
    value.len() <= MAX_APP_VERSION_LEN
        && value.starts_with(|c: char| c.is_ascii_digit())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-'))
}

/// Non-blank text of at most `max_chars` characters without control,
/// zero-width or bidi formatting characters, so a value shown next to a
/// fingerprint cannot disguise or reorder what the owner reads.
pub fn display_text(value: &str, max_chars: usize) -> bool {
    !value.trim().is_empty()
        && value.chars().count() <= max_chars
        && !value.chars().any(|c| {
            c.is_control()
                || matches!(c, '\u{061C}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
                    | '\u{2060}'..='\u{2069}' | '\u{FEFF}')
        })
}

/// Non-empty, at most `max_len` bytes, printable ASCII (space to `~`) only.
pub fn printable_ascii(value: &str, max_len: usize) -> bool {
    !value.is_empty() && value.len() <= max_len && value.bytes().all(|b| (b' '..=b'~').contains(&b))
}

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
    fn pairing_details_are_strictly_bounded() {
        for ok in [
            "0.1.14",
            "1.2.3-beta.1+build.5",
            &"1".repeat(MAX_APP_VERSION_LEN),
        ] {
            assert!(app_version(ok), "{ok}");
        }
        for bad in [
            "",
            "v1.2.3",
            "1.2 3",
            "1.2.3/x",
            "1.2.3\n",
            "١.2",
            &"1".repeat(33),
        ] {
            assert!(!app_version(bad), "{bad}");
        }
        assert!(display_text("Pixel 9 Pro (Ünïcode)", MAX_DEVICE_MODEL_LEN));
        assert!(display_text(&"é".repeat(64), MAX_DEVICE_MODEL_LEN)); // chars, not bytes
        for bad in [
            "",
            "  ",
            "a\u{7}b",
            "a\nb",
            "abc\u{202E}fed",
            "a\u{200B}b",
            "a\u{2066}b",
            "\u{FEFF}a",
            &"x".repeat(65),
        ] {
            assert!(!display_text(bad, MAX_DEVICE_MODEL_LEN), "{bad:?}");
        }
        assert!(printable_ascii("Mozilla/5.0 (X11) ~", MAX_USER_AGENT_LEN));
        assert!(!printable_ascii("", MAX_USER_AGENT_LEN));
        assert!(!printable_ascii("a\tb", MAX_USER_AGENT_LEN));
        assert!(!printable_ascii("caf\u{e9}", MAX_USER_AGENT_LEN));
        assert!(!printable_ascii(&"a".repeat(257), MAX_USER_AGENT_LEN));
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
