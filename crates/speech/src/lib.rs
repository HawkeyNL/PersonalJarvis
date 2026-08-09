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

/// Dimensionality of the stub embedding.
const STUB_DIM: usize = 32;

/// A deterministic, network-free engine for tests and wiring. `embed` is a
/// coarse energy fingerprint: the same audio yields the same vector (cosine 1.0)
/// and different audio yields a different one — enough to exercise enroll/verify
/// end-to-end. `transcribe` returns empty (there is no real model here).
pub struct StubEngine;

#[async_trait]
impl SpeechEngine for StubEngine {
    fn label(&self) -> &str {
        "stub"
    }

    async fn transcribe(&self, audio: &Audio) -> Result<String, SpeechError> {
        if audio.is_empty() {
            return Err(SpeechError::TooShort);
        }
        Ok(String::new())
    }

    async fn embed(&self, audio: &Audio) -> Result<Vec<f32>, SpeechError> {
        if audio.is_empty() {
            return Err(SpeechError::TooShort);
        }
        Ok(stub_embedding(audio))
    }
}

fn stub_embedding(audio: &Audio) -> Vec<f32> {
    // RMS energy over STUB_DIM equal chunks, then L2-normalized.
    let mut v = vec![0.0f32; STUB_DIM];
    let n = audio.pcm.len();
    for (i, bucket) in v.iter_mut().enumerate() {
        let start = i * n / STUB_DIM;
        let end = ((i + 1) * n / STUB_DIM).max(start + 1).min(n);
        let mut sum = 0.0f64;
        let mut count = 0u64;
        for &s in &audio.pcm[start..end.min(n)] {
            let x = s as f64 / 32768.0;
            sum += x * x;
            count += 1;
        }
        *bucket = if count > 0 {
            (sum / count as f64).sqrt() as f32
        } else {
            0.0
        };
    }
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

/// How to build the engine.
pub struct EngineConfig {
    pub provider: String,
}

/// Build a speech engine from config. Only `stub` exists today; real engines
/// (Whisper, speaker-embedding nets) slot in here behind the same trait.
pub fn build_engine(cfg: &EngineConfig) -> Arc<dyn SpeechEngine> {
    match cfg.provider.as_str() {
        "stub" => Arc::new(StubEngine),
        // Real providers (e.g. "whisper") slot in here.
        other => {
            tracing::warn!(provider = other, "unknown speech provider; using stub");
            Arc::new(StubEngine)
        }
    }
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
    async fn stub_embed_is_deterministic_and_discriminative() {
        let e = StubEngine;
        let a = tone(16000, 7);
        let b = tone(16000, 31);
        let ea1 = e.embed(&a).await.unwrap();
        let ea2 = e.embed(&a).await.unwrap();
        let eb = e.embed(&b).await.unwrap();
        assert_eq!(ea1.len(), STUB_DIM);
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
    fn build_defaults_to_stub() {
        let e = build_engine(&EngineConfig {
            provider: "stub".into(),
        });
        assert_eq!(e.label(), "stub");
    }
}
