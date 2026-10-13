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
    #[error("persona contains a control character")]
    ControlCharacter,
    #[error("persona must start with a \"# \" heading")]
    MissingHeading,
    #[error("persona appears to contain a key or token")]
    SecretLike,
}

/// Token prefixes and the minimum number of token characters that must
/// follow them. A match must start at a word boundary, so prose such as
/// "risk-based" is not mistaken for an `sk-` key.
const SECRET_PREFIXES: &[(&str, usize)] = &[
    ("sk-", 20),
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
            (start == 0 || !token_byte(bytes[start - 1]))
                && bytes[start + prefix.len()..]
                    .iter()
                    .take_while(|byte| token_byte(**byte))
                    .count()
                    >= min
        })
    })
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
    // C0 controls and DEL, except tab and line endings.
    if text
        .bytes()
        .any(|byte| byte.is_ascii_control() && !matches!(byte, b'\t' | b'\n' | b'\r'))
    {
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
        for control in ["\0", "\u{1b}[31m", "\u{7f}", "\u{8}", "\u{c}"] {
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
            "\u{feff}# Jarvis",
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
            format!("sk-ant-api03-{}", "a".repeat(40)),
            format!("sk-proj-{}", "B".repeat(40)),
            format!("ghp_{}", "c".repeat(36)),
            format!("github_pat_{}", "d".repeat(40)),
            format!("AKIA{}", "E".repeat(16)),
            format!("AIza{}", "f".repeat(35)),
            format!("xoxb-{}", "1".repeat(20)),
            format!("hf_{}", "g".repeat(34)),
            format!("glpat-{}", "h".repeat(20)),
            format!("eyJ{}.{}", "i".repeat(30), "j".repeat(30)),
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
                    AKIA and the word eyJ alone.\n";
        assert!(validate_persona(text.as_bytes()).is_ok());
    }

    #[test]
    fn errors_never_contain_content() {
        let text = format!("# Jarvis\nmarker-{}\0", "x".repeat(10));
        let error = validate_persona(text.as_bytes()).unwrap_err().to_string();
        assert!(!error.contains("marker"));
    }
}
