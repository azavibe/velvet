import { useEffect, useRef, useState, useCallback } from "react";
import { useTranslation } from "react-i18next";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { currentMonitor } from "@tauri-apps/api/window";
import { LogicalSize, LogicalPosition } from "@tauri-apps/api/dpi";
import { listen, emit } from "@tauri-apps/api/event";
import { Menu, MenuItem, PredefinedMenuItem } from "@tauri-apps/api/menu";
import { getVersion } from "@tauri-apps/api/app";
import { check } from "@tauri-apps/plugin-updater";
import { sendNotification } from "@tauri-apps/plugin-notification";
import { Mic, Sparkles, Square } from "lucide-react";
import { useDictation } from "@/hooks/useDictation";
import { useSettings } from "@/hooks/useSettings";
import { useHotkey } from "@/hooks/useHotkey";
import { useConversation } from "@/hooks/useConversation";
import { LoadingDots } from "@/components/ui/LoadingDots";
import { ConversationConsentModal } from "@/components/ui/ConversationConsentModal";
import { showSettings, quitApp, getSetting, setSetting } from "@/services/tauriApi";

// The overlay is fixed at 100x100 while idle (tauri.conf.json). While a
// conversation is active it expands to this size so the live transcript +
// suggestion are readable, then collapses back on stop. Both are driven by
// setSize()/setPosition() in logical units from these constants — never
// from persisted window state, which 0.8.7 deliberately stopped restoring
// for this window to avoid mixed-DPI size drift across launches.
const OVERLAY_IDLE_SIZE = 100;
const CONVERSATION_PANEL_WIDTH = 340;
const CONVERSATION_PANEL_HEIGHT = 420;

