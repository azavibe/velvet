import { useEffect, useRef, useState, useCallback } from "react";
import { useTranslation } from "react-i18next";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { listen, emit } from "@tauri-apps/api/event";
import { Menu, MenuItem, PredefinedMenuItem } from "@tauri-apps/api/menu";
import { getVersion } from "@tauri-apps/api/app";
import { check } from "@tauri-apps/plugin-updater";
import { sendNotification } from "@tauri-apps/plugin-notification";
import { AlertTriangle, Mic } from "lucide-react";
import { useDictation } from "@/hooks/useDictation";
import { useSettings } from "@/hooks/useSettings";
import { useHotkey } from "@/hooks/useHotkey";
import { useConversation } from "@/hooks/useConversation";
import { LoadingDots } from "@/components/ui/LoadingDots";
import { getOverlayMotionMode, getOverlayVisualPhase } from "@/components/overlayState";
import { whatsNewReleaseKey } from "@/config/whatsNew";
import {
  showSettings,
  showConversationWindow,
  quitApp,
  getSetting,
  setSetting,
} from "@/services/tauriApi";
import { dispatchCompletedVoiceCommand } from "@/services/voiceCommandDispatch";

function safeVoiceCommandError(error: unknown): string {
  const message = error instanceof Error ? error.message : typeof error === "string" ? error : "";
  const code = message.split(/[\s:]/, 1)[0] ?? "";
  return /^[a-z0-9_:-]{1,80}$/i.test(code) ? code : "dispatch_failed";
}

