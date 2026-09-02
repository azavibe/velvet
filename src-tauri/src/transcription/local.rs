use std::io::Cursor;
use std::path::Path;

use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, get_lang_str,
};

#[derive(Default)]
pub struct LocalTranscriptionState;

fn decode_wav(audio_data: &[u8]) -> Result<Vec<f32>, String> {
    let mut reader = hound::WavReader::new(Cursor::new(audio_data))
        .map_err(|_| "local_audio_invalid_wav".to_string())?;
    let spec = reader.spec();
    if spec.channels != 1 || spec.sample_rate != 16_000 {
        return Err("local_audio_must_be_mono_16khz".to_string());
    }
    match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 16) => reader
            .samples::<i16>()
            .map(|sample| {
                sample
                    .map(|value| value as f32 / i16::MAX as f32)
                    .map_err(|_| "local_audio_invalid_samples".to_string())
            })
            .collect(),
        (hound::SampleFormat::Float, 32) => reader
            .samples::<f32>()
            .map(|sample| sample.map_err(|_| "local_audio_invalid_samples".to_string()))
            .collect(),
        _ => Err("local_audio_unsupported_format".to_string()),
    }
}

impl LocalTranscriptionState {
    pub fn transcribe(
        &self,
        model_path: &Path,
        audio_data: &[u8],
        language: Option<&str>,
        prompt: Option<&str>,
    ) -> Result<(String, Option<String>), String> {
        let samples = decode_wav(audio_data)?;
        // Load per request so the GGML weights are released as soon as this
        // transcription finishes. Keeping the context cached retained
        // hundreds of MB indefinitely after a single local dictation.
        let context =
            WhisperContext::new_with_params(model_path, WhisperContextParameters::default())
                .map_err(|error| format!("local_model_load_failed: {error}"))?;
        let mut state = context
            .create_state()
            .map_err(|error| format!("local_state_create_failed: {error}"))?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        // Local transcription is compute-heavy. Use at most half the logical
        // CPUs (capped at four) so dictation does not monopolize the machine.
        let threads = std::thread::available_parallelism()
            .map(|count| (count.get() / 2).clamp(1, 4) as i32)
            .unwrap_or(2);
        params.set_n_threads(threads);
        params.set_translate(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_print_special(false);
        params.set_language(language.filter(|value| *value != "auto"));
        if let Some(prompt) = prompt.filter(|value| !value.trim().is_empty()) {
            params.set_initial_prompt(prompt);
        }
        state
            .full(params, &samples)
            .map_err(|error| format!("local_transcription_failed: {error}"))?;

        let mut text = String::new();
        for segment in state.as_iter() {
            text.push_str(
                &segment
                    .to_str_lossy()
                    .map_err(|error| format!("local_transcription_text_failed: {error}"))?,
            );
        }
        let detected_language = get_lang_str(state.full_lang_id_from_state()).map(str::to_string);
        let text = text.trim().to_string();
        drop(state);
        drop(context);
        Ok((text, detected_language))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_wav_audio() {
        assert_eq!(
            decode_wav(b"not a wav").unwrap_err(),
            "local_audio_invalid_wav"
        );
    }
}
