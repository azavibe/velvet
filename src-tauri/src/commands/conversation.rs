//! Tauri commands for the Conversations feature: start/stop dual-channel
//! capture, drain transcribed chunks into the DB + frontend events, and
//! generate on-demand suggestions from a persona prompt + the transcript
//! so far.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, State};

use super::ResultExt;
use crate::audio::conversation::{ConversationCapture, ConversationChunk, ConversationState};
use crate::database::{ConversationDetail, ConversationSummary, Database};
use crate::reasoning::{self, ReasoningRequest};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Clone, serde::Serialize)]
struct ConversationUtterancePayload {
    id: i64,
    conversation_id: i64,
    channel: String,
    started_at_ms: i64,
    text: String,
}

#[derive(Clone, serde::Serialize)]
struct ConversationErrorPayload {
    conversation_id: i64,
    message: String,
}

#[tauri::command]
pub async fn start_conversation(
    app: AppHandle,
    conv_state: State<'_, Arc<ConversationState>>,
    db: State<'_, Database>,
    mic_device_id: Option<String>,
    groq_api_key: String,
    persona_name: Option<String>,
) -> Result<i64, String> {
    log::info!("[Conversation] starting, persona={:?}", persona_name);

    let conversation_id = db.create_conversation(persona_name.as_deref()).str_err()?;

    if let Err(e) = ConversationCapture::start(&**conv_state, mic_device_id) {
        // Roll back the DB row we just created rather than leaving an
        // empty, never-ended conversation behind.
        let _ = db.delete_conversation(conversation_id);
        return Err(e.to_string());
    }

    let rx = match conv_state.take_chunk_receiver() {
        Some(rx) => rx,
        None => {
            let _ = ConversationCapture::stop(&**conv_state);
            let _ = db.delete_conversation(conversation_id);
            return Err("Conversation capture started without a chunk receiver".to_string());
        }
    };

    let app_for_consumer = app.clone();
    let api_key = groq_api_key;

    // Drain finished chunks on a blocking thread (std::mpsc::Receiver::recv
    // blocks); spawn each chunk's transcription as its own async task so a
    // slow "them" chunk never delays "me" chunks queued behind it.
    tauri::async_runtime::spawn_blocking(move || {
        while let Ok(chunk) = rx.recv() {
            let app2 = app_for_consumer.clone();
            let api_key2 = api_key.clone();
            tauri::async_runtime::spawn(async move {
                handle_conversation_chunk(app2, conversation_id, chunk, api_key2).await;
            });
        }
        log::info!("[Conversation] chunk consumer ended for conversation {}", conversation_id);
    });

    Ok(conversation_id)
}

async fn handle_conversation_chunk(app: AppHandle, conversation_id: i64, chunk: ConversationChunk, api_key: String) {
    let channel_str = chunk.channel.as_str();
    let started_at_ms = chunk.started_at_ms;

    let result = crate::transcription::cloud::transcribe_groq(
        chunk.wav,
        &api_key,
        "whisper-large-v3-turbo",
        None,
        None,
    )
    .await;

    let text = match result {
        Ok(t) => t.text.trim().to_string(),
        Err(e) => {
            log::error!("[Conversation] {} chunk transcription failed: {}", channel_str, e);
            let _ = app.emit(
                "conversation-error",
                ConversationErrorPayload { conversation_id, message: e.to_string() },
            );
            return;
        }
    };

    if text.is_empty() {
        return;
    }

    let db = app.state::<Database>();
    let utterance_id = match db.insert_conversation_utterance(conversation_id, channel_str, started_at_ms, &text) {
        Ok(id) => id,
        Err(e) => {
            log::error!("[Conversation] failed to persist utterance: {}", e);
            return;
        }
    };

    let _ = app.emit(
        "conversation-utterance",
        ConversationUtterancePayload {
            id: utterance_id,
            conversation_id,
            channel: channel_str.to_string(),
            started_at_ms,
            text,
        },
    );
}

