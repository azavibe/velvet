import type { ComponentType } from "react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Plus, Trash2, Play, Square, Handshake, LifeBuoy, Languages, GraduationCap, User, Sparkles } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import StyledSelect from "@/components/ui/StyledSelect";
import { SettingsSection, SettingsRow } from "@/components/ui/SettingsSection";
import { HotkeyInput } from "@/components/ui/HotkeyInput";
import { ConversationConsentModal } from "@/components/ui/ConversationConsentModal";
import { createPersona, type Persona } from "@/models/persona";
import {
  listAudioDevices,
  listConversations,
  deleteConversation,
  type AudioDevice,
  type ConversationSummary,
} from "@/services/tauriApi";
import { useConversation } from "@/hooks/useConversation";
import type { SectionProps } from "./types";

const PERSONA_ICONS: Record<string, ComponentType<{ className?: string }>> = {
  Handshake, LifeBuoy, Languages, GraduationCap, User,
};

function PersonaIcon({ name, className }: { name: string; className?: string }) {
  const Icon = PERSONA_ICONS[name] ?? User;
  return <Icon className={className} />;
}

export default function ConversationsSection({ settings, update, toast }: SectionProps) {
  const { t } = useTranslation();
  const [devices, setDevices] = useState<AudioDevice[]>([]);
  const [history, setHistory] = useState<ConversationSummary[]>([]);
  const [consentRequested, setConsentRequested] = useState(false);
  const [expandedPersonaId, setExpandedPersonaId] = useState<string | null>(null);

  const reasoningApiKey =
    (settings[`${settings.reasoningProvider}ApiKey` as keyof typeof settings] as string) ?? "";

  const conversation = useConversation({
    settings,
    reasoningModel: settings.reasoningModel,
    reasoningProvider: settings.reasoningProvider,
    reasoningApiKey,
    groqApiKey: settings.groqApiKey,
    onToast: (props) => toast?.({ ...props, variant: "destructive" }),
  });

  useEffect(() => {
    listAudioDevices().then(setDevices).catch(() => {});
  }, []);

  const refreshHistory = () => {
    listConversations(20, 0).then(setHistory).catch(() => {});
  };
  useEffect(() => {
    refreshHistory();
  }, [conversation.isActive]);

  const handleToggle = async () => {
    if (conversation.isActive) {
      await conversation.stop();
      refreshHistory();
      return;
    }
    setConsentRequested(true);
  };

  const updatePersona = (id: string, patch: Partial<Persona>) => {
    update("personas", settings.personas.map((p) => (p.id === id ? { ...p, ...patch } : p)));
  };

  const addPersona = () => {
    const p = createPersona({ name: t("conversation.personas.newName") });
    update("personas", [...settings.personas, p]);
    setExpandedPersonaId(p.id);
  };

  const removePersona = (id: string) => {
    const remaining = settings.personas.filter((p) => p.id !== id);
    update("personas", remaining);
    if (settings.activePersonaId === id && remaining[0]) {
      update("activePersonaId", remaining[0].id);
    }
  };

  return (
    <>
      <ConversationConsentModal
        requested={consentRequested}
        onAccept={async () => {
          setConsentRequested(false);
          await conversation.start();
        }}
        onCancel={() => setConsentRequested(false)}
      />

      <SettingsSection title={t("conversation.trigger.title")} description={t("conversation.trigger.description")}>
        <div className="space-y-3">
          <div className="flex p-0.5 rounded-control bg-surface-1 w-fit">
            {(["auto", "hotkey"] as const).map((mode) => (
              <button
                key={mode}
                onClick={() => update("conversationTriggerMode", mode)}
                className={`px-3 py-1.5 text-xs font-medium rounded-inner transition-all duration-150 ${
                  settings.conversationTriggerMode === mode
                    ? "bg-primary/15 text-primary border border-primary/30"
                    : "text-muted-foreground hover:text-foreground border border-transparent"
                }`}
              >
                {t(`conversation.trigger.mode.${mode}`)}
              </button>
            ))}
          </div>
          <p className="text-xs text-muted-foreground">{t("conversation.trigger.hotkeyAlwaysOn")}</p>
          <HotkeyInput value={settings.conversationHotkey} onChange={(hk) => update("conversationHotkey", hk)} />
        </div>
      </SettingsSection>

      <SettingsSection title={t("conversation.mic.title")}>
        <StyledSelect
          value={settings.conversationMicDeviceId}
          onChange={(v) => update("conversationMicDeviceId", v)}
          options={[
            { value: "", label: t("general.mic.systemDefault") },
            ...devices.map((d) => ({
              value: d.id,
              label: `${d.name}${d.is_default ? ` ${t("general.mic.default")}` : ""}`,
            })),
          ]}
          className="w-72"
        />
      </SettingsSection>

      <SettingsSection title={t("conversation.personas.title")} description={t("conversation.personas.description")}>
        <div className="space-y-2">
          {settings.personas.map((persona) => {
            const isActive = persona.id === settings.activePersonaId;
            const isExpanded = expandedPersonaId === persona.id;
            return (
              <div
                key={persona.id}
                className={`rounded-control border p-3 transition-colors ${
                  isActive ? "border-primary/40 bg-primary/5" : "border-border/70 bg-surface-1/50"
                }`}
              >
                <div className="flex items-center gap-2">
                  <button
                    type="button"
                    onClick={() => update("activePersonaId", persona.id)}
                    className="flex items-center gap-2 flex-1 min-w-0 text-left"
                  >
                    <PersonaIcon name={persona.icon} className={`w-4 h-4 shrink-0 ${isActive ? "text-primary" : "text-muted-foreground"}`} />
                    <span className="text-sm font-medium truncate">{persona.name}</span>
                    {isActive && (
                      <span className="text-[10px] uppercase tracking-wide text-primary/80 shrink-0">
                        {t("conversation.personas.active")}
                      </span>
                    )}
                  </button>
                  <button
                    type="button"
                    onClick={() => setExpandedPersonaId(isExpanded ? null : persona.id)}
                    className="text-xs text-muted-foreground hover:text-foreground shrink-0"
                  >
                    {isExpanded ? t("conversation.personas.collapse") : t("conversation.personas.edit")}
                  </button>
                  <button
                    type="button"
                    onClick={() => removePersona(persona.id)}
                    aria-label={t("conversation.personas.remove", { name: persona.name })}
                    className="text-muted-foreground hover:text-destructive transition-colors shrink-0"
                  >
                    <Trash2 className="w-3.5 h-3.5" />
                  </button>
                </div>
                {isExpanded && (
                  <div className="mt-3 space-y-2">
                    <Input
                      value={persona.name}
                      onChange={(e) => updatePersona(persona.id, { name: e.target.value })}
                      className="h-8 text-sm"
                      placeholder={t("conversation.personas.namePlaceholder")}
                    />
                    <textarea
                      value={persona.systemPrompt}
                      onChange={(e) => updatePersona(persona.id, { systemPrompt: e.target.value })}
                      placeholder={t("conversation.personas.promptPlaceholder")}
                      className="w-full px-3 py-2 text-sm bg-surface-1 border border-border rounded-control text-foreground placeholder:text-muted-foreground/60 resize-y min-h-24 focus:outline-none focus:ring-1 focus:ring-primary/20 focus:border-border-active"
                    />
                  </div>
                )}
              </div>
            );
          })}
          <Button variant="outline" size="sm" onClick={addPersona} className="gap-1.5">
            <Plus className="w-3.5 h-3.5" />
            {t("conversation.personas.add")}
          </Button>
        </div>
      </SettingsSection>

      <SettingsSection title={t("conversation.live.title")} description={t("conversation.live.description")}>
        <div className="space-y-3">
          <Button onClick={handleToggle} variant={conversation.isActive ? "destructive" : "default"} size="sm" className="gap-1.5">
            {conversation.isActive ? <Square className="w-3.5 h-3.5" /> : <Play className="w-3.5 h-3.5" />}
            {conversation.isActive ? t("conversation.live.stop") : t("conversation.live.start")}
          </Button>

          {conversation.isActive && (
            <>
              <div className="rounded-control border border-border/70 bg-surface-1/50 p-3 max-h-56 overflow-y-auto space-y-1.5">
                {conversation.transcript.length === 0 && (
                  <p className="text-xs text-muted-foreground">{t("conversation.live.listening")}</p>
                )}
                {conversation.transcript.map((u) => (
                  <div key={u.id} className="text-sm">
                    <span className={u.channel === "me" ? "text-primary font-medium" : "text-accent font-medium"}>
                      {u.channel === "me" ? t("conversation.live.me") : t("conversation.live.them")}:
                    </span>{" "}
                    <span className="text-foreground">{u.text}</span>
                  </div>
                ))}
              </div>

              <div className="flex items-center gap-2">
                <Button
                  onClick={conversation.forceSuggestion}
                  disabled={conversation.isGenerating}
                  variant="outline"
                  size="sm"
                  className="gap-1.5"
                >
                  <Sparkles className="w-3.5 h-3.5" />
                  {conversation.isGenerating ? t("conversation.live.generating") : t("conversation.live.suggestNow")}
                </Button>
                <span className="text-xs text-muted-foreground">{t("conversation.live.personaHint", { name: conversation.activePersona?.name })}</span>
              </div>

              {conversation.suggestion && (
                <div className="rounded-control border border-primary/30 bg-primary/5 p-3">
                  <p className="text-[10px] uppercase tracking-wide text-primary/80 mb-1">{t("conversation.live.suggestion")}</p>
                  <p className="text-sm text-foreground">{conversation.suggestion.text}</p>
                </div>
              )}
            </>
          )}
        </div>
      </SettingsSection>

      <SettingsSection title={t("conversation.history.title")}>
        {history.length === 0 && <p className="text-xs text-muted-foreground">{t("conversation.history.empty")}</p>}
        <div className="space-y-1.5">
          {history.map((c) => (
            <SettingsRow key={c.id} label={c.title || t("conversation.history.untitled")} description={c.started_at}>
              <button
                type="button"
                onClick={async () => {
                  await deleteConversation(c.id);
                  refreshHistory();
                }}
                aria-label={t("conversation.history.delete")}
                className="text-muted-foreground hover:text-destructive transition-colors"
              >
                <Trash2 className="w-3.5 h-3.5" />
              </button>
            </SettingsRow>
          ))}
        </div>
      </SettingsSection>
    </>
  );
}
