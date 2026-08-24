use super::ResultExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tauri::{Emitter, Manager, State};

const CAPTURE_INTENT_TTL: Duration = Duration::from_secs(30);
const MAX_PENDING_CAPTURE_INTENTS: usize = 8;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CaptureIntentKind {
    Note,
    Conversation,
    Stop,
    Settings,
}

impl CaptureIntentKind {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "note" => Ok(Self::Note),
            "conversation" => Ok(Self::Conversation),
            "stop" => Ok(Self::Stop),
            "settings" => Ok(Self::Settings),
            _ => Err("unsupported_capture_intent".to_owned()),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct CaptureIntent {
    pub id: u64,
    pub kind: CaptureIntentKind,
    pub persona_id: Option<String>,
}

#[derive(Debug, Clone)]
struct PendingCaptureIntent {
    intent: CaptureIntent,
    created_at: Instant,
}

pub struct CaptureIntentState {
    pending: Mutex<VecDeque<PendingCaptureIntent>>,
    next_id: AtomicU64,
}

impl Default for CaptureIntentState {
    fn default() -> Self {
        Self {
            pending: Mutex::new(VecDeque::new()),
            next_id: AtomicU64::new(1),
        }
    }
}

impl CaptureIntentState {
    fn purge_expired(pending: &mut VecDeque<PendingCaptureIntent>) {
        pending.retain(|entry| entry.created_at.elapsed() < CAPTURE_INTENT_TTL);
    }

    fn enqueue(
        &self,
        kind: CaptureIntentKind,
        persona_id: Option<String>,
    ) -> Result<CaptureIntent, String> {
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| "capture_intent_state_unavailable".to_owned())?;
        Self::purge_expired(&mut pending);
        if pending.len() >= MAX_PENDING_CAPTURE_INTENTS {
            return Err("capture_intent_queue_full".to_owned());
        }
        let intent = CaptureIntent {
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            kind,
            persona_id,
        };
        pending.push_back(PendingCaptureIntent {
            intent: intent.clone(),
            created_at: Instant::now(),
        });
        Ok(intent)
    }

    #[cfg(test)]
    fn peek(&self) -> Result<Option<CaptureIntent>, String> {
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| "capture_intent_state_unavailable".to_owned())?;
        Self::purge_expired(&mut pending);
        Ok(pending.front().map(|entry| entry.intent.clone()))
    }

    fn peek_for(&self, target: Option<&str>) -> Result<Option<CaptureIntent>, String> {
        if !matches!(target, None | Some("conversation") | Some("settings")) {
            return Err("unsupported_capture_intent_target".to_owned());
        }
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| "capture_intent_state_unavailable".to_owned())?;
        Self::purge_expired(&mut pending);
        let matches_target = |intent: &CaptureIntent| match target {
            Some("settings") => matches!(intent.kind, CaptureIntentKind::Settings),
            Some("conversation") => !matches!(intent.kind, CaptureIntentKind::Settings),
            _ => true,
        };
        Ok(pending
            .iter()
            .find(|entry| matches_target(&entry.intent))
            .map(|entry| entry.intent.clone()))
    }

    fn acknowledge(&self, intent_id: u64) -> Result<bool, String> {
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| "capture_intent_state_unavailable".to_owned())?;
        Self::purge_expired(&mut pending);
        let Some(index) = pending
            .iter()
            .position(|entry| entry.intent.id == intent_id)
        else {
            return Ok(false);
        };
        pending.remove(index);
        Ok(true)
    }

    fn expire(&self, intent_id: u64) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.retain(|entry| entry.intent.id != intent_id);
        }
    }
}

#[tauri::command]
pub fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}

#[tauri::command]
pub fn show_settings(app: tauri::AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("settings")
        .ok_or("Settings window not found")?;
    window.show().str_err()?;
    window.set_focus().str_err()?;
    Ok(())
}

#[tauri::command]
pub fn show_conversation_window(app: tauri::AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("conversation")
        .ok_or("Conversation window not found")?;
    window.show().str_err()?;
    window.set_focus().str_err()?;
    Ok(())
}

