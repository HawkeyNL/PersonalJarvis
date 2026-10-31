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

/// Non-blank, at most `max_len` bytes, printable ASCII (space to `~`) only.
pub fn printable_ascii(value: &str, max_len: usize) -> bool {
    !value.trim().is_empty()
        && value.len() <= max_len
        && value.bytes().all(|b| (b' '..=b'~').contains(&b))
}

/// No control, zero-width, bidi, line/paragraph separator, variation
/// selector, tag, Hangul filler or interlinear annotation characters, so a
/// device name shown next to a fingerprint cannot hide or reorder what the
/// owner reads.
/// Other Unicode stays allowed: default names such as "Gus’s iPhone" use it.
/// ponytail: explicit denylist of the invisible/bidi code points, not the
/// whole Unicode `Cf` category; switch to a category table if one is needed.
pub fn no_hidden_chars(value: &str) -> bool {
    !value.chars().any(|c| {
        c.is_control()
            || matches!(c, '\u{00AD}' | '\u{034F}' | '\u{061C}' | '\u{115F}' | '\u{1160}'
                | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2069}' | '\u{3164}' | '\u{FE00}'..='\u{FE0F}' | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}' | '\u{E0000}'..='\u{E007F}')
    })
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
        assert!(no_hidden_chars("Gus\u{2019}s iPhone (Ünïcode)"));
        for bad in [
            "a\u{7}b",
            "a\nb",
            "abc\u{202E}fed",
            "a\u{200B}b",
            "a\u{2066}b",
            "\u{FEFF}a",
            "a\u{AD}b",
            "a\u{2028}b",
            "a\u{2029}b",
            "a\u{E0000}b",
            "a\u{E0041}b",
            "a\u{E007F}b",
            "a\u{FE00}b",
            "a\u{FE0F}b",
            "a\u{34F}b",
            "a\u{115F}b",
            "a\u{1160}b",
            "a\u{3164}b",
            "a\u{FFF9}b",
            "a\u{FFFB}b",
        ] {
            assert!(!no_hidden_chars(bad), "{bad:?}");
        }
        assert!(printable_ascii("Pixel 9 Pro", MAX_DEVICE_MODEL_LEN));
        assert!(!printable_ascii("  ", MAX_DEVICE_MODEL_LEN));
        assert!(!printable_ascii("Pixel\u{202E}9", MAX_DEVICE_MODEL_LEN));
        assert!(!printable_ascii(&"x".repeat(65), MAX_DEVICE_MODEL_LEN));
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
