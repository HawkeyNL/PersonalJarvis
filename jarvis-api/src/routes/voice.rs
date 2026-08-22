//! Server-side speaker verification + STT. Voice is a *convenience* signal only:
//! an enrolled embedding is compared by cosine similarity to gate friendliness,
//! never to authorize anything — the device-signed boundary stays the real gate.
//! Embeddings are stored as little-endian f32 blobs in `voice_profiles`.

use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use jarvis_speech as speech;

use crate::error::{bad_request, speech_err};
use crate::{AppState, Authed};

/// At most five minutes of 48 kHz mono PCM. This is deliberately far above a
/// normal voice command while bounding decode, embedding and transcription work.
const MAX_AUDIO_SAMPLES: usize = 240_000;

fn encode_embedding(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn decode_embedding(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

#[derive(Serialize)]
struct VoiceBindings {
    user_id: String,
    #[serde(with = "serde_bytes")]
    embedding: Vec<u8>,
    dims: i64,
    engine: String,
}

#[derive(Deserialize)]
struct VoiceRow {
    #[serde(with = "serde_bytes")]
    embedding: Vec<u8>,
}

#[derive(Deserialize)]
pub(crate) struct AudioReq {
    sample_rate: u32,
    /// 16-bit mono PCM samples.
    pcm: Vec<i16>,
}

fn to_audio(req: AudioReq) -> Result<speech::Audio, (StatusCode, Json<Value>)> {
    if req.pcm.is_empty() || req.pcm.len() > MAX_AUDIO_SAMPLES {
        return Err(bad_request("audio is required"));
    }
    if !(8_000..=48_000).contains(&req.sample_rate) {
        return Err(bad_request("invalid sample_rate"));
    }
    Ok(speech::Audio::new(req.pcm, req.sample_rate))
}

/// Whether the authenticated user has enrolled a voice profile.
pub(crate) async fn voice_status(
    authed: Authed,
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut response = state
        .db
        .query("SELECT user_id FROM voice_profiles WHERE user_id = $user_id LIMIT 1")
        .bind(json!({"user_id": authed.user.id.to_string()}))
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":"internal error"})),
            )
        })?;
    let exists: Option<serde_json::Value> = response.take(0).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"internal error"})),
        )
    })?;
    Ok(Json(json!({
        "enrolled": exists.is_some(),
        "engine": state.speech.label(),
    })))
}

/// Enroll (or re-enroll) the user's voice: embed the audio and store it centrally.
pub(crate) async fn voice_enroll(
    authed: Authed,
    State(state): State<AppState>,
    Json(req): Json<AudioReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let audio = to_audio(req)?;
    let embedding = state.speech.embed(&audio).await.map_err(speech_err)?;
    let bytes = encode_embedding(&embedding);
    state.db.query(
        "UPSERT voice_profiles SET user_id = $user_id, embedding = <bytes>$embedding, dims = $dims, \
         engine = $engine, created_at = time::now(), updated_at = time::now() RETURN NONE",
    ).bind(VoiceBindings { user_id: authed.user.id.to_string(), embedding: bytes,
        dims: embedding.len() as i64, engine: state.speech.label().to_string() }).await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error":"internal error"}))))?;
    Ok(Json(
        json!({ "status": "enrolled", "dims": embedding.len() }),
    ))
}

/// Verify a voice against the enrolled profile and transcribe it.
pub(crate) async fn voice_verify(
    authed: Authed,
    State(state): State<AppState>,
    Json(req): Json<AudioReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let audio = to_audio(req)?;
    let embedding = state.speech.embed(&audio).await.map_err(speech_err)?;
    let transcript = state.speech.transcribe(&audio).await.unwrap_or_default();

    let mut response = state
        .db
        .query("SELECT embedding FROM voice_profiles WHERE user_id = $user_id LIMIT 1")
        .bind(json!({"user_id": authed.user.id.to_string()}))
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":"internal error"})),
            )
        })?;
    let stored: Option<VoiceRow> = response.take(0).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"internal error"})),
        )
    })?;

    match stored {
        None => Ok(Json(json!({
            "enrolled": false,
            "is_you": false,
            "score": 0.0,
            "transcript": transcript,
        }))),
        Some(row) => {
            let profile = decode_embedding(&row.embedding);
            let score = speech::cosine(&profile, &embedding);
            Ok(Json(json!({
                "enrolled": true,
                "is_you": score >= state.speech_verify_threshold,
                "score": score,
                "transcript": transcript,
            })))
        }
    }
}
