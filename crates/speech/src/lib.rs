//! Server-side speech: transcription (STT) + speaker verification.
//!
//! Provider-abstracted like the brain (`jarvis-llm`): a [`SpeechEngine`] trait
//! with a deterministic [`StubEngine`] for tests and the wiring, so the whole
//! pipeline (audio upload → transcribe + embed → compare to the enrolled
//! profile) is testable before a real model (Whisper / a speaker-embedding net)
//! is plugged in behind the trait.
//!
//! The speaker *profile* is a voice embedding stored server-side per user, so it
//! is inherently shared across that user's devices (the server is the source of
//! truth — no per-device sync needed).

use std::sync::Arc;

use async_trait::async_trait;

mod speaker;
pub(crate) use speaker::speaker_embedding;

#[cfg(feature = "whisper")]
mod whisper_engine;

/// 16-bit mono PCM audio at a known sample rate.
#[derive(Debug, Clone)]
pub struct Audio {
    pub pcm: Vec<i16>,
    pub sample_rate: u32,
}

impl Audio {
    pub fn new(pcm: Vec<i16>, sample_rate: u32) -> Self {
        Self { pcm, sample_rate }
    }
    pub fn is_empty(&self) -> bool {
        self.pcm.is_empty()
    }
}

/// Errors from a speech engine.
#[derive(Debug, thiserror::Error)]
pub enum SpeechError {
    #[error("speech engine not configured: {0}")]
    NotConfigured(String),
    #[error("audio was empty or too short")]
    TooShort,
    #[error("speech engine failed: {0}")]
    Failed(String),
}

/// A pluggable speech backend: transcription + speaker embedding.
#[async_trait]
pub trait SpeechEngine: Send + Sync {
    /// Short label for logs (e.g. `"stub"`, `"whisper:base"`).
    fn label(&self) -> &str;

    /// Transcribe speech to text.
    async fn transcribe(&self, audio: &Audio) -> Result<String, SpeechError>;

    /// Produce a fixed-length speaker embedding for verification.
    async fn embed(&self, audio: &Audio) -> Result<Vec<f32>, SpeechError>;
}

/// Cosine similarity in [-1, 1]; 0 when either vector is degenerate.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// Engine with a real speaker embedding but no STT: `embed` is the MFCC-based
/// [`speaker_embedding`] (deterministic — same audio → cosine 1.0), so speaker
/// verification works out of the box; `transcribe` returns empty until a real
/// STT model (whisper) is enabled. This is the default provider.
pub struct StubEngine;

#[async_trait]
impl SpeechEngine for StubEngine {
    fn label(&self) -> &str {
        "baseline"
    }

    async fn transcribe(&self, audio: &Audio) -> Result<String, SpeechError> {
        if audio.is_empty() {
            return Err(SpeechError::TooShort);
        }
        Ok(String::new())
    }

    async fn embed(&self, audio: &Audio) -> Result<Vec<f32>, SpeechError> {
        let v = speaker_embedding(audio);
        if v.is_empty() {
            return Err(SpeechError::TooShort);
        }
        Ok(v)
    }
}

/// How to build the engine.
#[derive(Debug, Clone, Default)]
pub struct EngineConfig {
    pub provider: String,
    /// Path to the Whisper GGML model (for `provider = "whisper"`).
    pub whisper_model: Option<String>,
    /// Whisper decode language (`nl`, `auto`, …).
    pub whisper_language: String,
}

/// Build a speech engine from config. `stub` is always available; `whisper`
/// requires the `whisper` feature (and a model), else it degrades to the stub.
pub fn build_engine(cfg: &EngineConfig) -> Arc<dyn SpeechEngine> {
    match cfg.provider.as_str() {
        "stub" => Arc::new(StubEngine),
        "whisper" => build_whisper(cfg),
        other => {
            tracing::warn!(provider = other, "unknown speech provider; using stub");
            Arc::new(StubEngine)
        }
    }
}

#[cfg(feature = "whisper")]
fn build_whisper(cfg: &EngineConfig) -> Arc<dyn SpeechEngine> {
    match whisper_engine::WhisperEngine::load(cfg) {
        Ok(engine) => {
            tracing::info!(engine = engine.label(), "whisper STT loaded");
            Arc::new(engine)
        }
        Err(e) => {
            tracing::error!(error = %e, "whisper unavailable; falling back to stub");
            Arc::new(StubEngine)
        }
    }
}

#[cfg(not(feature = "whisper"))]
fn build_whisper(_cfg: &EngineConfig) -> Arc<dyn SpeechEngine> {
    tracing::warn!(
        "speech provider 'whisper' requested but the crate was built without the \
         'whisper' feature; using stub. Rebuild the api with --features speech-whisper."
    );
    Arc::new(StubEngine)
}

/// The deterministic stub, as a trait object.
pub fn stub() -> Arc<dyn SpeechEngine> {
    Arc::new(StubEngine)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(len: usize, step: i16) -> Audio {
        let pcm: Vec<i16> = (0..len).map(|i| ((i as i16).wrapping_mul(step)) % 8000).collect();
        Audio::new(pcm, 16000)
    }

    #[test]
    fn cosine_identity_and_orthogonal() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        assert!((cosine(&a, &a) - 1.0).abs() < 1e-6);
        assert!(cosine(&a, &b).abs() < 1e-6);
        assert_eq!(cosine(&a, &[1.0, 0.0]), 0.0); // mismatched length
    }

    #[tokio::test]
    async fn baseline_embed_is_deterministic_and_discriminative() {
        let e = StubEngine;
        let a = tone(16000, 7);
        let b = tone(16000, 31);
        let ea1 = e.embed(&a).await.unwrap();
        let ea2 = e.embed(&a).await.unwrap();
        let eb = e.embed(&b).await.unwrap();
        assert!(!ea1.is_empty());
        // Same audio → identical embedding (perfect match).
        assert!((cosine(&ea1, &ea2) - 1.0).abs() < 1e-6);
        // Different audio → lower similarity than a self-match.
        assert!(cosine(&ea1, &eb) < 0.999);
    }

    #[tokio::test]
    async fn empty_audio_is_rejected() {
        let e = StubEngine;
        let empty = Audio::new(vec![], 16000);
        assert!(matches!(e.embed(&empty).await, Err(SpeechError::TooShort)));
        assert!(matches!(
            e.transcribe(&empty).await,
            Err(SpeechError::TooShort)
        ));
    }

    #[test]
    fn build_defaults_to_baseline() {
        let e = build_engine(&EngineConfig {
            provider: "stub".into(),
            ..Default::default()
        });
        assert_eq!(e.label(), "baseline");
    }
}
