use super::{ResultExt, recordings_dir};
use crate::audio::archive;
use crate::database::{AudioAssetStatus, Database};
use crate::transcription;
use serde::Serialize;
use std::time::Duration;
use tauri::{AppHandle, State};

const COMMAND_RETRY_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_COMMAND_PROMPT_CHARS: usize = 2_048;

fn supports_command_retry(provider: &str) -> bool {
    matches!(provider, "openai" | "groq" | "mistral" | "openrouter")
}

fn validate_command_retry_prompt(prompt: &str) -> Result<(), String> {
    if prompt.trim().is_empty() || prompt.chars().count() > MAX_COMMAND_PROMPT_CHARS {
        Err("invalid_command_retry_prompt".to_owned())
    } else {
        Ok(())
    }
}

/// Normalize locale codes like "en-US" to ISO 639-1 "en" for transcription APIs.
fn normalize_language(lang: Option<String>) -> Option<String> {
    lang.map(|l| l.split('-').next().unwrap_or(&l).to_string())
}

/// Result of a transcription Tauri command. `text` is the post-processed
/// (Simplified Chinese, full-width punctuation) output. `detected_language` is
/// the resolved language output by `resolve_language` — in bilingual mode it is
/// script/detection evidence constrained to the pair (`None` when
/// inconclusive), in single/auto mode the chosen/detected language. The
/// frontend forwards this into the subsequent reasoning call so AI enhancement
/// runs with the resolved language, or with the language-preserving auto
/// instruction when `None`.
#[derive(Debug, Serialize)]
pub struct TranscriptionResult {
    pub text: String,
    pub detected_language: Option<String>,
}

/// Script family a language or character belongs to, for resolving which of a
/// bilingual pair a transcript is written in. Latin-script languages are
/// indistinguishable from each other at this level.
#[derive(Clone, Copy, PartialEq)]
enum Script {
    Han,
    Kana,
    Hangul,
    Cyrillic,
    Arabic,
    Hebrew,
    Greek,
    Thai,
    Devanagari,
    Latin,
}

fn script_class(lang: &str) -> Script {
    let base = lang.split(['-', '_']).next().unwrap_or(lang);
    match base.to_ascii_lowercase().as_str() {
        "zh" | "yue" => Script::Han,
        "ja" => Script::Kana,
        "ko" => Script::Hangul,
        "ru" | "uk" | "be" | "bg" | "sr" | "mk" => Script::Cyrillic,
        "ar" | "fa" | "ur" => Script::Arabic,
        "he" => Script::Hebrew,
        "el" => Script::Greek,
        "th" => Script::Thai,
        "hi" | "mr" | "ne" => Script::Devanagari,
        _ => Script::Latin,
    }
}

fn char_script(c: char) -> Option<Script> {
    match c as u32 {
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF => Some(Script::Han),
        0x3040..=0x30FF => Some(Script::Kana),
        0x1100..=0x11FF | 0xAC00..=0xD7AF => Some(Script::Hangul),
        0x0400..=0x04FF => Some(Script::Cyrillic),
        0x0600..=0x06FF | 0x0750..=0x077F => Some(Script::Arabic),
        0x0590..=0x05FF => Some(Script::Hebrew),
        0x0370..=0x03FF => Some(Script::Greek),
        0x0E00..=0x0E7F => Some(Script::Thai),
        0x0900..=0x097F => Some(Script::Devanagari),
        _ if c.is_alphabetic() => Some(Script::Latin),
        _ => None,
    }
}

