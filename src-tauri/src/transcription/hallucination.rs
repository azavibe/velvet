//! Filter for canonical Whisper silence-hallucination phrases.
//!
//! On silent or noise-only audio, Whisper-family models emit fixed
//! subtitle-credit phrases learned from training data ("Thank you for
//! watching", "字幕由Amara.org社区提供", …). Matching is exact on a normalized
//! form (lowercased, letters/digits only) and applies to the whole output only
//! — a phrase embedded inside real speech is never stripped. Deliberately
//! excluded: bare "thank you" / "you", which are plausible real dictations;
//! the recorder's silence gate handles the dead-audio case that produces them.

/// Known hallucination phrases, pre-normalized (lowercase, alphanumeric only).
const HALLUCINATION_PHRASES: &[&str] = &[
    // English
    "thankyouforwatching",
    "thanksforwatching",
    "thankyousomuchforwatching",
    "pleasesubscribe",
    "pleaselikeandsubscribe",
    "dontforgettolikeandsubscribe",
    "subtitlesbytheamaraorgcommunity",
    "subtitlesbyamaraorg",
    "hellohowcaniassistyoutoday",
    "howcaniassistyoutoday",
    // Chinese
    "谢谢观看",
    "感谢观看",
    "字幕由amaraorg社区提供",
    "由amaraorg社区提供的字幕",
    "请不吝点赞订阅转发打赏支持明镜与点点栏目",
    "明镜与点点栏目",
    "优优独播剧场youkucom",
    // Japanese
    "ご視聴ありがとうございました",
    "ご清聴ありがとうございました",
    "チャンネル登録をお願いいたします",
    // Korean
    "시청해주셔서감사합니다",
    "구독과좋아요부탁드립니다",
    "mbc뉴스이덕영입니다",
];

/// Outputs longer than this (normalized chars) are never checked — real
/// dictation, not a credit-phrase loop.
const MAX_CHECK_CHARS: usize = 200;

fn normalize(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// True when `norm` is `phrase` repeated one or more times back-to-back
/// (Whisper often loops a credit phrase: "谢谢观看。谢谢观看。").
fn is_repetition_of(norm: &str, phrase: &str) -> bool {
    !phrase.is_empty()
        && norm.len().is_multiple_of(phrase.len())
        && norm
            .as_bytes()
            .chunks(phrase.len())
            .all(|c| c == phrase.as_bytes())
}

/// True when the entire output is a known hallucination phrase (or that phrase
/// repeated). Call after echo stripping; the caller blanks the text so the
/// frontend's empty-transcription skip applies.
pub fn is_known_hallucination(text: &str) -> bool {
    let norm = normalize(text);
    if norm.is_empty() || norm.chars().count() > MAX_CHECK_CHARS {
        return false;
    }
    HALLUCINATION_PHRASES
        .iter()
        .any(|p| is_repetition_of(&norm, p))
}

/// True when whisper.cpp returned only a decoder control token or a
/// non-speech caption. These are metadata, not words the user dictated, and
/// must not be sent into correction/enhancement as if they were speech.
pub fn is_non_speech_marker(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    if let Some(timestamp) = trimmed
        .strip_prefix("[_TT_")
        .and_then(|value| value.strip_suffix(']'))
    {
        return !timestamp.is_empty() && timestamp.chars().all(|ch| ch.is_ascii_digit());
    }
    if trimmed.starts_with("<|") && trimmed.ends_with("|>") {
        return true;
    }
    let caption = trimmed
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .or_else(|| {
            trimmed
                .strip_prefix('(')
                .and_then(|value| value.strip_suffix(')'))
        })
        .map(str::trim)
        .map(str::to_ascii_lowercase);
    matches!(
        caption.as_deref(),
        Some("blank_audio" | "silence" | "music" | "applause" | "inaudible" | "background noise")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_phrase_matches() {
        assert!(is_known_hallucination("Thank you for watching."));
        assert!(is_known_hallucination("Thanks for watching!"));
        assert!(is_known_hallucination("Hello! How can I assist you today?"));
        assert!(is_known_hallucination("谢谢观看"));
        assert!(is_known_hallucination("字幕由Amara.org社区提供"));
        assert!(is_known_hallucination("ご視聴ありがとうございました"));
        assert!(is_known_hallucination("시청해주셔서 감사합니다."));
    }

    #[test]
    fn repeated_phrase_matches() {
        assert!(is_known_hallucination("谢谢观看。谢谢观看。"));
        assert!(is_known_hallucination(
            "Thank you for watching. Thank you for watching. Thank you for watching."
        ));
        assert!(is_known_hallucination(
            "How can I assist you today? How can I assist you today?"
        ));
    }

    #[test]
    fn phrase_embedded_in_real_speech_is_kept() {
        assert!(!is_known_hallucination(
            "At the end of the video I always say thank you for watching to my viewers."
        ));
        assert!(!is_known_hallucination("我每次都会说谢谢观看然后结束直播"));
    }

    #[test]
    fn ordinary_sentences_are_kept() {
        assert!(!is_known_hallucination("Let's meet tomorrow at noon."));
        assert!(!is_known_hallucination("今天天气很好，我们去公园吧。"));
        assert!(!is_known_hallucination("Thank you."));
        assert!(!is_known_hallucination("you"));
    }

    #[test]
    fn long_output_is_never_checked() {
        let long = "谢谢观看".repeat(60);
        assert!(!is_known_hallucination(&long));
    }

    #[test]
    fn empty_output_is_not_a_hallucination() {
        assert!(!is_known_hallucination(""));
        assert!(!is_known_hallucination("   "));
    }

    #[test]
    fn decoder_tokens_and_non_speech_captions_are_not_transcripts() {
        for marker in [
            "[_TT_654]",
            "<|endoftext|>",
            "[BLANK_AUDIO]",
            "[Music]",
            "(silence)",
        ] {
            assert!(
                is_non_speech_marker(marker),
                "marker was retained: {marker}"
            );
        }
        assert!(!is_non_speech_marker("Meet me at [Home] tomorrow."));
        assert!(!is_non_speech_marker("[Aral]"));
    }
}
