//! Secure, bounded range serving for archived WAV assets.
//!
//! The frontend receives an opaque asset URL rather than a filesystem path.
//! Every request looks the asset up again, validates its owner and canonical
//! path, and reads at most one bounded range into the protocol response.

use crate::audio::archive;
use crate::commands::recordings_dir;
use crate::database::{AudioAsset, AudioAssetStatus, Database};
use serde::Serialize;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use tauri::http::{
    Method, Request, Response, StatusCode,
    header::{
        ACCEPT_RANGES, ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS,
        ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_EXPOSE_HEADERS, CACHE_CONTROL, CONTENT_LENGTH,
        CONTENT_RANGE, CONTENT_TYPE, RANGE,
    },
};
use tauri::{AppHandle, Manager, Runtime, State};

const AUDIO_PATH_PREFIX: &str = "/audio/";
const AUDIO_CONTENT_TYPE: &str = "audio/wav";
/// Keep every protocol response bounded. WebView2 asks for subsequent ranges
/// as needed, so seeking never requires a complete recording in memory.
pub(crate) const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AssetError {
    NotFound,
    NotReady,
    Missing,
    Failed,
    Invalid,
    Unsupported,
    Internal,
}

impl AssetError {
    fn status(self) -> StatusCode {
        match self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::NotReady => StatusCode::CONFLICT,
            Self::Missing => StatusCode::NOT_FOUND,
            Self::Failed => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Invalid => StatusCode::FORBIDDEN,
            Self::Unsupported => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn code(self) -> &'static str {
        match self {
            Self::NotFound => "audio_not_found",
            Self::NotReady => "audio_not_ready",
            Self::Missing => "audio_missing",
            Self::Failed => "audio_failed",
            Self::Invalid => "audio_invalid",
            Self::Unsupported => "audio_unsupported",
            Self::Internal => "audio_unavailable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ByteRange {
    start: u64,
    end: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RangeError {
    Invalid,
    Unsatisfiable,
}

#[derive(Debug, Clone, Serialize)]
pub struct AudioPlaybackSource {
    pub url: String,
    pub duration_ms: Option<i64>,
}

#[tauri::command]
pub fn get_audio_asset_url(
    app: AppHandle,
    db: State<'_, Database>,
    asset_id: i64,
) -> Result<AudioPlaybackSource, String> {
    let root = recordings_dir(&app).map_err(|_| AssetError::Internal.code().to_owned())?;
    let asset = db
        .get_audio_asset(asset_id)
        .map_err(|_| AssetError::Internal.code().to_owned())?
        .ok_or(AssetError::NotFound)
        .map_err(|error| error.code().to_owned())?;
    if let Err(error) = authorize_ready_asset(&root, &asset) {
        if error == AssetError::Missing {
            let _ = db.mark_audio_asset_missing(asset_id);
        }
        return Err(error.code().to_owned());
    }

    let origin = if cfg!(windows) {
        "http://recording.localhost"
    } else {
        "recording://localhost"
    };
    Ok(AudioPlaybackSource {
        url: format!("{origin}{AUDIO_PATH_PREFIX}{asset_id}"),
        duration_ms: asset.duration_ms,
    })
}

/// Handle one `recording://` request. The Tauri builder runs this function on
/// a worker thread because file metadata and range reads are synchronous.
pub fn serve_recording_request<R: Runtime>(
    app: &AppHandle<R>,
    request: Request<Vec<u8>>,
) -> Response<Vec<u8>> {
    if request.method() == Method::OPTIONS {
        return response_builder(StatusCode::NO_CONTENT)
            .header(ACCESS_CONTROL_ALLOW_METHODS, "GET, HEAD, OPTIONS")
            .header(ACCESS_CONTROL_ALLOW_HEADERS, "Range")
            .body(Vec::new())
            .expect("static recording response headers are valid");
    }

    if request.method() != Method::GET && request.method() != Method::HEAD {
        return empty_response(StatusCode::METHOD_NOT_ALLOWED);
    }

    let Some(asset_id) = asset_id_from_path(request.uri().path()) else {
        return empty_response(StatusCode::NOT_FOUND);
    };

    let root = match recordings_dir(app) {
        Ok(root) => root,
        Err(_) => return empty_response(AssetError::Internal.status()),
    };
    let db = app.state::<Database>();
    let asset = match db.get_audio_asset(asset_id) {
        Ok(Some(asset)) => asset,
        Ok(None) => return empty_response(AssetError::NotFound.status()),
        Err(_) => return empty_response(AssetError::Internal.status()),
    };
    let path = match authorize_ready_asset(&root, &asset) {
        Ok(path) => path,
        Err(error) => {
            if error == AssetError::Missing {
                let _ = db.mark_audio_asset_missing(asset_id);
            }
            return empty_response(error.status());
        }
    };

    serve_file(request, &path)
}

fn asset_id_from_path(path: &str) -> Option<i64> {
    let value = path.strip_prefix(AUDIO_PATH_PREFIX)?;
    if value.is_empty() || value.contains('/') || value.contains('\\') {
        return None;
    }
    value.parse::<i64>().ok().filter(|id| *id > 0)
}

fn allowed_asset(asset: &AudioAsset) -> bool {
    matches!(
        (asset.owner_type.as_str(), asset.channel.as_str()),
        ("dictation", "main")
            | ("conversation", "me")
            | ("conversation", "them")
            | ("note", "main")
    )
}

fn authorize_ready_asset(root: &Path, asset: &AudioAsset) -> Result<PathBuf, AssetError> {
    if !allowed_asset(asset) {
        return Err(AssetError::Invalid);
    }
    match asset.status {
        AudioAssetStatus::Saving => return Err(AssetError::NotReady),
        AudioAssetStatus::Missing => return Err(AssetError::Missing),
        AudioAssetStatus::Failed => return Err(AssetError::Failed),
        AudioAssetStatus::Ready => {}
    }

    let raw_path = asset.path.as_deref().ok_or(AssetError::Invalid)?;
    let path = archive::validated_recording_path(root, raw_path).ok_or(AssetError::Invalid)?;
    let metadata = fs::metadata(&path).map_err(|_| AssetError::Missing)?;
    if !metadata.is_file() {
        return Err(AssetError::Invalid);
    }
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        != Some("wav".to_owned())
    {
        return Err(AssetError::Unsupported);
    }

    let mut file = File::open(&path).map_err(|_| AssetError::Missing)?;
    let mut header = [0_u8; 12];
    file.read_exact(&mut header)
        .map_err(|_| AssetError::Unsupported)?;
    if &header[0..4] != b"RIFF" || &header[8..12] != b"WAVE" {
        return Err(AssetError::Unsupported);
    }

    Ok(path)
}

fn response_builder(status: StatusCode) -> tauri::http::response::Builder {
    Response::builder()
        .status(status)
        .header(ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header(
            ACCESS_CONTROL_EXPOSE_HEADERS,
            "Accept-Ranges, Content-Range, Content-Length, Content-Type",
        )
        .header(CACHE_CONTROL, "no-store")
        .header("X-Content-Type-Options", "nosniff")
}

fn empty_response(status: StatusCode) -> Response<Vec<u8>> {
    response_builder(status)
        .body(Vec::new())
        .expect("static recording response headers are valid")
}

fn range_not_satisfiable(length: u64) -> Response<Vec<u8>> {
    response_builder(StatusCode::RANGE_NOT_SATISFIABLE)
        .header(CONTENT_RANGE, format!("bytes */{length}"))
        .header(ACCEPT_RANGES, "bytes")
        .body(Vec::new())
        .expect("static recording response headers are valid")
}

pub(crate) fn serve_file(request: Request<Vec<u8>>, path: &Path) -> Response<Vec<u8>> {
    let length = match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => metadata.len(),
        _ => return empty_response(StatusCode::NOT_FOUND),
    };

    let requested_range = match request.headers().get(RANGE) {
        None => None,
        Some(value) => match value.to_str() {
            Ok(value) => Some(value),
            Err(_) => return range_not_satisfiable(length),
        },
    };

    // A metadata-only HEAD request can report the complete representation
    // length without reading or allocating it. This is the form WebView2 uses
    // while probing some media sources.
    if request.method() == Method::HEAD && requested_range.is_none() {
        return response_builder(StatusCode::OK)
            .header(CONTENT_TYPE, AUDIO_CONTENT_TYPE)
            .header(ACCEPT_RANGES, "bytes")
            .header(CONTENT_LENGTH, length)
            .body(Vec::new())
            .expect("static recording response headers are valid");
    }

    if length == 0 {
        return if requested_range.is_some() {
            range_not_satisfiable(length)
        } else {
            response_builder(StatusCode::OK)
                .header(CONTENT_TYPE, AUDIO_CONTENT_TYPE)
                .header(ACCEPT_RANGES, "bytes")
                .header(CONTENT_LENGTH, 0)
                .body(Vec::new())
                .expect("static recording response headers are valid")
        };
    }

    let (requested, partial) = match requested_range {
        Some(value) => match parse_range_header(value, length) {
            Ok(range) => (range, true),
            Err(_) => return range_not_satisfiable(length),
        },
        None => (
            ByteRange {
                start: 0,
                end: length - 1,
            },
            length > MAX_RESPONSE_BYTES,
        ),
    };
    let range = match bound_response_range(requested) {
        Some(range) => range,
        None => return range_not_satisfiable(length),
    };

    let range_length = range.end - range.start + 1;
    let status = if partial {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    let mut response = response_builder(status)
        .header(CONTENT_TYPE, AUDIO_CONTENT_TYPE)
        .header(ACCEPT_RANGES, "bytes")
        .header(CONTENT_LENGTH, range_length);
    if partial {
        response = response.header(
            CONTENT_RANGE,
            format!("bytes {}-{}/{length}", range.start, range.end),
        );
    }

    if request.method() == Method::HEAD {
        return response
            .body(Vec::new())
            .expect("static recording response headers are valid");
    }

    let body = match read_range(path, range) {
        Ok(body) => body,
        Err(_) => return empty_response(StatusCode::NOT_FOUND),
    };
    response
        .body(body)
        .expect("static recording response headers are valid")
}

fn read_range(path: &Path, range: ByteRange) -> std::io::Result<Vec<u8>> {
    let length = range
        .end
        .checked_sub(range.start)
        .and_then(|length| length.checked_add(1))
        .filter(|length| *length <= MAX_RESPONSE_BYTES)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "recording range exceeds response bound",
            )
        })?;
    let allocation = usize::try_from(length).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "recording range cannot be represented",
        )
    })?;
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(range.start))?;
    let mut body = vec![0_u8; allocation];
    file.read_exact(&mut body)?;
    Ok(body)
}

