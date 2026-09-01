use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

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
    idle_icon: Image<'static>,
    recording_icon: Image<'static>,
}

impl RecordingIndicator {
    pub(crate) fn new(idle_icon: Image<'static>) -> Self {
        let recording_icon = magenta_icon(&idle_icon);
        Self {
            active_sources: Mutex::new(HashSet::new()),
            generations: Mutex::new(HashMap::new()),
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
}

pub(crate) fn start_recording<R: Runtime>(app: &AppHandle<R>, source: RecordingSource) -> u64 {
    let indicator = app.state::<RecordingIndicator>();
    let generation = {
        let mut generations = indicator
            .generations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let generation = generations.get(&source).copied().unwrap_or(0) + 1;
        generations.insert(source, generation);
        generation
    };
    {
        let mut active = indicator
            .active_sources
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        active.insert(source);
    }

    update_icon(app, &indicator, true);
    generation
}

pub(crate) fn stop_recording<R: Runtime>(
    app: &AppHandle<R>,
    source: RecordingSource,
    generation: Option<u64>,
) {
    let indicator = app.state::<RecordingIndicator>();
    if let Some(expected) = generation {
        let current = indicator
            .generations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&source)
            .copied();
        if current != Some(expected) {
            return;
        }
    }

    let any_recording = {
        let mut active = indicator
            .active_sources
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        active.remove(&source);
        !active.is_empty()
    };

    update_icon(app, &indicator, any_recording);
}

fn update_icon<R: Runtime>(
    app: &AppHandle<R>,
    indicator: &RecordingIndicator,
    any_recording: bool,
) {
    if let Some(tray) = app.tray_by_id(TRAY_ID)
        && let Err(error) = tray.set_icon(Some(indicator.icon_for(any_recording)))
    {
        log::warn!("Failed to update tray recording indicator: {error}");
    }
}

/// Preserve the application's silhouette, transparency, and shading while
/// shifting visible pixels to the recording color used by the overlay.
fn magenta_icon(source: &Image<'_>) -> Image<'static> {
    let mut rgba = source.rgba().to_vec();
    for pixel in rgba.chunks_exact_mut(4) {
        if pixel[3] == 0 {
            continue;
        }

        let luminance =
            (u16::from(pixel[0]) * 54 + u16::from(pixel[1]) * 183 + u16::from(pixel[2]) * 19) / 256;
        // #FF2BD6, scaled by the source luminance so highlights and the
        // microphone glyph remain legible at Windows tray-icon sizes.
        pixel[0] = luminance as u8;
        pixel[1] = ((luminance * 43) / 255) as u8;
        pixel[2] = ((luminance * 214) / 255) as u8;
    }
    Image::new_owned(rgba, source.width(), source.height())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magenta_icon_preserves_dimensions_and_alpha() {
        let source = Image::new(&[100, 150, 200, 0, 200, 180, 160, 255], 2, 1);
        let tinted = magenta_icon(&source);

        assert_eq!(tinted.width(), 2);
        assert_eq!(tinted.height(), 1);
        assert_eq!(tinted.rgba()[3], 0);
        assert_eq!(tinted.rgba()[7], 255);
        assert!(tinted.rgba()[4] > tinted.rgba()[6]);
        assert!(tinted.rgba()[6] > tinted.rgba()[5]);
    }
}