/// Which of the bilingual pair the transcript's script indicates.
///
/// The transcript itself is ground truth for what the ASR emitted, so script
/// evidence outranks the provider's language ID (which mis-fires on short
/// clips). CJK chars weigh 3× — one Han char carries roughly a Latin word — and
/// a pair language wins at ≥50% of the weighted letter mass. `None` when the
/// pair shares a script (en+de) or the mix is inconclusive.
fn script_language<'a>(text: &str, primary: &'a str, secondary: &'a str) -> Option<&'a str> {
    let pc = script_class(primary);
    let sc = script_class(secondary);
    if pc == sc {
        return None;
    }

    // ja+zh pair: Han appears in both scripts; kana is the discriminator
    // (Japanese text virtually always contains kana).
    if (pc == Script::Kana && sc == Script::Han) || (pc == Script::Han && sc == Script::Kana) {
        let has_kana = text.chars().any(|c| char_script(c) == Some(Script::Kana));
        let has_han = text.chars().any(|c| char_script(c) == Some(Script::Han));
        let kana_lang = if pc == Script::Kana {
            primary
        } else {
            secondary
        };
        let han_lang = if pc == Script::Han {
            primary
        } else {
            secondary
        };
        return match (has_kana, has_han) {
            (true, _) => Some(kana_lang),
            (false, true) => Some(han_lang),
            (false, false) => None,
        };
    }

    // A char counts toward a language when it is in that language's script;
    // Han also counts toward Japanese (kanji inside Japanese text).
    let counts_toward =
        |cs: Script, class: Script| cs == class || (class == Script::Kana && cs == Script::Han);
    let weight = |cs: Script| match cs {
        Script::Han | Script::Kana | Script::Hangul => 3.0_f32,
        _ => 1.0,
    };

    let (mut total, mut p_count, mut s_count) = (0.0_f32, 0.0_f32, 0.0_f32);
    for c in text.chars() {
        if let Some(cs) = char_script(c) {
            let w = weight(cs);
            total += w;
            if counts_toward(cs, pc) {
                p_count += w;
            }
            if counts_toward(cs, sc) {
                s_count += w;
            }
        }
    }
    if total == 0.0 {
        return None;
    }
    if p_count >= total * 0.5 && p_count > s_count {
        Some(primary)
    } else if s_count >= total * 0.5 && s_count > p_count {
        Some(secondary)
    } else {
        None
    }
}

/// Resolve the effective language used for post-processing (T→S) and reported
/// back to the frontend (which forwards it to AI enhancement).
///
/// - **Bilingual** (`secondary` is `Some`): the transcript's script decides
///   first (`script_language`); otherwise a detected language inside the pair
///   is kept. Anything else resolves to `None` — never the old snap-to-primary,
///   which handed enhancement a language the user may not have spoken and made
///   it translate (e.g. Chinese dictation rewritten in English because "en"
///   sat in the first slot). `None` lets T→S fall back to its kana heuristic
///   and enhancement fall back to the language-preserving auto instruction.
/// - **Auto / Single** (`secondary` is `None`): unchanged — auto/empty prefers
///   the detected language, an explicit language wins outright.
fn resolve_language(
    primary: Option<&str>,
    secondary: Option<&str>,
    detected: Option<&str>,
    text: &str,
) -> Option<String> {
    match secondary {
        Some(sec) => {
            let p = primary.unwrap_or("");
            if let Some(by_script) = script_language(text, p, sec) {
                return Some(by_script.to_string());
            }
            match detected {
                Some(d) if d == p || d == sec => Some(d.to_string()),
                _ => None,
            }
        }
        None => match primary {
            None | Some("auto") => detected.map(str::to_string),
            Some(other) => Some(other.to_string()),
        },
    }
}

/// Per-request language parameters shared by both buffered transcription
/// commands: the normalized primary/secondary codes, the conditioning prompt
/// (bilingual when both are set, else single/auto), and the `engine_lang` handed
/// to the model — `None` in bilingual mode so it auto-detects within the pair,
/// otherwise the user's choice.
fn prepare_language_params(
    language: Option<String>,
    secondary_language: Option<String>,
    dictionary: &[String],
) -> (Option<String>, Option<String>, String, Option<String>) {
    let primary = normalize_language(language);
    let secondary = normalize_language(secondary_language);
    // Bilingual: prime BOTH languages and let the model detect within the pair
    // (no forced -l). Otherwise keep the single/auto prompt.
    let prompt = match (primary.as_deref(), secondary.as_deref()) {
        (Some(p), Some(s)) => crate::transcription::build_bilingual_prompt(dictionary, p, s),
        _ => crate::transcription::build_prompt(dictionary, primary.as_deref()),
    };
    // In bilingual mode never force a language; auto/single pass the choice through.
    let engine_lang = match secondary.as_deref() {
        Some(_) => None,
        None => primary.clone(),
    };
    (primary, secondary, prompt, engine_lang)
}

