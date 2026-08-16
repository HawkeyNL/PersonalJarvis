//! Server-side speaker verification + STT. Voice is a *convenience* signal only:
//! an enrolled embedding is compared by cosine similarity to gate friendliness,
//! never to authorize anything — the device-signed boundary stays the real gate.
//! Embeddings are stored as little-endian f32 blobs in `voice_profiles`.

use axum::{extract::State, http::StatusCode, Json};
use serde::Deserialize;
use serde_json::{json, Value};

use jarvis_speech as speech;

use crate::error::{bad_request, db_err, speech_err};
use crate::{AppState, Authed};

fn encode_embedding(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn decode_embedding(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

#[derive(Deserialize)]
pub(crate) struct AudioReq {
    sample_rate: u32,
    /// 16-bit mono PCM samples.
    pcm: Vec<i16>,
}

fn to_audio(req: AudioReq) -> Result<speech::Audio, (StatusCode, Json<Value>)> {
    if req.pcm.is_empty() {
        return Err(bad_request("audio is required"));
    }
    Ok(speech::Audio::new(req.pcm, req.sample_rate))
}

/// Whether the authenticated user has enrolled a voice profile.
pub(crate) async fn voice_status(
    authed: Authed,
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let exists: Option<i32> = sqlx::query_scalar("select 1 from voice_profiles where user_id = $1")
        .bind(authed.user.id)
        .fetch_optional(&state.db)
        .await
        .map_err(db_err)?;
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
    sqlx::query(
        "insert into voice_profiles (user_id, embedding, dims, engine, updated_at) \
         values ($1, $2, $3, $4, now()) \
         on conflict (user_id) do update set \
           embedding = $2, dims = $3, engine = $4, updated_at = now()",
    )
    .bind(authed.user.id)
    .bind(&bytes)
    .bind(embedding.len() as i32)
    .bind(state.speech.label())
    .execute(&state.db)
    .await
    .map_err(db_err)?;
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

    let stored: Option<Vec<u8>> =
        sqlx::query_scalar("select embedding from voice_profiles where user_id = $1")
            .bind(authed.user.id)
            .fetch_optional(&state.db)
            .await
            .map_err(db_err)?;

    match stored {
        None => Ok(Json(json!({
            "enrolled": false,
            "is_you": false,
            "score": 0.0,
            "transcript": transcript,
        }))),
        Some(bytes) => {
            let profile = decode_embedding(&bytes);
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
