//! Content checks for the owner persona (`Jarvis.md`). They catch corrupt,
//! oversized or accidentally pasted content before it is installed; they do
//! not judge what the persona says. Errors never echo persona content.

/// Largest accepted persona, in bytes. Change this one value to raise or
/// lower the limit.
pub const PERSONA_MAX_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PersonaError {
    #[error("persona is empty")]
    Empty,
    #[error("persona exceeds {PERSONA_MAX_BYTES} bytes")]
    TooLarge,
    #[error("persona is not valid UTF-8")]
    NotUtf8,
    #[error("persona contains a control or invisible formatting character")]
    ControlCharacter,
    #[error("persona must start with a \"# \" heading")]
    MissingHeading,
    #[error("persona appears to contain a key or token")]
    SecretLike,
}

/// Token prefixes and the minimum number of token characters that must
/// follow them. A match must start at a word boundary, so prose such as
/// "risk-based" is not mistaken for an `sk-` key, and the tail must contain a
/// digit, so long kebab-case or snake_case words are not either.
const SECRET_PREFIXES: &[(&str, usize)] = &[
    ("sk-", 20),
    ("sk_live_", 20),
    ("rk_live_", 20),
    ("ghp_", 30),
    ("gho_", 30),
    ("ghu_", 30),
    ("ghs_", 30),
    ("ghr_", 30),
    ("github_pat_", 20),
    ("glpat-", 20),
    ("xoxb-", 10),
    ("xoxp-", 10),
    ("xoxa-", 10),
    ("xoxs-", 10),
    ("xapp-", 10),
    ("xai-", 40),
    ("hf_", 30),
    ("AKIA", 16),
    ("AIza", 30),
    ("eyJ", 30),
];

fn token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
}

fn looks_like_secret(text: &str) -> bool {
    if text.contains("PRIVATE KEY-----") {
        return true;
    }
    let bytes = text.as_bytes();
    SECRET_PREFIXES.iter().any(|&(prefix, min)| {
        text.match_indices(prefix).any(|(start, _)| {
            let tail: Vec<u8> = bytes[start + prefix.len()..]
                .iter()
                .copied()
                .take_while(|byte| token_byte(*byte))
                .collect();
            (start == 0 || !token_byte(bytes[start - 1]))
                && tail.len() >= min
                && tail.iter().any(u8::is_ascii_digit)
        })
    })
}

/// Control characters (C0, DEL, C1) except tab and line endings, and
/// invisible characters that can hide or reorder text in a reviewed diff:
/// bidirectional overrides and isolates, zero-width space, word joiner and a
/// byte order mark.
fn forbidden_char(character: char) -> bool {
    (character.is_control() && !matches!(character, '\t' | '\n' | '\r'))
        || matches!(
            character,
            '\u{200b}' | '\u{202a}'..='\u{202e}' | '\u{2060}' | '\u{2066}'..='\u{2069}' | '\u{feff}'
        )
}

/// Validate persona bytes and return them as text.
pub fn validate_persona(bytes: &[u8]) -> Result<&str, PersonaError> {
    if bytes.is_empty() {
        return Err(PersonaError::Empty);
    }
    if bytes.len() > PERSONA_MAX_BYTES {
        return Err(PersonaError::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| PersonaError::NotUtf8)?;
    if text.chars().any(forbidden_char) {
        return Err(PersonaError::ControlCharacter);
    }
    if !text
        .lines()
        .find(|line| !line.trim().is_empty())
        .is_some_and(|line| line.starts_with("# "))
    {
        return Err(PersonaError::MissingHeading);
    }
    if looks_like_secret(text) {
        return Err(PersonaError::SecretLike);
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = "# Jarvis\n\nYou are a careful assistant.\r\n\tKeep answers short.\n";

    #[test]
    fn accepts_a_normal_persona_and_leading_blank_lines() {
        assert_eq!(validate_persona(VALID.as_bytes()), Ok(VALID));
        assert!(validate_persona(b"\n  \n# Jarvis\nBody").is_ok());
        assert!(validate_persona(b"# J").is_ok());
    }

    #[test]
    fn enforces_the_size_bounds_exactly() {
        assert_eq!(validate_persona(b""), Err(PersonaError::Empty));
        let mut at_cap = b"# Jarvis\n".to_vec();
        at_cap.resize(PERSONA_MAX_BYTES, b'a');
        assert!(validate_persona(&at_cap).is_ok());
        at_cap.push(b'a');
        assert_eq!(validate_persona(&at_cap), Err(PersonaError::TooLarge));
    }

    #[test]
    fn rejects_invalid_utf8_and_control_characters() {
        assert_eq!(
            validate_persona(b"# Jarvis\n\xff\xfe"),
            Err(PersonaError::NotUtf8)
        );
        for control in [
            "\0",
            "\u{1b}[31m",
            "\u{7f}",
            "\u{8}",
            "\u{c}",
            "\u{85}",
            "\u{202e}",
            "\u{2066}",
            "\u{200b}",
            "\u{feff}",
        ] {
            let text = format!("# Jarvis\n{control}");
            assert_eq!(
                validate_persona(text.as_bytes()),
                Err(PersonaError::ControlCharacter),
                "{control:?}"
            );
        }
    }

    #[test]
    fn requires_a_level_one_heading_first() {
        for text in [
            "Jarvis\n# Jarvis",
            "#Jarvis",
            "## Jarvis",
            " # Jarvis",
            "   \n\n",
        ] {
            assert_eq!(
                validate_persona(text.as_bytes()),
                Err(PersonaError::MissingHeading),
                "{text:?}"
            );
        }
    }

    #[test]
    fn rejects_obvious_keys_and_tokens() {
        // Built at runtime so this file holds no realistic secret literal.
        let samples = [
            format!("sk-ant-api03-{}", "a1".repeat(20)),
            format!("sk-proj-{}", "B2".repeat(20)),
            format!("sk_live_{}", "k3".repeat(12)),
            format!("ghp_{}", "c4".repeat(18)),
            format!("github_pat_{}", "d5".repeat(20)),
            format!("AKIA{}", "E6".repeat(8)),
            format!("AIza{}", "f7".repeat(18)),
            format!("xoxb-{}", "1".repeat(20)),
            format!("xapp-{}", "1-a".repeat(5)),
            format!("hf_{}", "g8".repeat(17)),
            format!("glpat-{}", "h9".repeat(10)),
            format!("eyJ{}.{}", "i0".repeat(15), "j".repeat(30)),
            format!("-----BEGIN OPENSSH {}-----", "PRIVATE KEY"),
        ];
        for secret in samples {
            let text = format!("# Jarvis\nUse token ({secret}) here.\n");
            assert_eq!(
                validate_persona(text.as_bytes()),
                Err(PersonaError::SecretLike),
                "{secret}"
            );
        }
    }

    #[test]
    fn ordinary_prose_is_not_mistaken_for_a_secret() {
        let text = "# Jarvis\nA risk-assessment-framework-for-everything-here, the \
                    xai-api provider, task-scoped-and-carefully-bounded-work, sk-short, \
                    sk-learn-compatible-pipelines-and-models, hf_hub_download_with_cache_dir_set, \
                    AKIA and the word eyJ alone. Emoji with a joiner: \u{1f469}\u{200d}\u{1f4bb}.\n";
        assert!(validate_persona(text.as_bytes()).is_ok());
    }

    #[test]
    fn errors_never_contain_content() {
        let text = format!("# Jarvis\nmarker-{}\0", "x".repeat(10));
        let error = validate_persona(text.as_bytes()).unwrap_err().to_string();
        assert!(!error.contains("marker"));
    }
}