fn bound_response_range(range: ByteRange) -> Option<ByteRange> {
    if range.end < range.start || MAX_RESPONSE_BYTES == 0 {
        return None;
    }
    let max_end = range
        .start
        .saturating_add(MAX_RESPONSE_BYTES.saturating_sub(1));
    Some(ByteRange {
        start: range.start,
        end: range.end.min(max_end),
    })
}

fn parse_range_header(value: &str, length: u64) -> Result<ByteRange, RangeError> {
    if length == 0 {
        return Err(RangeError::Unsatisfiable);
    }
    let (unit, spec) = value.split_once('=').ok_or(RangeError::Invalid)?;
    if unit != "bytes"
        || spec.is_empty()
        || spec.contains(',')
        || spec.chars().any(char::is_whitespace)
    {
        return Err(RangeError::Invalid);
    }
    let (start_text, end_text) = spec.split_once('-').ok_or(RangeError::Invalid)?;
    if start_text.is_empty() {
        let suffix = end_text.parse::<u64>().map_err(|_| RangeError::Invalid)?;
        if suffix == 0 {
            return Err(RangeError::Unsatisfiable);
        }
        let start = length.saturating_sub(suffix);
        return Ok(ByteRange {
            start,
            end: length - 1,
        });
    }

    let start = start_text.parse::<u64>().map_err(|_| RangeError::Invalid)?;
    if start >= length {
        return Err(RangeError::Unsatisfiable);
    }
    let end = if end_text.is_empty() {
        length - 1
    } else {
        let end = end_text.parse::<u64>().map_err(|_| RangeError::Invalid)?;
        if end < start {
            return Err(RangeError::Unsatisfiable);
        }
        end.min(length - 1)
    };
    Ok(ByteRange { start, end })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_root(label: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("agenda-playback-{label}-{suffix}"));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn asset(path: Option<String>, status: AudioAssetStatus) -> AudioAsset {
        AudioAsset {
            id: 1,
            owner_type: "dictation".to_owned(),
            owner_id: 1,
            channel: "main".to_owned(),
            sequence: 0,
            status,
            path,
            byte_length: None,
            duration_ms: None,
            error: None,
        }
    }

    #[test]
    fn asset_authorization_rejects_non_ready_and_unrecognized_assets() {
        let root = test_root("authorization");
        let path = root.join("recording.wav");
        fs::write(&path, b"RIFF0000WAVEfmt ").unwrap();

        for status in [
            AudioAssetStatus::Saving,
            AudioAssetStatus::Missing,
            AudioAssetStatus::Failed,
        ] {
            assert_eq!(
                authorize_ready_asset(
                    &root,
                    &asset(Some(path.to_string_lossy().into_owned()), status)
                ),
                Err(match status {
                    AudioAssetStatus::Saving => AssetError::NotReady,
                    AudioAssetStatus::Missing => AssetError::Missing,
                    AudioAssetStatus::Failed => AssetError::Failed,
                    AudioAssetStatus::Ready => unreachable!(),
                })
            );
        }

        let mut unknown_owner = asset(
            Some(path.to_string_lossy().into_owned()),
            AudioAssetStatus::Ready,
        );
        unknown_owner.owner_type = "unknown".to_owned();
        assert_eq!(
            authorize_ready_asset(&root, &unknown_owner),
            Err(AssetError::Invalid)
        );

        let outside = root.parent().unwrap().join("outside.wav");
        fs::write(&outside, b"RIFF0000WAVEfmt ").unwrap();
        let escaped = asset(
            Some(outside.to_string_lossy().into_owned()),
            AudioAssetStatus::Ready,
        );
        assert_eq!(
            authorize_ready_asset(&root, &escaped),
            Err(AssetError::Invalid)
        );

        let _ = fs::remove_file(outside);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn asset_authorization_rejects_missing_and_unsupported_files() {
        let root = test_root("unsupported");
        let missing = asset(
            Some(root.join("missing.wav").to_string_lossy().into_owned()),
            AudioAssetStatus::Ready,
        );
        assert_eq!(
            authorize_ready_asset(&root, &missing),
            Err(AssetError::Missing)
        );

        let path = root.join("not-audio.wav");
        fs::write(&path, b"not a wav").unwrap();
        let unsupported = asset(
            Some(path.to_string_lossy().into_owned()),
            AudioAssetStatus::Ready,
        );
        assert_eq!(
            authorize_ready_asset(&root, &unsupported),
            Err(AssetError::Unsupported)
        );

        let wrong_extension = root.join("recording.bin");
        fs::write(&wrong_extension, b"RIFF0000WAVEfmt ").unwrap();
        let unsupported = asset(
            Some(wrong_extension.to_string_lossy().into_owned()),
            AudioAssetStatus::Ready,
        );
        assert_eq!(
            authorize_ready_asset(&root, &unsupported),
            Err(AssetError::Unsupported)
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn parses_valid_ranges_and_rejects_invalid_or_unsatisfiable_ranges() {
        assert_eq!(
            parse_range_header("bytes=0-3", 10),
            Ok(ByteRange { start: 0, end: 3 })
        );
        assert_eq!(
            parse_range_header("bytes=4-", 10),
            Ok(ByteRange { start: 4, end: 9 })
        );
        assert_eq!(
            parse_range_header("bytes=-4", 10),
            Ok(ByteRange { start: 6, end: 9 })
        );
        assert_eq!(
            parse_range_header("bytes=0-99", 10),
            Ok(ByteRange { start: 0, end: 9 })
        );

        for value in [
            "",
            "items=0-1",
            "bytes=0-1,2-3",
            "bytes=abc-1",
            "bytes=1-0",
            "bytes=10-",
            "bytes=-0",
            "bytes= 0-1",
            "bytes=0 -1",
            "bytes=0-1 ",
        ] {
            assert!(parse_range_header(value, 10).is_err(), "{value}");
        }
    }

    #[test]
    fn asset_urls_accept_only_positive_numeric_ids() {
        assert_eq!(asset_id_from_path("/audio/42"), Some(42));
        for path in [
            "/audio/",
            "/audio/0",
            "/audio/-1",
            "/audio/1/extra",
            "/audio/..%2f1",
            "/audio/1\\extra",
            "/other/1",
        ] {
            assert_eq!(asset_id_from_path(path), None, "{path}");
        }
    }

    #[test]
    fn large_open_ended_and_explicit_ranges_are_bounded_and_seekable() {
        let root = test_root("ranges");
        let path = root.join("large.wav");
        let contents: Vec<u8> = (0..(MAX_RESPONSE_BYTES as usize * 2 + 137))
            .map(|value| (value % 251) as u8)
            .collect();
        fs::write(&path, &contents).unwrap();
        let total = contents.len() as u64;

        let head = Request::builder()
            .method(Method::HEAD)
            .uri("/audio/1")
            .body(Vec::new())
            .unwrap();
        let response = serve_file(head, &path);
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(CONTENT_LENGTH).unwrap(),
            total.to_string().as_str()
        );
        assert_eq!(response.headers().get(ACCEPT_RANGES).unwrap(), "bytes");
        assert!(response.body().is_empty());

        let request = Request::builder()
            .method(Method::GET)
            .header(RANGE, "bytes=10-20")
            .uri("/audio/1")
            .body(Vec::new())
            .unwrap();
        let response = serve_file(request, &path);
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.body(), &contents[10..=20]);
        assert_eq!(
            response.headers().get(CONTENT_RANGE).unwrap(),
            format!("bytes 10-20/{total}").as_str()
        );

        let request = Request::builder()
            .method(Method::GET)
            .header(RANGE, "bytes=0-")
            .uri("/audio/1")
            .body(Vec::new())
            .unwrap();
        let response = serve_file(request, &path);
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers().get(CONTENT_RANGE).unwrap(),
            format!("bytes 0-{}/{total}", MAX_RESPONSE_BYTES - 1).as_str()
        );
        assert_eq!(response.body(), &contents[..MAX_RESPONSE_BYTES as usize]);

        let second_start = MAX_RESPONSE_BYTES;
        let request = Request::builder()
            .method(Method::GET)
            .header(RANGE, format!("bytes={second_start}-"))
            .uri("/audio/1")
            .body(Vec::new())
            .unwrap();
        let response = serve_file(request, &path);
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers().get(CONTENT_RANGE).unwrap(),
            format!(
                "bytes {second_start}-{}/{total}",
                second_start + MAX_RESPONSE_BYTES - 1
            )
            .as_str()
        );
        assert_eq!(response.body().len(), MAX_RESPONSE_BYTES as usize);

        let final_start = MAX_RESPONSE_BYTES * 2;
        let request = Request::builder()
            .method(Method::GET)
            .header(RANGE, format!("bytes={final_start}-"))
            .uri("/audio/1")
            .body(Vec::new())
            .unwrap();
        let response = serve_file(request, &path);
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.body(), &contents[final_start as usize..]);
        assert_eq!(
            response.headers().get(CONTENT_RANGE).unwrap(),
            format!("bytes {final_start}-{}/{total}", total - 1).as_str()
        );

        let near_end = total - 7;
        let request = Request::builder()
            .method(Method::GET)
            .header(RANGE, format!("bytes={near_end}-"))
            .uri("/audio/1")
            .body(Vec::new())
            .unwrap();
        let response = serve_file(request, &path);
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.body(), &contents[near_end as usize..]);

        let request = Request::builder()
            .method(Method::GET)
            .header(RANGE, format!("bytes=10-{}", total - 1))
            .uri("/audio/1")
            .body(Vec::new())
            .unwrap();
        let response = serve_file(request, &path);
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.body().len(), MAX_RESPONSE_BYTES as usize);
        assert_eq!(
            response.headers().get(CONTENT_RANGE).unwrap(),
            format!("bytes 10-{}/{total}", 10 + MAX_RESPONSE_BYTES - 1).as_str()
        );

        let request = Request::builder()
            .method(Method::GET)
            .header(RANGE, "bytes=-11")
            .uri("/audio/1")
            .body(Vec::new())
            .unwrap();
        let response = serve_file(request, &path);
        assert_eq!(response.body(), &contents[contents.len() - 11..]);

        let request = Request::builder()
            .method(Method::GET)
            .uri("/audio/1")
            .body(Vec::new())
            .unwrap();
        let response = serve_file(request, &path);
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.body().len(), MAX_RESPONSE_BYTES as usize);
        assert_eq!(
            response.headers().get(CONTENT_RANGE).unwrap(),
            format!("bytes 0-{}/{total}", MAX_RESPONSE_BYTES - 1).as_str()
        );

        for value in [
            format!("bytes={total}-"),
            "bytes=wat".to_owned(),
            "bytes=0-1,2-3".to_owned(),
        ] {
            let request = Request::builder()
                .method(Method::GET)
                .header(RANGE, &value)
                .uri("/audio/1")
                .body(Vec::new())
                .unwrap();
            let response = serve_file(request, &path);
            assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
            assert_eq!(
                response.headers().get(CONTENT_RANGE).unwrap(),
                format!("bytes */{total}").as_str()
            );
        }

        let invalid_header = tauri::http::HeaderValue::from_bytes(&[0xff]).unwrap();
        let request = Request::builder()
            .method(Method::GET)
            .header(RANGE, invalid_header)
            .uri("/audio/1")
            .body(Vec::new())
            .unwrap();
        assert_eq!(
            serve_file(request, &path).status(),
            StatusCode::RANGE_NOT_SATISFIABLE
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn empty_files_and_untrusted_direct_ranges_never_panic() {
        let root = test_root("empty");
        let path = root.join("empty.wav");
        fs::write(&path, []).unwrap();

        let head = Request::builder()
            .method(Method::HEAD)
            .uri("/audio/1")
            .body(Vec::new())
            .unwrap();
        let response = serve_file(head, &path);
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get(CONTENT_LENGTH).unwrap(), "0");

        let get = Request::builder()
            .method(Method::GET)
            .uri("/audio/1")
            .body(Vec::new())
            .unwrap();
        let response = serve_file(get, &path);
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.body().is_empty());

        let ranged = Request::builder()
            .method(Method::GET)
            .header(RANGE, "bytes=0-")
            .uri("/audio/1")
            .body(Vec::new())
            .unwrap();
        let response = serve_file(ranged, &path);
        assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(response.headers().get(CONTENT_RANGE).unwrap(), "bytes */0");

        assert!(
            read_range(
                &path,
                ByteRange {
                    start: 0,
                    end: MAX_RESPONSE_BYTES,
                },
            )
            .is_err()
        );

        let missing = root.join("deleted.wav");
        let request = Request::builder()
            .method(Method::GET)
            .uri("/audio/1")
            .body(Vec::new())
            .unwrap();
        assert_eq!(
            serve_file(request, &missing).status(),
            StatusCode::NOT_FOUND
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn head_does_not_read_a_body() {
        let root = test_root("head");
        let path = root.join("small.wav");
        fs::write(&path, b"small").unwrap();
        let request = Request::builder()
            .method(Method::HEAD)
            .uri("/audio/1")
            .body(Vec::new())
            .unwrap();
        let response = serve_file(request, &path);
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.body().is_empty());
        assert_eq!(response.headers().get(CONTENT_LENGTH).unwrap(), "5");
        let _ = fs::remove_dir_all(root);
    }
}