#[tauri::command]
pub fn stop_conversation(
    conv_state: State<'_, Arc<ConversationState>>,
    db: State<'_, Database>,
    conversation_id: i64,
    title: Option<String>,
) -> Result<(), String> {
    // Stopping joins both capture threads; each flushes its trailing
    // buffered utterance as one last chunk before exiting. That final
    // chunk is still transcribed asynchronously by the consumer task
    // spawned in start_conversation, so a `conversation-utterance` event
    // for it can arrive slightly after this command returns.
    ConversationCapture::stop(&**conv_state).str_err()?;
    db.end_conversation(conversation_id, title.as_deref()).str_err()?;
    Ok(())
}

#[tauri::command]
pub fn get_conversation_audio_levels(conv_state: State<'_, Arc<ConversationState>>) -> Result<(f32, f32), String> {
    Ok((conv_state.mic_level(), conv_state.them_level()))
}

#[tauri::command]
pub fn is_conversation_active(conv_state: State<'_, Arc<ConversationState>>) -> Result<bool, String> {
    Ok(conv_state.is_active())
}

/// Surfaces a capture-thread setup/stream error (e.g. no loopback-capable
/// output device) so the frontend can show it instead of silently sitting
/// on an empty transcript.
#[tauri::command]
pub fn get_conversation_error(conv_state: State<'_, Arc<ConversationState>>) -> Result<Option<String>, String> {
    Ok(conv_state.get_error())
}

#[tauri::command]
pub fn list_conversations(db: State<'_, Database>, limit: u32, offset: u32) -> Result<Vec<ConversationSummary>, String> {
    db.list_conversations(limit, offset).str_err()
}

#[tauri::command]
pub fn get_conversation(db: State<'_, Database>, conversation_id: i64) -> Result<ConversationDetail, String> {
    db.get_conversation(conversation_id).str_err()
}

#[tauri::command]
pub fn delete_conversation(db: State<'_, Database>, conversation_id: i64) -> Result<(), String> {
    db.delete_conversation(conversation_id).str_err()
}

/// Build a suggested reply from a persona's system prompt plus the full
/// transcript so far, using the same reasoning pipeline enhancement uses.
/// Fired either automatically on turn-end (loopback VAD hangover, driven
/// from the frontend today) or on demand via the always-live hotkey.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn generate_suggestion(
    app: AppHandle,
    db: State<'_, Database>,
    conversation_id: i64,
    persona_system_prompt: String,
    persona_name: Option<String>,
    model: String,
    provider: String,
    api_key: String,
) -> Result<String, String> {
    let detail = db.get_conversation(conversation_id).str_err()?;

    if detail.utterances.is_empty() {
        return Err("No transcript yet".to_string());
    }

    let transcript = detail
        .utterances
        .iter()
        .map(|u| format!("{}: {}", if u.channel == "me" { "Me" } else { "Them" }, u.text))
        .collect::<Vec<_>>()
        .join("\n");

    let req = ReasoningRequest {
        text: format!("[CONVERSATION_TRANSCRIPT]\n{}", transcript),
        model,
        provider,
        system_prompt: persona_system_prompt,
        api_key,
        max_tokens: Some(300),
        temperature: Some(0.6),
    };

    let response = reasoning::process(&req).await.map_err(|e| e.to_string())?;
    let created_at_ms = now_ms();

    let suggestion_id = db
        .insert_conversation_suggestion(conversation_id, created_at_ms, persona_name.as_deref(), &response.text)
        .str_err()?;

    #[derive(Clone, serde::Serialize)]
    struct SuggestionPayload {
        id: i64,
        conversation_id: i64,
        created_at_ms: i64,
        persona_name: Option<String>,
        text: String,
    }
    let _ = app.emit(
        "conversation-suggestion",
        SuggestionPayload {
            id: suggestion_id,
            conversation_id,
            created_at_ms,
            persona_name,
            text: response.text.clone(),
        },
    );

    Ok(response.text)
}