/// Queue an application action before showing its target WebView. The event
/// is only a wake-up; the pending row is the source of truth and is consumed
/// after the receiving window has finished registering its listeners.
fn create_configured_window(
    app: &tauri::AppHandle,
    label: &str,
) -> Result<tauri::WebviewWindow, String> {
    if let Some(window) = app.get_webview_window(label) {
        return Ok(window);
    }
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|config| config.label == label)
        .cloned()
        .ok_or_else(|| "conversation_window_config_missing".to_owned())?;
    tauri::WebviewWindowBuilder::from_config(app, &config)
        .map_err(|_| "conversation_window_creation_failed".to_owned())?
        .build()
        .or_else(|_| {
            app.get_webview_window(label)
                .ok_or(tauri::Error::WindowNotFound)
        })
        .map_err(|_| "conversation_window_creation_failed".to_owned())
}

trait CaptureIntentTarget {
    fn deliver(&self, intent: &CaptureIntent) -> Result<(), String>;
}

struct TauriCaptureIntentTarget<'a> {
    app: &'a tauri::AppHandle,
}

impl CaptureIntentTarget for TauriCaptureIntentTarget<'_> {
    fn deliver(&self, intent: &CaptureIntent) -> Result<(), String> {
        let label = if matches!(intent.kind, CaptureIntentKind::Settings) {
            "settings"
        } else {
            "conversation"
        };
        let window = create_configured_window(self.app, label)?;
        if !matches!(&intent.kind, CaptureIntentKind::Stop)
            && window.show().and_then(|_| window.set_focus()).is_err()
        {
            return Err("conversation_window_dispatch_failed".to_owned());
        }
        self.app
            .emit_to(label, "capture-intent-available", ())
            .map_err(|_| "capture_intent_notify_failed".to_owned())
    }
}

fn dispatch_to_target(
    state: &CaptureIntentState,
    kind: CaptureIntentKind,
    persona_id: Option<String>,
    target: &impl CaptureIntentTarget,
) -> Result<u64, String> {
    // Persist the action before any target-window work. A slow first WebView
    // mount can miss the event notification and still retrieve this intent.
    let intent = state.enqueue(kind, persona_id)?;
    #[cfg(debug_assertions)]
    log::debug!(
        "[voice-command] code=intent_created id={} action={:?}",
        intent.id,
        intent.kind
    );

    if let Err(code) = target.deliver(&intent) {
        // A known terminal target failure is the safe expiry point. Leaving
        // it queued would execute a command later after the user was shown an
        // error and reasonably retried.
        state.expire(intent.id);
        #[cfg(debug_assertions)]
        log::debug!("[voice-command] code={code} id={}", intent.id);
        return Err(code);
    }
    Ok(intent.id)
}

#[tauri::command]
pub async fn dispatch_capture_intent(
    app: tauri::AppHandle,
    state: State<'_, CaptureIntentState>,
    kind: String,
    persona_id: Option<String>,
) -> Result<u64, String> {
    let kind = CaptureIntentKind::parse(&kind)?;
    let persona_id = if matches!(&kind, CaptureIntentKind::Conversation) {
        persona_id
    } else {
        None
    };
    dispatch_to_target(
        &state,
        kind,
        persona_id,
        &TauriCaptureIntentTarget { app: &app },
    )
}

#[tauri::command]
pub fn get_pending_capture_intent(
    state: State<'_, CaptureIntentState>,
    target: Option<String>,
) -> Result<Option<CaptureIntent>, String> {
    state.peek_for(target.as_deref())
}

#[tauri::command]
pub fn acknowledge_capture_intent(
    state: State<'_, CaptureIntentState>,
    intent_id: u64,
) -> Result<bool, String> {
    let acknowledged = state.acknowledge(intent_id)?;
    #[cfg(debug_assertions)]
    log::debug!(
        "[voice-command] code=intent_acknowledged id={} accepted={}",
        intent_id,
        acknowledged
    );
    Ok(acknowledged)
}

