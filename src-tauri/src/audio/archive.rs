//! Shared, dependency-free helpers for durable WAV archives.
//!
//! Archive writers write into a sibling `.part` file and publish the finished
//! WAV with a same-directory rename. Callers must not expose the final path or
//! mark the database asset ready until `finalize_wav_file` succeeds.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArchiveFailure {
    InvalidPath,
    TempExists,
    Open,
    Write,
    Flush,
    Sync,
    InvalidWav,
    FinalPathExists,
    Rename,
    Metadata,
    Interrupted,
}

impl ArchiveFailure {
    /// Stable, privacy-safe text suitable for the database and UI.
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::InvalidPath => "invalid_recording_path",
            Self::TempExists => "temporary_file_exists",
            Self::Open => "open_failed",
            Self::Write => "write_failed",
            Self::Flush => "flush_failed",
            Self::Sync => "sync_failed",
            Self::InvalidWav => "invalid_wav",
            Self::FinalPathExists => "final_file_exists",
            Self::Rename => "publish_failed",
            Self::Metadata => "metadata_failed",
            Self::Interrupted => "interrupted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WavMetadata {
    pub byte_length: i64,
    pub duration_ms: i64,
}

pub(crate) fn temp_path(final_path: &Path) -> PathBuf {
    let name = final_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("recording.wav");
    final_path.with_file_name(format!("{name}.part"))
}

/// Validate the complete WAV and collect metadata without exposing any path
/// or OS error text to the caller.
pub(crate) fn validate_wav_file(path: &Path) -> Result<WavMetadata, ArchiveFailure> {
    let mut reader = hound::WavReader::open(path).map_err(|_| ArchiveFailure::InvalidWav)?;
    let spec = reader.spec();
    if spec.channels == 0 || spec.sample_rate == 0 {
        return Err(ArchiveFailure::InvalidWav);
    }

    let mut samples = 0u64;
    for sample in reader.samples::<i16>() {
        sample.map_err(|_| ArchiveFailure::InvalidWav)?;
        samples = samples.saturating_add(1);
    }

    let denominator = u64::from(spec.sample_rate) * u64::from(spec.channels);
    let duration_ms = samples
        .saturating_mul(1000)
        .checked_div(denominator)
        .unwrap_or(0)
        .min(i64::MAX as u64) as i64;
    let byte_length = fs::metadata(path)
        .map_err(|_| ArchiveFailure::Metadata)?
        .len()
        .min(i64::MAX as u64) as i64;

    Ok(WavMetadata {
        byte_length,
        duration_ms,
    })
}

/// Publish a finalized WAV from a sibling temporary file. Rename is atomic on
/// the same volume, so a reader sees either no final path or the complete WAV.
pub(crate) fn finalize_wav_file(
    temp_file: &Path,
    final_file: &Path,
) -> Result<WavMetadata, ArchiveFailure> {
    if !temp_file.is_absolute()
        || !final_file.is_absolute()
        || temp_file.parent() != final_file.parent()
    {
        return Err(ArchiveFailure::InvalidPath);
    }
    if final_file.exists() {
        return Err(ArchiveFailure::FinalPathExists);
    }

    validate_wav_file(temp_file)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(temp_file)
        .map_err(|_| ArchiveFailure::Sync)?;
    file.sync_all().map_err(|_| ArchiveFailure::Sync)?;
    drop(file);
    fs::rename(temp_file, final_file).map_err(|_| ArchiveFailure::Rename)?;

    match validate_wav_file(final_file) {
        Ok(metadata) => Ok(metadata),
        Err(error) => {
            let _ = fs::remove_file(final_file);
            Err(error)
        }
    }
}

