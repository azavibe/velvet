import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { Play, Square, Pause, Sparkles, X, Minus, Pin, PinOff, Copy } from "lucide-react";
import { useSettings } from "@/hooks/useSettings";
import { useConversation } from "@/hooks/useConversation";
import { useNoteCapture } from "@/hooks/useNoteCapture";
import { ConversationConsentModal } from "@/components/ui/ConversationConsentModal";
import { ToastProvider, useToast } from "@/components/ui/Toast";
import { setClipboardText, setNoteTitle as saveNoteTitle, onNoteAppendRequested } from "@/services/tauriApi";
import StyledSelect from "@/components/ui/StyledSelect";

/** Synthetic persona-dropdown entry — not a real Persona, never persisted.
 *  Picking it swaps the window into note-taking mode (see `mode` state
 *  below) instead of selecting `activePersonaId`. */
const NOTE_MODE_VALUE = "__note__";

function ConversationWindowInner() {
  const { t } = useTranslation();
  const { settings, update, loaded } = useSettings();
  const { toast } = useToast();
  const [consentRequested, setConsentRequested] = useState(false);
  const [mode, setMode] = useState<"conversation" | "note">("conversation");
  const [noteTitle, setNoteTitle] = useState("");
  // The window starts alwaysOnTop (tauri.conf.json) so it doesn't get lost
  // behind a call app the moment it opens — this just lets the user drop
  // that once they've got it positioned where they want it.
  const [pinned, setPinned] = useState(true);

  const onCaptureToast = useCallback(
    (props: { title?: string; description?: string }) => toast({ ...props, variant: "destructive" }),
    [toast],
  );

  const conversation = useConversation({
    settings,
    reasoningModel: settings.conversationReasoningModel,
    reasoningProvider: settings.conversationReasoningProvider,
    reasoningApiKey:
      (settings[`${settings.conversationReasoningProvider}ApiKey` as keyof typeof settings] as string) ?? "",
    groqApiKey: settings.groqApiKey,
    autoTrigger: true,
    onToast: onCaptureToast,
  });

  const note = useNoteCapture({
    groqApiKey: settings.groqApiKey,
    micDeviceId: settings.conversationMicDeviceId,
    onToast: onCaptureToast,
  });

  // Reset the title field whenever a fresh note starts (an "Append
  // Dictation" resume would ideally show the existing title, but that's a
  // History-card entry point outside this window for now).
  useEffect(() => {
    if (!note.isActive) setNoteTitle("");
  }, [note.isActive]);

  // "Append Dictation" from a History card (Settings window) asks this
  // window to switch into note mode and resume capture into that note.
  useEffect(() => {
    const unlisten = onNoteAppendRequested((noteId) => {
      setMode("note");
      note.start(noteId);
    });
    return () => {
      unlisten.then((fn) => fn());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const handleClose = useCallback(() => {
    getCurrentWebviewWindow().hide();
  }, []);
  const handleMinimize = useCallback(() => {
    getCurrentWebviewWindow().minimize();
  }, []);
  const handleTogglePin = useCallback(() => {
    setPinned((prev) => {
      const next = !prev;
      getCurrentWebviewWindow().setAlwaysOnTop(next);
      return next;
    });
  }, []);

  // Reflect the window's actual alwaysOnTop state on mount, in case it
  // differs from our default assumption (e.g. a future config change).
  useEffect(() => {
    getCurrentWebviewWindow()
      .isAlwaysOnTop()
      .then(setPinned)
      .catch(() => {});
  }, []);

  const handleToggle = () => {
    if (conversation.isActive) {
      conversation.stop();
    } else {
      setConsentRequested(true);
    }
  };

  const handleNoteTitleBlur = () => {
    if (note.noteId != null && noteTitle.trim()) {
      saveNoteTitle(note.noteId, noteTitle.trim());
    }
  };

  const handleCopyNote = async () => {
    const text = note.utterances.map((u) => u.text).join(" ");
    try {
      await setClipboardText(text);
      toast({ title: t("notes.copied"), variant: "success" });
    } catch (e) {
      toast({ title: t("notes.copyFailed"), description: String(e), variant: "destructive" });
    }
  };

  if (!loaded) return null;

  const dropdownValue = mode === "note" ? NOTE_MODE_VALUE : settings.activePersonaId;
  const dropdownOptions = [
    ...settings.personas.map((p) => ({ value: p.id, label: p.name })),
    { value: NOTE_MODE_VALUE, label: t("notes.takeNote") },
  ];
  const handleDropdownChange = (v: string) => {
    if (v === NOTE_MODE_VALUE) {
      setMode("note");
    } else {
      setMode("conversation");
      update("activePersonaId", v);
    }
  };

  return (
    <div className="h-screen flex flex-col bg-background text-foreground" onContextMenu={(e) => e.preventDefault()}>
      <ConversationConsentModal
        requested={consentRequested}
        onAccept={async () => {
          setConsentRequested(false);
          await conversation.start();
        }}
        onCancel={() => setConsentRequested(false)}
      />

      {/* Custom titlebar — matches SettingsPanel's style */}
      <div
        data-tauri-drag-region
        className="h-8 flex items-center justify-between px-3 bg-background select-none shrink-0 border-b border-border-subtle"
      >
        <span className="text-[13px] font-medium tracking-wide text-muted-foreground truncate">
          {mode === "note" ? t("notes.takeNote") : (conversation.activePersona?.name ?? t("overlay.conversation.title"))}
        </span>
        <div className="flex items-center gap-1">
          <button
            onClick={handleTogglePin}
            title={pinned ? t("conversation.live.unpin") : t("conversation.live.pin")}
            className={`w-6 h-6 flex items-center justify-center rounded-inner transition-colors ${
              pinned ? "text-primary hover:bg-primary/10" : "text-muted-foreground hover:bg-surface-raised"
            }`}
          >
            {pinned ? <Pin className="w-3 h-3" /> : <PinOff className="w-3 h-3" />}
          </button>
          <button onClick={handleMinimize} className="w-6 h-6 flex items-center justify-center rounded-inner hover:bg-surface-raised transition-colors">
            <Minus className="w-3 h-3 text-muted-foreground" />
          </button>
          <button onClick={handleClose} className="w-6 h-6 flex items-center justify-center rounded-inner hover:bg-destructive/20 transition-colors">
            <X className="w-3 h-3 text-muted-foreground" />
          </button>
        </div>
      </div>

      {mode === "note" ? (
        <div className="p-3 flex flex-col gap-3 flex-1 min-h-0">
          <div className="flex items-center gap-2 shrink-0">
            <StyledSelect
              value={dropdownValue}
              onChange={handleDropdownChange}
              options={dropdownOptions}
              className="flex-1 min-w-0"
            />
          </div>

          <input
            type="text"
            value={noteTitle}
            onChange={(e) => setNoteTitle(e.target.value)}
            onBlur={handleNoteTitleBlur}
            placeholder={t("notes.titlePlaceholder")}
            className="h-8 px-2.5 text-sm bg-surface-1 border border-border rounded-control text-foreground placeholder:text-muted-foreground shrink-0"
          />

          <div className="flex items-center gap-2 shrink-0">
            <button
              onClick={() => (note.isActive ? note.stop() : note.start())}
              className={`flex-1 flex items-center justify-center gap-1.5 h-8 rounded-control text-sm font-medium transition-colors ${
                note.isActive
                  ? "bg-destructive/15 text-destructive hover:bg-destructive/25"
                  : "bg-primary text-primary-foreground hover:bg-primary/90"
              }`}
            >
              {note.isActive ? <Square className="w-3.5 h-3.5" /> : <Play className="w-3.5 h-3.5" />}
              {note.isActive ? t("notes.stop") : t("notes.start")}
            </button>
            {note.isActive && (
              <button
                onClick={() => (note.isPaused ? note.resume() : note.pause())}
                className="flex items-center justify-center gap-1.5 h-8 px-3 rounded-control text-sm font-medium border border-border text-foreground hover:bg-surface-1 transition-colors"
              >
                {note.isPaused ? <Play className="w-3.5 h-3.5" /> : <Pause className="w-3.5 h-3.5" />}
                {note.isPaused ? t("notes.resume") : t("notes.pause")}
              </button>
            )}
          </div>

          <div className="flex-1 min-h-0 overflow-y-auto rounded-control border border-border-subtle bg-surface-1/50 p-2.5">
            {!note.isActive && note.utterances.length === 0 && (
              <p className="text-xs text-muted-foreground">{t("notes.description")}</p>
            )}
            {note.isActive && note.isPaused && (
              <p className="text-xs text-muted-foreground mb-2">{t("notes.pausedHint")}</p>
            )}
            {note.isActive && note.utterances.length === 0 && !note.isPaused && (
              <p className="text-xs text-muted-foreground">{t("overlay.conversation.listening")}</p>
            )}
            <p className="text-sm text-foreground leading-relaxed whitespace-pre-line">
              {note.utterances.map((u) => u.text).join(" ")}
            </p>
          </div>

          <button
            onClick={handleCopyNote}
            disabled={note.utterances.length === 0}
            className="flex items-center justify-center gap-1.5 h-8 rounded-control text-sm font-medium text-primary border border-primary/30 hover:bg-primary/10 transition-colors shrink-0 disabled:opacity-50"
          >
            <Copy className="w-3.5 h-3.5" />
            {t("notes.copy")}
          </button>
        </div>
      ) : (
        <div className="p-3 flex flex-col gap-3 flex-1 min-h-0">
          {/* Persona + trigger mode, compact single row */}
          <div className="flex items-center gap-2 shrink-0">
            <StyledSelect
              value={dropdownValue}
              onChange={handleDropdownChange}
              options={dropdownOptions}
              className="flex-1 min-w-0"
            />
            <div className="flex p-0.5 rounded-control bg-surface-1 shrink-0">
              {(["auto", "hotkey"] as const).map((m) => (
                <button
                  key={m}
                  onClick={() => update("conversationTriggerMode", m)}
                  title={t(`conversation.trigger.mode.${m}`)}
                  className={`px-2 py-1 text-[11px] font-medium rounded-inner transition-all duration-150 ${
                    settings.conversationTriggerMode === m
                      ? "bg-primary/15 text-primary border border-primary/30"
                      : "text-muted-foreground hover:text-foreground border border-transparent"
                  }`}
                >
                  {t(`conversation.trigger.mode.${m}`)}
                </button>
              ))}
            </div>
          </div>

          <button
            onClick={handleToggle}
            className={`flex items-center justify-center gap-1.5 h-8 rounded-control text-sm font-medium transition-colors shrink-0 ${
              conversation.isActive
                ? "bg-destructive/15 text-destructive hover:bg-destructive/25"
                : "bg-primary text-primary-foreground hover:bg-primary/90"
            }`}
          >
            {conversation.isActive ? <Square className="w-3.5 h-3.5" /> : <Play className="w-3.5 h-3.5" />}
            {conversation.isActive ? t("conversation.live.stop") : t("conversation.live.start")}
          </button>

          <div className="flex-1 min-h-0 overflow-y-auto rounded-control border border-border-subtle bg-surface-1/50 p-2.5 space-y-1.5">
            {!conversation.isActive && (
              <p className="text-xs text-muted-foreground">{t("conversation.live.description")}</p>
            )}
            {conversation.isActive && conversation.transcript.length === 0 && (
              <p className="text-xs text-muted-foreground">{t("overlay.conversation.listening")}</p>
            )}
            {conversation.transcript.map((u) => (
              <div key={u.id} className="text-sm leading-relaxed">
                <span className={u.channel === "me" ? "text-primary font-medium" : "text-accent font-medium"}>
                  {u.channel === "me" ? t("conversation.live.me") : t("conversation.live.them")}:
                </span>{" "}
                <span className="text-foreground">{u.text}</span>
              </div>
            ))}
          </div>

          {conversation.isActive && (
            <>
              {conversation.suggestion && (
                <div className="rounded-control border border-primary/30 bg-primary/5 p-2.5 shrink-0">
                  <p className="text-[9px] uppercase tracking-wide text-primary/80 mb-0.5">{t("conversation.live.suggestion")}</p>
                  <p className="text-sm text-foreground leading-relaxed">{conversation.suggestion.text}</p>
                </div>
              )}
              <button
                onClick={conversation.forceSuggestion}
                disabled={conversation.isGenerating}
                className="flex items-center justify-center gap-1.5 h-8 rounded-control text-sm font-medium text-primary border border-primary/30 hover:bg-primary/10 transition-colors shrink-0 disabled:opacity-50"
              >
                <Sparkles className="w-3.5 h-3.5" />
                {conversation.isGenerating ? t("conversation.live.generating") : t("conversation.live.suggestNow")}
              </button>
            </>
          )}
        </div>
      )}
    </div>
  );
}

export default function ConversationWindow() {
  return (
    <ToastProvider>
      <ConversationWindowInner />
    </ToastProvider>
  );
}
