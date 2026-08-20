//! Jarvis Core — application identity and future orchestration boundary.
//!
//! The Core deliberately owns no HTTP transport, policy rules, approval state,
//! sandbox, or executor. Those remain in their dedicated crates. Today it owns
//! the canonical Jarvis persona loaded by the native Home-Node process; future
//! orchestration may move here only when it has a real ownership boundary.

use std::sync::Arc;

/// Fallback persona used when the protected on-disk persona cannot be read.
/// This keeps development and CI functional while production logs the failed
/// load at startup.
pub const JARVIS_SYSTEM_FALLBACK: &str = "Je bent Jarvis, de persoonlijke AI-assistent op het HUD-dashboard van de gebruiker. \
Antwoord in het Nederlands, kort en duidelijk, in een rustige en behulpzame toon. \
Je helpt met het systeem, de portfolio en trading-inzichten. \
Zeg het eerlijk wanneer je iets niet zeker weet in plaats van te gokken. \
Voer nooit trades of onomkeerbare acties uit — die vereisen altijd een expliciete bevestiging van de gebruiker.";

/// Load Jarvis' canonical persona from `path`.
///
/// A missing, unreadable, or empty file fails safely to the built-in persona.
/// The caller is responsible for recording that degraded startup state without
/// exposing the file contents or filesystem details to API clients.
pub fn load_persona(path: &str) -> (Arc<str>, bool) {
    match std::fs::read_to_string(path) {
        Ok(text) if !text.trim().is_empty() => (Arc::from(text.trim()), true),
        _ => (Arc::from(JARVIS_SYSTEM_FALLBACK), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persona_falls_back_when_file_is_absent() {
        let (text, loaded) = load_persona("does/not/exist/Jarvis.md");
        assert!(!loaded);
        assert_eq!(&*text, JARVIS_SYSTEM_FALLBACK);
    }

    #[test]
    fn persona_loads_from_file_when_present() {
        let path = std::env::temp_dir().join("jarvis_persona_test.md");
        std::fs::write(&path, "  Je bent Jarvis, de kern.  \n").unwrap();
        let (text, loaded) = load_persona(path.to_str().unwrap());
        assert!(loaded);
        assert_eq!(&*text, "Je bent Jarvis, de kern.");
        let _ = std::fs::remove_file(&path);
    }
}