/// Write and publish a complete WAV atomically. Failed writes remove the
/// temporary file; an abrupt process termination is recovered from the
/// database's `saving` state on the next startup.
pub(crate) fn write_wav_atomically(
    final_file: &Path,
    bytes: &[u8],
) -> Result<WavMetadata, ArchiveFailure> {
    if !final_file.is_absolute() {
        return Err(ArchiveFailure::InvalidPath);
    }
    let temp_file = temp_path(final_file);
    if temp_file.exists() {
        return Err(ArchiveFailure::TempExists);
    }

    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_file)
            .map_err(|_| ArchiveFailure::Open)?;
        file.write_all(bytes).map_err(|_| ArchiveFailure::Write)?;
        file.flush().map_err(|_| ArchiveFailure::Flush)?;
        file.sync_all().map_err(|_| ArchiveFailure::Sync)?;
        drop(file);
        finalize_wav_file(&temp_file, final_file)
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp_file);
    }
    result
}

/// Resolve a stored path only when it is inside the canonical recordings
/// directory. Existing files are canonicalized to reject symlink escapes;
/// missing files are checked through their existing parent so repeated delete
/// operations remain safe.
pub(crate) fn validated_recording_path(root: &Path, raw_path: &str) -> Option<PathBuf> {
    let candidate = Path::new(raw_path);
    if !candidate.is_absolute() {
        return None;
    }

    let canonical_root = fs::canonicalize(root).ok()?;
    if let Ok(canonical_candidate) = fs::canonicalize(candidate) {
        return canonical_candidate
            .starts_with(&canonical_root)
            .then_some(canonical_candidate);
    }

    let parent = candidate.parent()?;
    let file_name = candidate.file_name()?;
    let canonical_parent = fs::canonicalize(parent).ok()?;
    if canonical_parent.starts_with(&canonical_root) {
        Some(canonical_parent.join(file_name))
    } else {
        None
    }
}

pub(crate) fn remove_recording_if_safe(root: &Path, raw_path: &str) -> bool {
    let Some(path) = validated_recording_path(root, raw_path) else {
        return false;
    };
    if !path.exists() {
        return true;
    }
    fs::remove_file(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::recorder::encode_wav;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_root(label: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("agenda-audio-{label}-{suffix}"));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn atomic_write_publishes_only_a_complete_wav() {
        let root = test_root("atomic");
        let final_path = root.join("dictation-1.wav");
        let wav = encode_wav(&[0.1, -0.1, 0.2, -0.2], 16_000).unwrap();

        let metadata = write_wav_atomically(&final_path, &wav).unwrap();

        assert!(final_path.exists());
        assert!(!temp_path(&final_path).exists());
        assert_eq!(
            metadata.byte_length,
            fs::metadata(&final_path).unwrap().len() as i64
        );
        assert!(validate_wav_file(&final_path).is_ok());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn invalid_or_interrupted_write_never_publishes_ready_file() {
        let root = test_root("interrupted");
        let final_path = root.join("dictation-2.wav");
        let wav = encode_wav(&[0.1, -0.1], 16_000).unwrap();
        let temp = temp_path(&final_path);
        fs::write(&temp, &wav[..wav.len() / 2]).unwrap();

        assert_eq!(
            finalize_wav_file(&temp, &final_path),
            Err(ArchiveFailure::InvalidWav)
        );
        assert!(!final_path.exists());
        assert!(temp.exists());

        fs::remove_file(&temp).unwrap();
        assert_eq!(
            write_wav_atomically(&final_path, b"not a wav"),
            Err(ArchiveFailure::InvalidWav)
        );
        assert!(!final_path.exists());
        assert!(!temp.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn deletion_accepts_only_recordings_paths_and_is_idempotent() {
        let root = test_root("delete");
        let inside = root.join("inside.wav");
        let outside = root.parent().unwrap().join("agenda-audio-outside.wav");
        fs::write(&inside, b"audio").unwrap();
        fs::write(&outside, b"audio").unwrap();

        assert!(remove_recording_if_safe(&root, &inside.to_string_lossy()));
        assert!(!inside.exists());
        assert!(remove_recording_if_safe(&root, &inside.to_string_lossy()));
        assert!(!remove_recording_if_safe(&root, &outside.to_string_lossy()));
        assert!(outside.exists());

        let _ = fs::remove_file(outside);
        let _ = fs::remove_dir_all(root);
    }
}
