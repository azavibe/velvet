import { useCallback, useState } from "react";
import { useTranslation } from "react-i18next";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { Play, Square, Sparkles, X, Minus } from "lucide-react";
import { useSettings } from "@/hooks/useSettings";
import { useConversation } from "@/hooks/useConversation";
import { ConversationConsentModal } from "@/components/ui/ConversationConsentModal";
import { ToastProvider, useToast } from "@/components/ui/Toast";
import StyledSelect from "@/components/ui/StyledSelect";

function ConversationWindowInner() {
  const { t } = useTranslation();
  const { settings, update, loaded } = useSettings();
  const { toast } = useToast();
  const [consentRequested, setConsentRequested] = useState(false);

  const conversation = useConversation({
    settings,
    reasoningModel: settings.conversationReasoningModel,
    reasoningProvider: settings.conversationReasoningProvider,
    reasoningApiKey:
      (settings[`${settings.conversationReasoningProvider}ApiKey` as keyof typeof settings] as string) ?? "",
    groqApiKey: settings.groqApiKey,
    autoTrigger: true,
    onToast: (props) => toast({ ...props, variant: "destructive" }),
  });

  const handleClose = useCallback(() => {
    getCurrentWebviewWindow().hide();
  }, []);
  const handleMinimize = useCallback(() => {
    getCurrentWebviewWindow().minimize();
  }, []);

  const handleToggle = () => {
    if (conversation.isActive) {
      conversation.stop();
    } else {
      setConsentRequested(true);
    }
  };

  if (!loaded) return null;

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
          {conversation.activePersona?.name ?? t("overlay.conversation.title")}
        </span>
        <div className="flex items-center gap-1">
          <button onClick={handleMinimize} className="w-6 h-6 flex items-center justify-center rounded-inner hover:bg-surface-raised transition-colors">
            <Minus className="w-3 h-3 text-muted-foreground" />
          </button>
          <button onClick={handleClose} className="w-6 h-6 flex items-center justify-center rounded-inner hover:bg-destructive/20 transition-colors">
            <X className="w-3 h-3 text-muted-foreground" />
          </button>
        </div>
      </div>

      <div className="p-3 flex flex-col gap-3 flex-1 min-h-0">
        {/* Persona + trigger mode, compact single row */}
        <div className="flex items-center gap-2 shrink-0">
          <StyledSelect
            value={settings.activePersonaId}
            onChange={(v) => update("activePersonaId", v)}
            options={settings.personas.map((p) => ({ value: p.id, label: p.name }))}
            className="flex-1 min-w-0"
          />
          <div className="flex p-0.5 rounded-control bg-surface-1 shrink-0">
            {(["auto", "hotkey"] as const).map((mode) => (
              <button
                key={mode}
                onClick={() => update("conversationTriggerMode", mode)}
                title={t(`conversation.trigger.mode.${mode}`)}
                className={`px-2 py-1 text-[11px] font-medium rounded-inner transition-all duration-150 ${
                  settings.conversationTriggerMode === mode
                    ? "bg-primary/15 text-primary border border-primary/30"
                    : "text-muted-foreground hover:text-foreground border border-transparent"
                }`}
              >
                {t(`conversation.trigger.mode.${mode}`)}
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
