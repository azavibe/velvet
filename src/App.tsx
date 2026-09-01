import { useState, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { listen } from "@tauri-apps/api/event";
import { getSetting, onCaptureIntentAvailable } from "@/services/tauriApi";
import DictationOverlay from "@/components/DictationOverlay";
import SettingsPanel from "@/components/settings/SettingsPanel";
import ConversationWindow from "@/components/ConversationWindow";
import { startupMark } from "@/services/startupDiagnostics";
import { settingsIntentConsumer } from "@/services/captureIntentConsumer";

type AppView = "overlay" | "settings" | "conversation";

function currentView(): AppView {
  const label = getCurrentWebviewWindow().label;
  if (label === "settings") return "settings";
  if (label === "conversation") return "conversation";
  return "overlay";
}

function App() {
  // Resolve the native window before the first render. Starting every WebView
  // as the overlay briefly mounted its hotkeys and startup effects in the
  // hidden Settings and Conversation windows as well.
  const [view] = useState<AppView>(currentView);
  const { i18n } = useTranslation();

  useEffect(() => {
    startupMark("React/WebView initialized");
    const label = getCurrentWebviewWindow().label;
    startupMark("WebView window resolved", { has_window_label: label.length > 0 });
  }, []);

  // Sync i18next language from stored setting on mount + cross-window changes
  useEffect(() => {
    startupMark("UI language query started");
    getSetting<string>("uiLanguage").then((lang) => {
      if (lang) i18n.changeLanguage(lang);
      startupMark("UI language query complete", { configured: !!lang });
    }).catch(() => {});

    const unlisten = listen<{ key: string; value: unknown }>(
      "settings-changed",
      (event) => {
        if (event.payload.key === "uiLanguage" && event.payload.value) {
          i18n.changeLanguage(event.payload.value as string);
        }
      }
    );
    return () => { unlisten.then((fn) => fn()); };
  }, [i18n]);

  useEffect(() => {
    if (view !== "settings") return;
    settingsIntentConsumer.setDiagnosticListener(
      import.meta.env.DEV ? (event) => console.debug("[voice-command]", event) : undefined,
    );
    const consume = () => settingsIntentConsumer.consume(true, async (intent) => {
      if (intent.kind !== "settings") throw new Error("capture_intent_target_mismatch");
    });
    const unlisten = onCaptureIntentAvailable(() => { void consume().catch(() => {}); });
    void unlisten.then(() => consume()).catch(() => {});
    return () => { unlisten.then((fn) => fn()); };
  }, [view]);

  if (view === "settings") {
    return <SettingsPanel />;
  }

  if (view === "conversation") {
    return <ConversationWindow />;
  }

  return <DictationOverlay />;
}

export default App;