function DictationOverlayInner() {
  const { t } = useTranslation();
  const [overlayError, setOverlayError] = useState(false);
  const errorTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const showOverlayError = useCallback(() => {
    setOverlayError(true);
    if (errorTimerRef.current !== null) clearTimeout(errorTimerRef.current);
    errorTimerRef.current = setTimeout(() => {
      errorTimerRef.current = null;
      setOverlayError(false);
    }, 5000);
  }, []);

  useEffect(() => () => {
    if (errorTimerRef.current !== null) clearTimeout(errorTimerRef.current);
  }, []);

  // Use native OS notifications instead of in-window toasts (overlay is too small)
  const notifyError = useCallback((props: { title?: string; description?: string }) => {
    showOverlayError();
    sendNotification({ title: props.title ?? t("overlay.notification.title"), body: props.description ?? "" });
  }, [showOverlayError, t]);

  const { settings, loaded } = useSettings();

  // Spoken app commands ("Aral, start notes"). Recognized here because the
  // overlay owns dictation, but deliberately *executed* by asking the
  // Conversation window to do it — that window owns capture state, the
  // consent gate, and persona selection, so a voice-started conversation
  // goes through exactly the same path as a clicked one.
  const handleVoiceCommand = useCallback(
    async (
      text: string,
      context: {
        agentName: string;
        agentAliases: string[];
        retryTranscription: (prompt: string) => Promise<string | null>;
        applicationLanguage: string | null;
        durationMs: number | null;
      },
    ): Promise<boolean> => {
      const diagnosticsEnabled = import.meta.env.DEV;
      try {
        const result = await dispatchCompletedVoiceCommand(
          text,
          context.agentName,
          context.agentAliases,
          settings.personas,
          undefined,
          diagnosticsEnabled
            ? (event) => console.debug("[voice-command]", event)
            : undefined,
          context.retryTranscription,
          context.applicationLanguage ?? undefined,
          context.durationMs !== null && context.durationMs <= 6_000,
        );
        return result.handled;
      } catch (e) {
        if (diagnosticsEnabled) {
          console.debug("[voice-command] dispatch failed", {
            reason: safeVoiceCommandError(e),
          });
        }
        notifyError({ description: safeVoiceCommandError(e) });
        // Handled (and reported) — falling through to paste would type the
        // command into whatever window is focused, which is worse.
        return true;
      }
    },
    [settings.personas, notifyError],
  );

  const handleHotkeyRegistrationError = useCallback(() => {
    notifyError({
      title: t("overlay.error"),
      description: t("overlay.hotkeyRegistrationFailed"),
    });
  }, [notifyError, t]);

  const { phase, isRecording, isProcessing, start, stop, toggle, cancel } =
    useDictation({ onToast: notifyError, onVoiceCommand: handleVoiceCommand });

  // Lightweight instance — no transcript/suggestion state, autoTrigger off
  // (the dedicated Conversation window is the one that auto-fires
  // suggestions; this instance exists only so the dictation hotkey can
  // check `isActive` and reuse itself as the "suggest now" trigger, and so
  // the context menu can start/stop from here too).
  const conversation = useConversation({
    settings,
    reasoningModel: settings.conversationReasoningModel,
    reasoningProvider: settings.conversationReasoningProvider,
    reasoningApiKey:
      (settings[`${settings.conversationReasoningProvider}ApiKey` as keyof typeof settings] as string) ?? "",
    groqApiKey: settings.groqApiKey,
    autoTrigger: false,
    onToast: notifyError,
  });

  // On first launch: open settings if no API keys are configured.
  // After version change: open settings (the panel self-detects
  // version changes and shows What's New independently).
  // After in-app update: reopen settings so the user sees the About tab.
  useEffect(() => {
    if (!loaded) return;
    const hasAnyKey =
      settings.openaiApiKey || settings.anthropicApiKey || settings.geminiApiKey ||
      settings.groqApiKey || settings.mistralApiKey || settings.qwenApiKey ||
      settings.openrouterApiKey;
    if (!hasAnyKey) {
      showSettings();
      return;
    }
    (async () => {
      try {
        const [currentVersion, lastSeen, openAfterUpdate, lastWhatsNewRelease] = await Promise.all([
          getVersion(),
          getSetting<string>("lastSeenVersion"),
          getSetting<boolean>("openSettingsAfterUpdate"),
          getSetting<string>("lastWhatsNewRelease"),
        ]);
        let needsSettings = false;
        const isDev = import.meta.env.DEV;
        if (isDev || lastSeen !== currentVersion) {
          await setSetting("lastSeenVersion", currentVersion);
          needsSettings = true;
        }
        if (openAfterUpdate) {
          setSetting("openSettingsAfterUpdate", false);
          needsSettings = true;
        }
        if (lastWhatsNewRelease !== whatsNewReleaseKey(currentVersion)) {
          needsSettings = true;
        }
        if (needsSettings) showSettings();
      } catch {
        // Silently ignore — startup checks are non-critical
      }
    })();
  }, [loaded]); // eslint-disable-line react-hooks/exhaustive-deps

  // Check for updates on startup and notify settings window
  const [updateAvailable, setUpdateAvailable] = useState(false);
  useEffect(() => {
    check()
      .then((update) => {
        if (update) {
          setUpdateAvailable(true);
          emit("update-available", { version: update.version });
        }
      })
      .catch(() => {}); // silently ignore network errors
  }, []);

  // Suspend hotkey while settings window is capturing a new shortcut
  const [hotkeyCapturing, setHotkeyCapturing] = useState(false);
  useEffect(() => {
    const unlisten = listen<{ capturing: boolean }>("hotkey-capturing", (event) => {
      setHotkeyCapturing(event.payload.capturing);
    });
    return () => { unlisten.then((fn) => fn()); };
  }, []);

  // Hotkey integration — reused for conversation suggestions. While a
  // conversation is active you're never also dictating, so the same key
  // does double duty: triggers a suggestion instead of starting/stopping
  // dictation. No separate hotkey to configure.
  useHotkey({
    shortcut: settings.dictationKey,
    activationMode: settings.activationMode,
    onToggle: () => {
      if (conversation.isActive) { conversation.forceSuggestion(); return; }
      toggle(settings.selectedMicDeviceId || undefined);
    },
    onPushStart: () => {
      if (conversation.isActive) { conversation.forceSuggestion(); return; }
      start(settings.selectedMicDeviceId || undefined);
    },
    onPushEnd: () => {
      if (conversation.isActive) return; // avoid a second call on release
      stop();
    },
    onRegistrationError: handleHotkeyRegistrationError,
    enabled: loaded && !!settings.dictationKey && !hotkeyCapturing,
  });

  // Right-click to open native context menu (renders outside the small webview)
  const handleContextMenu = useCallback(
    async (e: React.MouseEvent) => {
      e.preventDefault();
      const items: (MenuItem | PredefinedMenuItem)[] = [
        await MenuItem.new({ id: "settings", text: t("overlay.menu.settings"), action: () => showSettings() }),
      ];
      if (isRecording) {
        items.push(
          await MenuItem.new({ id: "cancel", text: t("overlay.menu.cancel"), action: () => cancel() }),
        );
      }
      items.push(await PredefinedMenuItem.new({ item: "Separator" }));
      items.push(
        await MenuItem.new({
          id: "conversation",
          text: conversation.isActive
            ? t("overlay.menu.stopConversation")
            : t("overlay.menu.openConversation"),
          action: () => {
            if (conversation.isActive) {
              conversation.stop();
            } else {
              showConversationWindow();
            }
          },
        }),
      );
      items.push(await PredefinedMenuItem.new({ item: "Separator" }));
      items.push(await MenuItem.new({ id: "quit", text: t("overlay.menu.quit"), action: () => quitApp() }));
      const menu = await Menu.new({ items });
      await menu.popup();
    },
    [isRecording, cancel, t, conversation]
  );

  // Drag-vs-click detection on the recording button
  const dragStartRef = useRef<{ x: number; y: number } | null>(null);
  const isDraggingRef = useRef(false);

  const handleButtonPointerDown = useCallback((e: React.PointerEvent) => {
    if (e.button !== 0) return; // left-click only
    dragStartRef.current = { x: e.clientX, y: e.clientY };
    isDraggingRef.current = false;
  }, []);

  const handleButtonPointerMove = useCallback(async (e: React.PointerEvent) => {
    if (!dragStartRef.current || isDraggingRef.current) return;
    const dx = e.clientX - dragStartRef.current.x;
    const dy = e.clientY - dragStartRef.current.y;
    if (Math.abs(dx) + Math.abs(dy) > 5) {
      isDraggingRef.current = true;
      dragStartRef.current = null;
      await getCurrentWebviewWindow().startDragging();
    }
  }, []);

  const handleButtonPointerUp = useCallback((e: React.PointerEvent) => {
    if (e.button !== 0) return; // left-click only
    if (isDraggingRef.current) {
      isDraggingRef.current = false;
      dragStartRef.current = null;
      return;
    }
    dragStartRef.current = null;
    if (phase === "idle") {
      start(settings.selectedMicDeviceId || undefined);
    } else if (phase === "recording") {
      stop();
    }
  }, [phase, start, stop, settings.selectedMicDeviceId]);

  const visualPhase = getOverlayVisualPhase(phase, overlayError);
  const showError = visualPhase === "error";
  const motionMode = getOverlayMotionMode(visualPhase, false);

  return (
    <>
      <style>{`
        @keyframes overlay-listening-smoke {
          0%, 100% { transform: scale(0.88); opacity: 0.42; }
          50% { transform: scale(1.08); opacity: 0.78; }
        }
        @keyframes overlay-processing-pulse {
          0%, 100% { transform: translateY(0) scaleY(0.84); opacity: 0.62; }
          50% { transform: translateY(-1px) scaleY(1.08); opacity: 1; }
        }
        .overlay-visible-circle {
          transform-origin: 50% 50%;
          isolation: isolate;
        }
        .overlay-listening-smoke {
          background: radial-gradient(
            circle at 50% 50%,
            rgba(210, 60, 255, 0.95) 0%,
            rgba(114, 92, 255, 0.82) 48%,
            rgba(40, 183, 255, 0.68) 74%,
            rgba(210, 60, 255, 0.36) 100%
          );
          transform-origin: 50% 50%;
          animation: overlay-listening-smoke 1.6s ease-in-out infinite;
        }
        .overlay-processing-surface {
          background: radial-gradient(
            circle at 50% 50%,
            #d23cff 0%,
            #725cff 58%,
            #28b7ff 100%
          );
        }
        .overlay-processing-dots > div {
          animation: overlay-processing-pulse 0.9s ease-in-out infinite;
        }
        .overlay-processing-dots > div:nth-child(2) {
          animation-delay: 0.14s;
        }
        .overlay-processing-dots > div:nth-child(3) {
          animation-delay: 0.28s;
        }
        @media (prefers-reduced-motion: reduce) {
          .overlay-listening-smoke,
          .overlay-processing-dots > div {
            animation: none !important;
          }
        }
      `}</style>
      <div className="dictation-window flex flex-col items-center justify-center h-screen pointer-events-none">
        <div
          className="relative w-11 h-11 flex items-center justify-center pointer-events-auto"
          onContextMenu={handleContextMenu}
        >
          <button
            onPointerDown={handleButtonPointerDown}
            onPointerMove={handleButtonPointerMove}
            onPointerUp={handleButtonPointerUp}
            disabled={isProcessing}
            data-overlay-hit-target="44px"
            className={`group relative flex w-11 h-11 items-center justify-center border-0 bg-transparent shadow-none transition-transform duration-200 focus-visible:outline-none ${
              isProcessing
                ? "cursor-wait"
                : isRecording
                  ? "active:scale-95"
                  : "hover:scale-105 active:scale-95"
            }`}
            aria-label={
              showError
                ? t("overlay.error")
                : isProcessing
                  ? t("overlay.processing")
                  : isRecording
                    ? t("overlay.stopRecording")
                    : t("overlay.startRecording")
            }
          >
            <span
              data-overlay-visible-circle="32px"
              className={`overlay-visible-circle relative flex w-8 h-8 shrink-0 items-center justify-center overflow-hidden rounded-full border-0 shadow-md group-focus-visible:ring-2 group-focus-visible:ring-inset group-focus-visible:ring-ring ${
                showError
                  ? "bg-destructive text-destructive-foreground"
                  : isProcessing
                    ? "overlay-processing-surface text-foreground-bright"
                    : isRecording
                      ? "bg-[#725cff] text-white"
                      : "bg-primary text-foreground-bright"
              }`}
            >
              {motionMode === "listening" && (
                <span
                  data-overlay-internal-effect="centered-clipped"
                  className="overlay-listening-smoke overlay-motion pointer-events-none absolute inset-0"
                  aria-hidden="true"
                />
              )}
              {showError ? (
                <AlertTriangle className="relative z-10 w-4 h-4" aria-hidden="true" />
              ) : isProcessing ? (
                <LoadingDots className={motionMode === "processing" ? "overlay-processing-dots overlay-motion" : "overlay-processing-dots"} />
              ) : (
                <Mic className="relative z-10 w-4 h-4" aria-hidden="true" />
              )}
              {updateAvailable && (
                <span
                  className="absolute right-0.5 top-0.5 z-20 w-2 h-2 rounded-full bg-warning"
                  title={t("overlay.updateAvailable")}
                />
              )}
            </span>
          </button>
        </div>
      </div>
    </>
  );
}

export default function DictationOverlay() {
  return <DictationOverlayInner />;
}
