use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use tauri::image::Image;
use tauri::{AppHandle, Manager, Runtime};

pub(crate) const TRAY_ID: &str = "main-tray";

/// Independent capture paths can overlap briefly while windows hand off.
/// Tracking owners keeps the tray active until the final capture stops.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum RecordingSource {
    Dictation,
    Conversation,
    Note,
}

pub(crate) struct RecordingIndicator {
    active_sources: Mutex<HashSet<RecordingSource>>,
    generations: Mutex<HashMap<RecordingSource, u64>>,
    icon_revision: AtomicU64,
    idle_icon: Image<'static>,
    recording_icon: Image<'static>,
}

impl RecordingIndicator {
    pub(crate) fn new(idle_icon: Image<'static>) -> Self {
        let recording_icon = violet_icon(&idle_icon);
        Self {
            active_sources: Mutex::new(HashSet::new()),
            generations: Mutex::new(HashMap::new()),
            icon_revision: AtomicU64::new(0),
            idle_icon,
            recording_icon,
        }
    }

    fn icon_for(&self, recording: bool) -> Image<'_> {
        if recording {
            self.recording_icon.clone()
        } else {
            self.idle_icon.clone()
        }
    }

    fn begin(&self, source: RecordingSource) -> u64 {
        let generation = {
            let mut generations = self
                .generations
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let generation = generations.get(&source).copied().unwrap_or(0) + 1;
            generations.insert(source, generation);
            generation
        };
        self.active_sources
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(source);
        generation
    }

    fn end(&self, source: RecordingSource, generation: Option<u64>) -> bool {
        if let Some(expected) = generation {
            let current = self
                .generations
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(&source)
                .copied();
            if current != Some(expected) {
                return false;
            }
        }
        self.active_sources
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&source);
        true
    }

    fn is_active(&self) -> bool {
        !self
            .active_sources
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty()
    }
}

pub(crate) fn start_recording<R: Runtime>(app: &AppHandle<R>, source: RecordingSource) -> u64 {
    let indicator = app.state::<RecordingIndicator>();
    let generation = indicator.begin(source);
    update_icon(app);
    generation
}

pub(crate) fn stop_recording<R: Runtime>(
    app: &AppHandle<R>,
    source: RecordingSource,
    generation: Option<u64>,
) {
    let indicator = app.state::<RecordingIndicator>();
    if indicator.end(source, generation) {
        update_icon(app);
    }
}

fn update_icon<R: Runtime>(app: &AppHandle<R>) {
    let revision = app
        .state::<RecordingIndicator>()
        .icon_revision
        .fetch_add(1, Ordering::SeqCst)
        + 1;
    let ui_app = app.clone();
    if let Err(error) = app.run_on_main_thread(move || {
        let indicator = ui_app.state::<RecordingIndicator>();
        if indicator.icon_revision.load(Ordering::SeqCst) != revision {
            return;
        }
        let recording = indicator.is_active();
        if let Some(tray) = ui_app.tray_by_id(TRAY_ID) {
            if let Err(error) = tray.set_icon(Some(indicator.icon_for(recording))) {
                log::warn!("Failed to update tray recording indicator: {error}");
            }
            let tooltip = if recording {
                "Agenda — Recording"
            } else {
                "Agenda"
            };
            if let Err(error) = tray.set_tooltip(Some(tooltip)) {
                log::warn!("Failed to update tray recording tooltip: {error}");
            }
        }
    }) {
        log::warn!("Failed to schedule tray recording indicator update: {error}");
    }
}

/// Preserve the application's silhouette, transparency, and shading while
/// shifting visible pixels to the recording color used by the overlay.
fn violet_icon(source: &Image<'_>) -> Image<'static> {
    let mut rgba = source.rgba().to_vec();
    for pixel in rgba.chunks_exact_mut(4) {
        if pixel[3] == 0 {
            continue;
        }

        let luminance =
            (u16::from(pixel[0]) * 54 + u16::from(pixel[1]) * 183 + u16::from(pixel[2]) * 19) / 256;
        // Bright violet (#A855F7), with a luminance floor so the indicator
        // stays purple and visible at Windows tray-icon sizes instead of
        // collapsing toward a dark red tint.
        let strength = 140 + (u32::from(luminance) * 115) / 255;
        pixel[0] = ((168 * strength) / 255) as u8;
        pixel[1] = ((85 * strength) / 255) as u8;
        pixel[2] = ((247 * strength) / 255) as u8;
    }
    Image::new_owned(rgba, source.width(), source.height())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn violet_icon_preserves_dimensions_and_alpha() {
        let source = Image::new(&[100, 150, 200, 0, 200, 180, 160, 255], 2, 1);
        let tinted = violet_icon(&source);

        assert_eq!(tinted.width(), 2);
        assert_eq!(tinted.height(), 1);
        assert_eq!(tinted.rgba()[3], 0);
        assert_eq!(tinted.rgba()[7], 255);
        assert!(tinted.rgba()[6] > tinted.rgba()[4]);
        assert!(tinted.rgba()[4] > tinted.rgba()[5]);
    }

    #[test]
    fn stale_stop_cannot_clear_a_new_recording_generation() {
        let icon = Image::new_owned(vec![0, 0, 0, 0], 1, 1);
        let indicator = RecordingIndicator::new(icon);
        let first = indicator.begin(RecordingSource::Dictation);
        let second = indicator.begin(RecordingSource::Dictation);

        assert!(!indicator.end(RecordingSource::Dictation, Some(first)));
        assert!(indicator.is_active());
        assert!(indicator.end(RecordingSource::Dictation, Some(second)));
        assert!(!indicator.is_active());
    }

    #[test]
    fn one_stopped_source_keeps_another_source_visible() {
        let icon = Image::new_owned(vec![0, 0, 0, 0], 1, 1);
        let indicator = RecordingIndicator::new(icon);
        indicator.begin(RecordingSource::Dictation);
        indicator.begin(RecordingSource::Note);

        assert!(indicator.end(RecordingSource::Dictation, None));
        assert!(indicator.is_active());
        assert!(indicator.end(RecordingSource::Note, None));
        assert!(!indicator.is_active());
    }
}