#[tauri::command]
pub fn hide_conversation_window(app: tauri::AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("conversation")
        .ok_or("Conversation window not found")?;
    window.hide().str_err()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct FakeTarget<'a> {
        state: &'a CaptureIntentState,
        present: Cell<bool>,
        fail_creation: bool,
        creations: Cell<u32>,
        deliveries: Cell<u32>,
    }

    impl CaptureIntentTarget for FakeTarget<'_> {
        fn deliver(&self, intent: &CaptureIntent) -> Result<(), String> {
            // The durable row must exist before creation, show/focus, or the
            // wake-up event is attempted.
            assert!(
                self.state
                    .pending
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|item| item.intent.id == intent.id)
            );
            if !self.present.get() {
                if self.fail_creation {
                    return Err("conversation_window_creation_failed".to_owned());
                }
                self.present.set(true);
                self.creations.set(self.creations.get() + 1);
            }
            self.deliveries.set(self.deliveries.get() + 1);
            Ok(())
        }
    }

    fn target(state: &CaptureIntentState, present: bool, fail_creation: bool) -> FakeTarget<'_> {
        FakeTarget {
            state,
            present: Cell::new(present),
            fail_creation,
            creations: Cell::new(0),
            deliveries: Cell::new(0),
        }
    }

    #[test]
    fn slow_mount_peeks_without_clearing_and_acknowledges_once() {
        let state = CaptureIntentState::default();
        let intent = state.enqueue(CaptureIntentKind::Note, None).unwrap();

        assert_eq!(state.peek().unwrap(), Some(intent.clone()));
        assert_eq!(state.peek().unwrap(), Some(intent.clone()));
        assert!(state.acknowledge(intent.id).unwrap());
        assert!(!state.acknowledge(intent.id).unwrap());
        assert_eq!(state.peek().unwrap(), None);
    }

    #[test]
    fn close_commands_remain_ordered_and_repeated_actions_get_new_ids() {
        let state = CaptureIntentState::default();
        let note = state.enqueue(CaptureIntentKind::Note, None).unwrap();
        let support = state
            .enqueue(CaptureIntentKind::Conversation, Some("support".to_owned()))
            .unwrap();

        assert_eq!(state.peek().unwrap(), Some(note.clone()));
        assert!(state.acknowledge(note.id).unwrap());
        assert_eq!(state.peek().unwrap(), Some(support.clone()));
        assert!(state.acknowledge(support.id).unwrap());
        let repeated = state.enqueue(CaptureIntentKind::Note, None).unwrap();
        assert_ne!(note.id, repeated.id);
    }

    #[test]
    fn failed_target_delivery_expires_only_the_failed_intent() {
        let state = CaptureIntentState::default();
        let failed = state.enqueue(CaptureIntentKind::Note, None).unwrap();
        let next = state
            .enqueue(CaptureIntentKind::Conversation, None)
            .unwrap();
        state.expire(failed.id);
        assert_eq!(state.peek().unwrap(), Some(next));
    }

    #[test]
    fn dispatch_creates_an_absent_target_after_persisting_the_intent() {
        let state = CaptureIntentState::default();
        let target = target(&state, false, false);
        let id = dispatch_to_target(&state, CaptureIntentKind::Note, None, &target).unwrap();

        assert_eq!(target.creations.get(), 1);
        assert_eq!(target.deliveries.get(), 1);
        assert_eq!(state.peek().unwrap().map(|intent| intent.id), Some(id));
    }

    #[test]
    fn dispatch_reuses_an_open_target_and_expires_failed_creation() {
        let state = CaptureIntentState::default();
        let open = target(&state, true, false);
        dispatch_to_target(&state, CaptureIntentKind::Conversation, None, &open).unwrap();
        assert_eq!(open.creations.get(), 0);
        let first = state.peek().unwrap().unwrap();
        state.acknowledge(first.id).unwrap();

        let failed = target(&state, false, true);
        assert_eq!(
            dispatch_to_target(&state, CaptureIntentKind::Note, None, &failed),
            Err("conversation_window_creation_failed".to_owned())
        );
        assert_eq!(state.peek().unwrap(), None);
    }

    #[test]
    fn capture_intent_kind_rejects_unknown_actions() {
        assert_eq!(
            CaptureIntentKind::parse("note").unwrap(),
            CaptureIntentKind::Note
        );
        assert!(CaptureIntentKind::parse("paste").is_err());
        assert_eq!(
            CaptureIntentKind::parse("settings").unwrap(),
            CaptureIntentKind::Settings
        );
    }

    #[test]
    fn target_queries_do_not_steal_each_others_intents() {
        let state = CaptureIntentState::default();
        let settings = state.enqueue(CaptureIntentKind::Settings, None).unwrap();
        let note = state.enqueue(CaptureIntentKind::Note, None).unwrap();
        assert_eq!(state.peek_for(Some("conversation")).unwrap(), Some(note));
        assert_eq!(state.peek_for(Some("settings")).unwrap(), Some(settings));
        assert_eq!(
            state.peek_for(Some("other")),
            Err("unsupported_capture_intent_target".to_owned())
        );
    }
}
