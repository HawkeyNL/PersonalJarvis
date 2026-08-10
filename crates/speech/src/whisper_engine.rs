//! Real speech-to-text via whisper.cpp (the `whisper-rs` bindings).
//!
//! Gated behind the `whisper` feature — building whisper.cpp needs a C++
//! toolchain + cmake, so the default build keeps the stub. Speaker embedding is
//! still the placeholder energy fingerprint here (real speaker model = stage 2,
//! ADR-025); only `transcribe` is a real model.

use std::sync::Arc;

use async_trait::async_trait;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::{stub_embedding, Audio, EngineConfig, SpeechEngine, SpeechError};

/// Whisper STT engine. The loaded model is read-only and shared across calls;
/// each transcription gets its own decode state and runs on a blocking thread.
pub struct WhisperEngine {
    ctx: Arc<WhisperContext>,
    label: String,
    language: String,
}

impl WhisperEngine {
    /// Load the GGML model named by `cfg.whisper_model`.
    pub fn load(cfg: &EngineConfig) -> Result<Self, SpeechError> {
        let path = cfg.whisper_model.as_deref().ok_or_else(|| {
            SpeechError::NotConfigured("JARVIS_SPEECH_WHISPER_MODEL not set".into())
        })?;
        let ctx = WhisperContext::new_with_params(path, WhisperContextParameters::default())
            .map_err(|e| SpeechError::NotConfigured(format!("whisper model '{path}': {e}")))?;
        let language = if cfg.whisper_language.trim().is_empty() {
            "auto".to_string()
        } else {
            cfg.whisper_language.clone()
        };
        let name = std::path::Path::new(path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("whisper");
        Ok(Self {
            ctx: Arc::new(ctx),
            label: format!("whisper:{name}"),
            language,
        })
    }
}

#[async_trait]
impl SpeechEngine for WhisperEngine {
    fn label(&self) -> &str {
        &self.label
    }

    async fn transcribe(&self, audio: &Audio) -> Result<String, SpeechError> {
        if audio.is_empty() {
            return Err(SpeechError::TooShort);
        }
        let samples = to_mono_f32_16k(audio);
        let ctx = self.ctx.clone();
        let language = self.language.clone();
        // Whisper is CPU/GPU-bound and synchronous; keep it off the async runtime.
        tokio::task::spawn_blocking(move || decode(&ctx, &language, &samples))
            .await
            .map_err(|e| SpeechError::Failed(format!("whisper task: {e}")))?
    }

    async fn embed(&self, audio: &Audio) -> Result<Vec<f32>, SpeechError> {
        // Placeholder speaker embedding (energy fingerprint). Real speaker model
        // is stage 2 (ADR-025); kept so enroll/verify stays functional meanwhile.
        if audio.is_empty() {
            return Err(SpeechError::TooShort);
        }
        Ok(stub_embedding(audio))
    }
}

fn decode(ctx: &WhisperContext, language: &str, samples: &[f32]) -> Result<String, SpeechError> {
    let mut state = ctx
        .create_state()
        .map_err(|e| SpeechError::Failed(format!("whisper state: {e}")))?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    if language != "auto" {
        params.set_language(Some(language));
    }
    params.set_translate(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_special(false);
    params.set_print_timestamps(false);

    state
        .full(params, samples)
        .map_err(|e| SpeechError::Failed(format!("whisper decode: {e}")))?;

    let segments = state
        .full_n_segments()
        .map_err(|e| SpeechError::Failed(format!("whisper segments: {e}")))?;
    let mut text = String::new();
    for i in 0..segments {
        let seg = state
            .full_get_segment_text(i)
            .map_err(|e| SpeechError::Failed(format!("whisper text: {e}")))?;
        text.push_str(seg.trim());
        text.push(' ');
    }
    Ok(text.trim().to_string())
}

/// i16 mono PCM → f32 mono at 16 kHz (Whisper's expected input).
fn to_mono_f32_16k(audio: &Audio) -> Vec<f32> {
    let f: Vec<f32> = audio.pcm.iter().map(|&s| s as f32 / 32768.0).collect();
    if audio.sample_rate == 16_000 {
        f
    } else {
        resample_linear(&f, audio.sample_rate, 16_000)
    }
}

fn resample_linear(input: &[f32], in_rate: u32, out_rate: u32) -> Vec<f32> {
    if in_rate == out_rate || input.is_empty() {
        return input.to_vec();
    }
    let ratio = in_rate as f64 / out_rate as f64;
    let out_len = ((input.len() as f64) / ratio).floor() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let idx = i as f64 * ratio;
        let i0 = idx.floor() as usize;
        let frac = (idx - i0 as f64) as f32;
        let a = input[i0];
        let b = *input.get(i0 + 1).unwrap_or(&a);
        out.push(a * (1.0 - frac) + b * frac);
    }
    out
}