#[tauri::command]
pub async fn transcribe_cloud(
    audio_data: Vec<u8>,
    provider: String,
    api_key: String,
    model: String,
    language: Option<String>,
    secondary_language: Option<String>,
    dictionary: Vec<String>,
    protected_terms: Vec<String>,
) -> Result<TranscriptionResult, String> {
    // Never log any part of the API key — even a 4-char prefix/suffix can leak
    // into shipped logs or bug reports. Log only whether a key is present.
    log::info!(
        "[Agenda] Transcribing: provider={}, model={}, has_key={}",
        provider,
        model,
        !api_key.is_empty()
    );

    let (primary, secondary, prompt, engine_lang) =
        prepare_language_params(language, secondary_language, &dictionary);

    let output = match provider.as_str() {
        "openai" => transcription::cloud::transcribe_openai(
            audio_data,
            &api_key,
            &model,
            engine_lang.as_deref(),
            Some(prompt.as_str()),
            None,
            None,
        )
        .await
        .str_err()?,

        "groq" => transcription::cloud::transcribe_groq(
            audio_data,
            &api_key,
            &model,
            engine_lang.as_deref(),
            Some(prompt.as_str()),
            None,
        )
        .await
        .str_err()?,

        "qwen" => transcription::cloud::transcribe_qwen(audio_data, &api_key, &model)
            .await
            .str_err()?,

        "mistral" => transcription::cloud::transcribe_mistral(
            audio_data,
            &api_key,
            &model,
            engine_lang.as_deref(),
            Some(prompt.as_str()),
            None,
        )
        .await
        .str_err()?,

        "openrouter" => transcription::cloud::transcribe_openrouter(
            audio_data,
            &api_key,
            &model,
            engine_lang.as_deref(),
            Some(prompt.as_str()),
            None,
        )
        .await
        .str_err()?,

        other => return Err(format!("Unknown transcription provider: {}", other)),
    };

    let stripped = transcription::cloud::strip_prompt_echo(
        &output.text,
        Some(prompt.as_str()),
        &protected_terms,
    );
    let stripped =
        transcription::cloud::strip_dictionary_edge_echo(&stripped, &dictionary, &protected_terms);
    // Whole-output hallucination phrases (silence artifacts) are blanked; the
    // frontend's empty-transcription check then skips the result silently.
    let stripped = if transcription::hallucination::is_known_hallucination(&stripped)
        || transcription::hallucination::is_non_speech_marker(&stripped)
    {
        log::info!("[Agenda] Dropped known hallucination phrase output");
        String::new()
    } else {
        stripped
    };
    let resolved = resolve_language(
        primary.as_deref(),
        secondary.as_deref(),
        output.detected_language.as_deref(),
        &stripped,
    );
    let text = transcription::finalize_chinese_text(&stripped, resolved.as_deref());
    Ok(TranscriptionResult {
        text,
        detected_language: resolved,
    })
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn transcribe_local(
    app: AppHandle,
    state: State<'_, std::sync::Arc<transcription::local::LocalTranscriptionState>>,
    audio_data: Vec<u8>,
    model: String,
    language: Option<String>,
    secondary_language: Option<String>,
    dictionary: Vec<String>,
    protected_terms: Vec<String>,
) -> Result<TranscriptionResult, String> {
    let model_path = super::local_models::installed_model_path(&app, &model)?;
    let (primary, secondary, prompt, engine_lang) =
        prepare_language_params(language, secondary_language, &dictionary);
    let state = state.inner().clone();
    let inference_prompt = prompt.clone();
    let output = tauri::async_runtime::spawn_blocking(move || {
        state.transcribe(
            &model_path,
            &audio_data,
            engine_lang.as_deref(),
            Some(inference_prompt.as_str()),
        )
    })
    .await
    .map_err(|_| "local_transcription_task_failed".to_string())??;

    let stripped =
        transcription::cloud::strip_prompt_echo(&output.0, Some(prompt.as_str()), &protected_terms);
    let stripped =
        transcription::cloud::strip_dictionary_edge_echo(&stripped, &dictionary, &protected_terms);
    let stripped = if transcription::hallucination::is_known_hallucination(&stripped)
        || transcription::hallucination::is_non_speech_marker(&stripped)
    {
        String::new()
    } else {
        stripped
    };
    let resolved = resolve_language(
        primary.as_deref(),
        secondary.as_deref(),
        output.1.as_deref(),
        &stripped,
    );
    Ok(TranscriptionResult {
        text: transcription::finalize_chinese_text(&stripped, resolved.as_deref()),
        detected_language: resolved,
    })
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn retry_failed_transcription(
    app: AppHandle,
    db: State<'_, Database>,
    transcription_id: i64,
    audio_asset_id: i64,
    provider: String,
    api_key: String,
    model: String,
    language: Option<String>,
    secondary_language: Option<String>,
    dictionary: Vec<String>,
    protected_terms: Vec<String>,
) -> Result<TranscriptionResult, String> {
    let asset = db
        .get_audio_asset(audio_asset_id)
        .str_err()?
        .ok_or_else(|| "retry_audio_not_found".to_string())?;
    if asset.owner_type != "dictation"
        || asset.owner_id != transcription_id
        || asset.channel != "main"
        || asset.status != AudioAssetStatus::Ready
    {
        return Err("retry_audio_not_ready".to_string());
    }
    let raw_path = asset
        .path
        .as_deref()
        .ok_or_else(|| "retry_audio_not_ready".to_string())?;
    let root = recordings_dir(&app)?;
    let path = archive::validated_recording_path(&root, raw_path)
        .ok_or_else(|| "retry_audio_invalid".to_string())?;
    let audio_data = tauri::async_runtime::spawn_blocking(move || std::fs::read(path))
        .await
        .map_err(|_| "retry_audio_read_failed".to_string())?
        .map_err(|_| "retry_audio_read_failed".to_string())?;

    let result = transcribe_cloud(
        audio_data,
        provider.clone(),
        api_key,
        model,
        language,
        secondary_language,
        dictionary,
        protected_terms,
    )
    .await?;
    if result.text.trim().is_empty() {
        return Err("empty_transcription".to_string());
    }
    db.complete_transcription_retry(transcription_id, &result.text, &provider)
        .str_err()?;
    Ok(result)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn retranscribe_local(
    app: AppHandle,
    db: State<'_, Database>,
    local_state: State<'_, std::sync::Arc<transcription::local::LocalTranscriptionState>>,
    transcription_id: i64,
    audio_asset_id: i64,
    model: String,
    language: Option<String>,
    secondary_language: Option<String>,
    dictionary: Vec<String>,
    protected_terms: Vec<String>,
) -> Result<TranscriptionResult, String> {
    let asset = db
        .get_audio_asset(audio_asset_id)
        .str_err()?
        .ok_or_else(|| "retry_audio_not_found".to_string())?;
    if asset.owner_type != "dictation"
        || asset.owner_id != transcription_id
        || asset.channel != "main"
        || asset.status != AudioAssetStatus::Ready
    {
        return Err("retry_audio_not_ready".to_string());
    }
    let raw_path = asset
        .path
        .as_deref()
        .ok_or_else(|| "retry_audio_not_ready".to_string())?;
    let root = recordings_dir(&app)?;
    let path = archive::validated_recording_path(&root, raw_path)
        .ok_or_else(|| "retry_audio_invalid".to_string())?;
    let audio_data = tauri::async_runtime::spawn_blocking(move || std::fs::read(path))
        .await
        .map_err(|_| "retry_audio_read_failed".to_string())?
        .map_err(|_| "retry_audio_read_failed".to_string())?;
    let result = transcribe_local(
        app,
        local_state,
        audio_data,
        model,
        language,
        secondary_language,
        dictionary,
        protected_terms,
    )
    .await?;
    if result.text.trim().is_empty() {
        return Err("empty_transcription".to_string());
    }
    db.replace_transcription(transcription_id, &result.text, "retry:local")
        .str_err()?;
    Ok(result)
}

/// One command-scoped retranscription over the original in-memory recording.
/// Unsupported providers return `None`; timeout and request failures are
/// nonfatal at the frontend resolver boundary and never create database rows.
#[tauri::command]
pub async fn transcribe_command_retry(
    audio_data: Vec<u8>,
    provider: String,
    api_key: String,
    model: String,
    language: Option<String>,
    prompt: String,
) -> Result<Option<TranscriptionResult>, String> {
    validate_command_retry_prompt(&prompt)?;
    if !supports_command_retry(&provider) {
        #[cfg(debug_assertions)]
        log::debug!("[voice-command] code=retry_unsupported_provider");
        return Ok(None);
    }

    let language = normalize_language(language);
    #[cfg(debug_assertions)]
    log::debug!("[voice-command] code=retry_request_started provider={provider}");
    let request = async {
        match provider.as_str() {
            "openai" => {
                transcription::cloud::transcribe_openai(
                    audio_data,
                    &api_key,
                    &model,
                    language.as_deref(),
                    Some(&prompt),
                    None,
                    Some(0.0),
                )
                .await
            }
            "groq" => {
                transcription::cloud::transcribe_groq(
                    audio_data,
                    &api_key,
                    &model,
                    language.as_deref(),
                    Some(&prompt),
                    Some(0.0),
                )
                .await
            }
            "mistral" => {
                transcription::cloud::transcribe_mistral(
                    audio_data,
                    &api_key,
                    &model,
                    language.as_deref(),
                    Some(&prompt),
                    Some(0.0),
                )
                .await
            }
            "openrouter" => {
                transcription::cloud::transcribe_openrouter(
                    audio_data,
                    &api_key,
                    &model,
                    language.as_deref(),
                    Some(&prompt),
                    Some(0.0),
                )
                .await
            }
            _ => unreachable!("provider support checked above"),
        }
    };

    let output = tokio::time::timeout(COMMAND_RETRY_TIMEOUT, request)
        .await
        .map_err(|_| "command_retry_timeout".to_owned())?
        .str_err()?;
    #[cfg(debug_assertions)]
    log::debug!("[voice-command] code=retry_request_completed");
    let text = transcription::finalize_chinese_text(&output.text, language.as_deref());
    Ok(Some(TranscriptionResult {
        text,
        detected_language: output.detected_language,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_retry_support_is_explicit_and_prompt_is_bounded() {
        for provider in ["openai", "groq", "mistral", "openrouter"] {
            assert!(supports_command_retry(provider));
        }
        assert!(!supports_command_retry("qwen"));
        assert!(!supports_command_retry("unknown"));
        assert!(validate_command_retry_prompt("Wake: Agent. Commands: notes.").is_ok());
        assert!(validate_command_retry_prompt("  ").is_err());
        assert!(validate_command_retry_prompt(&"x".repeat(MAX_COMMAND_PROMPT_CHARS + 1)).is_err());
    }

    #[test]
    fn resolve_keeps_in_set_detection_without_script_evidence() {
        // Bilingual: no script evidence (empty text) → an in-pair detection is kept.
        assert_eq!(
            resolve_language(Some("zh"), Some("en"), Some("en"), ""),
            Some("en".to_string())
        );
        assert_eq!(
            resolve_language(Some("zh"), Some("en"), Some("zh"), ""),
            Some("zh".to_string())
        );
    }

    #[test]
    fn resolve_script_evidence_overrides_wrong_detection() {
        // A Chinese transcript with a mis-detected in-pair "en" resolves to zh —
        // the transcript's script is ground truth.
        assert_eq!(
            resolve_language(
                Some("en"),
                Some("zh"),
                Some("en"),
                "今天天气很好，我们去公园吧"
            ),
            Some("zh".to_string())
        );
    }

    #[test]
    fn resolve_script_evidence_picks_latin_peer() {
        assert_eq!(
            resolve_language(Some("en"), Some("zh"), None, "Let's meet tomorrow at noon"),
            Some("en".to_string())
        );
    }

    #[test]
    fn resolve_cjk_weighting_keeps_chinese_with_embedded_latin_term() {
        assert_eq!(
            resolve_language(Some("en"), Some("zh"), None, "我用 TypeScript 写代码"),
            Some("zh".to_string())
        );
    }

    #[test]
    fn resolve_out_of_set_detection_is_unresolved() {
        // Bilingual: a third-language detection no longer snaps to primary —
        // that snap injected an unspoken language into enhancement (translation bug).
        assert_eq!(
            resolve_language(Some("zh"), Some("en"), Some("ko"), ""),
            None
        );
    }

    #[test]
    fn resolve_no_detection_no_script_is_unresolved() {
        assert_eq!(resolve_language(Some("zh"), Some("en"), None, ""), None);
    }

    #[test]
    fn resolve_same_script_pair_uses_detection_only() {
        // en+de share the Latin script — script evidence can't discriminate.
        assert_eq!(
            resolve_language(Some("de"), Some("fr"), Some("fr"), "Bonjour tout le monde"),
            Some("fr".to_string())
        );
        assert_eq!(
            resolve_language(Some("de"), Some("fr"), None, "hello"),
            None
        );
    }

    #[test]
    fn resolve_ja_zh_pair_discriminates_on_kana() {
        assert_eq!(
            resolve_language(Some("ja"), Some("zh"), None, "ありがとうございます"),
            Some("ja".to_string())
        );
        assert_eq!(
            resolve_language(Some("ja"), Some("zh"), None, "谢谢你的帮助"),
            Some("zh".to_string())
        );
    }

    #[test]
    fn resolve_non_bilingual_matches_legacy_behavior() {
        // secondary = None → auto/empty prefers detection, explicit wins outright.
        assert_eq!(
            resolve_language(Some("auto"), None, Some("zh"), ""),
            Some("zh".to_string())
        );
        assert_eq!(
            resolve_language(None, None, Some("ja"), ""),
            Some("ja".to_string())
        );
        assert_eq!(
            resolve_language(Some("ja"), None, Some("zh"), ""),
            Some("ja".to_string())
        );
        assert_eq!(resolve_language(Some("auto"), None, None, ""), None);
    }
}