function DictationOverlayInner() {
  const { t } = useTranslation();
  // Use native OS notifications instead of in-window toasts (overlay is too small)
  const notifyError = useCallback((props: { title?: string; description?: string }) => {
    sendNotification({ title: props.title ?? t("overlay.notification.title"), body: props.description ?? "" });
  }, [t]);

  const { phase, isRecording, isProcessing, audioLevel, start, stop, toggle, cancel } =
    useDictation({ onToast: notifyError });

  const { settings, loaded } = useSettings();

  const reasoningApiKey = (settings[`${settings.reasoningProvider}ApiKey` as keyof typeof settings] as string) ?? "";
  const conversation = useConversation({
    settings,
    reasoningModel: settings.reasoningModel,
    reasoningProvider: settings.reasoningProvider,
    reasoningApiKey,
    groqApiKey: settings.groqApiKey,
    onToast: notifyError,
  });
  const [conversationConsentRequested, setConversationConsentRequested] = useState(false);

  // Expand the overlay into a live transcript + suggestion panel for the
  // duration of a conversation, then collapse back to the idle 100x100.
  // Position is restored exactly (not recomputed) on collapse; the growth
  // direction on expand is picked from proximity to the monitor's work-area
  // edges so a button parked near a corner never grows off-screen.
  const originalPosRef = useRef<{ x: number; y: number } | null>(null);
  useEffect(() => {
    const win = getCurrentWebviewWindow();
    (async () => {
      if (conversation.isActive) {
        try {
          const physicalPos = await win.outerPosition();
          const monitor = await currentMonitor();
          const scale = monitor?.scaleFactor ?? await win.scaleFactor();
          const logicalX = physicalPos.x / scale;
          const logicalY = physicalPos.y / scale;
          originalPosRef.current = { x: logicalX, y: logicalY };

          let newX = logicalX;
          let newY = logicalY;
          if (monitor) {
            const workX = monitor.workArea.position.x / scale;
            const workY = monitor.workArea.position.y / scale;
            const workW = monitor.workArea.size.width / scale;
            const workH = monitor.workArea.size.height / scale;

            const spaceRight = workX + workW - (logicalX + OVERLAY_IDLE_SIZE);
            const spaceBelow = workY + workH - (logicalY + OVERLAY_IDLE_SIZE);
            const growLeft = spaceRight < CONVERSATION_PANEL_WIDTH - OVERLAY_IDLE_SIZE;
            const growUp = spaceBelow < CONVERSATION_PANEL_HEIGHT - OVERLAY_IDLE_SIZE;

            newX = growLeft ? logicalX + OVERLAY_IDLE_SIZE - CONVERSATION_PANEL_WIDTH : logicalX;
            newY = growUp ? logicalY + OVERLAY_IDLE_SIZE - CONVERSATION_PANEL_HEIGHT : logicalY;
            newX = Math.max(workX, Math.min(newX, workX + workW - CONVERSATION_PANEL_WIDTH));
            newY = Math.max(workY, Math.min(newY, workY + workH - CONVERSATION_PANEL_HEIGHT));
          }

          await win.setSize(new LogicalSize(CONVERSATION_PANEL_WIDTH, CONVERSATION_PANEL_HEIGHT));
          await win.setPosition(new LogicalPosition(newX, newY));
        } catch {
          // Best-effort — worst case the panel renders at whatever size/
          // position the window ends up at.
        }
      } else if (originalPosRef.current) {
        const pos = originalPosRef.current;
        originalPosRef.current = null;
        try {
          await win.setSize(new LogicalSize(OVERLAY_IDLE_SIZE, OVERLAY_IDLE_SIZE));
          await win.setPosition(new LogicalPosition(pos.x, pos.y));
        } catch {
          // ignore
        }
      }
    })();
  }, [conversation.isActive]);

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
        const [currentVersion, lastSeen, openAfterUpdate] = await Promise.all([
          getVersion(),
          getSetting<string>("lastSeenVersion"),
          getSetting<boolean>("openSettingsAfterUpdate"),
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

  // Hotkey integration
  useHotkey({
    shortcut: settings.dictationKey,
    activationMode: settings.activationMode,
    onToggle: () => toggle(settings.selectedMicDeviceId || undefined),
    onPushStart: () => start(settings.selectedMicDeviceId || undefined),
    onPushEnd: () => stop(),
    enabled: loaded && !!settings.dictationKey && !hotkeyCapturing,
  });

  // Conversation hotkey stays live regardless of trigger mode — auto mode
  // still fires on turn-end, but this lets you force a fresh suggestion
  // mid-turn without waiting for a pause. Only registered while a
  // conversation is actually running, so it never steals a keybinding
  // during ordinary dictation.
  useHotkey({
    shortcut: settings.conversationHotkey,
    activationMode: "tap",
    onToggle: () => conversation.forceSuggestion(),
    enabled: loaded && !!settings.conversationHotkey && !hotkeyCapturing && conversation.isActive,
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
            : t("overlay.menu.startConversation"),
          action: () => {
            if (conversation.isActive) {
              conversation.stop();
            } else {
              setConversationConsentRequested(true);
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

  // Audio level visualization — scale the button ring
  const levelScale = 1 + audioLevel * 0.3;

  return (
    <>
    <style>{`
      @keyframes pulse-mic {
        0%, 100% { transform: scale(1); opacity: 1; }
        50% { transform: scale(1.15); opacity: 0.7; }
      }
      @keyframes breathe {
        0%, 100% { transform: scale(1); }
        50% { transform: scale(1.05); }
      }
    `}</style>
    <ConversationConsentModal
      requested={conversationConsentRequested}
      onAccept={async () => {
        setConversationConsentRequested(false);
        await conversation.start();
      }}
      onCancel={() => setConversationConsentRequested(false)}
    />
    {conversation.isActive ? (
      <div className="dictation-window flex flex-col h-screen bg-surface-0 border border-border rounded-control overflow-hidden pointer-events-auto">
        <div
          data-tauri-drag-region
          className="flex items-center justify-between px-3 py-2 border-b border-border-subtle shrink-0 cursor-move"
        >
          <span className="text-xs font-medium text-foreground-bright truncate">
            {conversation.activePersona?.name ?? t("overlay.conversation.title")}
          </span>
          <button
            onClick={() => conversation.stop()}
            className="w-6 h-6 flex items-center justify-center rounded-inner hover:bg-destructive/20 text-muted-foreground hover:text-destructive transition-colors"
            aria-label={t("overlay.menu.stopConversation")}
          >
            <Square className="w-3 h-3" />
          </button>
        </div>

        <div className="flex-1 overflow-y-auto px-3 py-2 space-y-1.5">
          {conversation.transcript.length === 0 && (
            <p className="text-xs text-muted-foreground">{t("overlay.conversation.listening")}</p>
          )}
          {conversation.transcript.map((u) => (
            <div key={u.id} className="text-xs leading-relaxed">
              <span className={u.channel === "me" ? "text-primary font-medium" : "text-accent font-medium"}>
                {u.channel === "me" ? t("conversation.live.me") : t("conversation.live.them")}:
              </span>{" "}
              <span className="text-foreground">{u.text}</span>
            </div>
          ))}
        </div>

        {conversation.suggestion && (
          <div className="px-3 py-2 border-t border-border-subtle bg-primary/5 shrink-0">
            <p className="text-[9px] uppercase tracking-wide text-primary/80 mb-0.5">{t("conversation.live.suggestion")}</p>
            <p className="text-xs text-foreground leading-relaxed">{conversation.suggestion.text}</p>
          </div>
        )}

        <button
          onClick={conversation.forceSuggestion}
          disabled={conversation.isGenerating}
          className="flex items-center justify-center gap-1.5 px-3 py-2 border-t border-border-subtle text-xs font-medium text-primary hover:bg-primary/10 transition-colors shrink-0 disabled:opacity-50"
        >
          <Sparkles className="w-3.5 h-3.5" />
          {conversation.isGenerating ? t("conversation.live.generating") : t("conversation.live.suggestNow")}
        </button>
      </div>
    ) : (
    <div
      className="dictation-window flex flex-col items-center justify-center h-screen pointer-events-none"
    >
      {/* Button area */}
      <div className="relative flex items-center justify-center pointer-events-auto" onContextMenu={handleContextMenu}>
        {/* Outer glow ring for audio level */}
        <div
          className="absolute transition-transform duration-75"
          style={{
            width: "3.5rem",
            height: "3.5rem",
            borderRadius: "50%",
            transform: `scale(${isRecording ? levelScale : 1})`,
            background: isRecording
              ? `radial-gradient(circle, hsl(312, 100%, 58%, ${0.2 + audioLevel * 0.3}), transparent 70%)`
              : "transparent",
          }}
        />

        {/* Main button */}
        <button
          onPointerDown={handleButtonPointerDown}
          onPointerMove={handleButtonPointerMove}
          onPointerUp={handleButtonPointerUp}
          disabled={isProcessing}
          style={!isRecording && !isProcessing ? { animation: "breathe 3s ease-in-out infinite" } : undefined}
          className={`relative w-12 h-12 rounded-full border-2 transition-all duration-200 ${
            isProcessing
              ? "bg-surface-1 border-foreground/30 cursor-wait shadow-md shadow-black/50"
              : isRecording
                ? "bg-recording border-foreground-bright shadow-lg shadow-recording/40"
                : "bg-primary border-foreground-bright/80 shadow-md shadow-black/50 hover:border-foreground-bright hover:shadow-lg hover:shadow-primary/40 active:scale-95"
          }`}
          aria-label={
            isProcessing
              ? t("overlay.processing")
              : isRecording
                ? t("overlay.stopRecording")
                : t("overlay.startRecording")
          }
        >
          <div className="flex items-center justify-center">
            {isProcessing ? (
              <LoadingDots />
            ) : (
              <Mic
                className={`w-6 h-6 ${isRecording ? "text-recording-foreground" : "text-foreground-bright"}`}
                style={isRecording ? { animation: "pulse-mic 1.2s ease-in-out infinite" } : undefined}
              />
            )}
          </div>
          {updateAvailable && (
            <span className="absolute top-0.5 right-0.5 w-2.5 h-2.5 rounded-full bg-warning animate-pulse" title={t("overlay.updateAvailable")} />
          )}
        </button>
      </div>
    </div>
    )}
    </>
  );
}

export default function DictationOverlay() {
  return <DictationOverlayInner />;
}
