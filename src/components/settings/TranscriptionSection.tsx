import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { SettingsSection, SettingsRow } from "@/components/ui/SettingsSection";
import { Toggle } from "@/components/ui/toggle";
import { getTranscriptionProviders } from "./providerHelpers";
import ProviderModelSelector from "./ProviderModelSelector";
import LiveProviderModelSelector from "./LiveProviderModelSelector";
import { LiveConsentModal } from "@/components/ui/LiveConsentModal";
import type { SectionProps } from "./types";
import { Button } from "@/components/ui/button";
import { listLocalModels, onLocalModelDownloadProgress, resetMemory } from "@/services/tauriApi";

export default function TranscriptionSection({ settings, update, toast }: SectionProps) {
  const { t } = useTranslation();
  const [resettingMemory, setResettingMemory] = useState(false);
  const [hasLocalModel, setHasLocalModel] = useState(false);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    const refresh = () =>
      listLocalModels().then((models) => setHasLocalModel(models.some((model) => model.installed)));
    void refresh();
    void onLocalModelDownloadProgress((event) => {
      if (event.status === "installed" || event.status === "deleted") void refresh();
    }).then((cleanup) => {
      unlisten = cleanup;
    });
    return () => unlisten?.();
  }, []);

  async function handleResetMemory() {
    setResettingMemory(true);
    try {
      await resetMemory();
      toast?.({ description: t("transcription.memory.resetSuccess"), variant: "success" });
    } catch {
      toast?.({ description: t("transcription.memory.resetError"), variant: "destructive" });
    } finally {
      setResettingMemory(false);
    }
  }
  return (
    <SettingsSection title={t("transcription.title")} description={t("transcription.description")}>
      {/* Mode toggle */}
      <div className="space-y-2">
        <label className="text-sm font-medium text-text-secondary">
          {t("transcription.mode.label", { defaultValue: "Dictation Mode" })}
        </label>
        <div
          role="radiogroup"
          aria-label={t("transcription.mode.label", { defaultValue: "Dictation Mode" })}
          className="inline-flex bg-background-secondary p-1 rounded-control border border-border"
        >
          <button
            role="radio"
            aria-checked={settings.dictationMode !== "live"}
            onClick={() => update("dictationMode", "standard")}
            className={`px-3 py-1.5 text-sm rounded-inner transition-all ${
              settings.dictationMode !== "live"
                ? "bg-primary/15 text-primary border border-primary/30"
                : "text-text-secondary"
            }`}
          >
            {t("transcription.mode.standard", { defaultValue: "Standard" })}
          </button>
          <button
            role="radio"
            aria-checked={settings.dictationMode === "live"}
            onClick={() => update("dictationMode", "live")}
            className={`px-3 py-1.5 text-sm rounded-inner transition-all ${
              settings.dictationMode === "live"
                ? "bg-primary/15 text-primary border border-primary/30"
                : "text-text-secondary"
            }`}
          >
            {t("transcription.mode.live", { defaultValue: "Live" })}{" "}
            <span className="text-primary text-[11px]">
              {t("transcription.mode.live.beta", { defaultValue: "Beta" })}
            </span>
          </button>
        </div>
        <p className="text-xs text-text-tertiary">
          {settings.dictationMode === "live"
            ? t("transcription.mode.live.description", {
                defaultValue: "Stream audio in real-time and see text as you speak.",
              })
            : t("transcription.mode.standard.description", {
                defaultValue: "Record then transcribe — best accuracy.",
              })}
        </p>
      </div>

      {/* Standard (Buffered) controls */}
      {settings.dictationMode !== "live" && (
        <>
          <ProviderModelSelector
            providers={getTranscriptionProviders(settings)}
            selectedProvider={settings.cloudTranscriptionProvider}
            selectedModel={
              settings.cloudTranscriptionProvider === "local"
                ? settings.localTranscriptionModel
                : settings.cloudTranscriptionModel
            }
            registryKey="transcriptionProviders"
            openRouterDefault="openai/gpt-audio-mini"
            onProviderChange={(id) => update("cloudTranscriptionProvider", id)}
            onModelChange={(model, providerId) => {
              if ((providerId ?? settings.cloudTranscriptionProvider) === "local") {
                update("localTranscriptionModel", model);
              } else {
                update("cloudTranscriptionModel", model);
              }
            }}
            settings={settings}
            update={update}
          />
          <SettingsRow
            label={t("transcription.localFallback.label")}
            description={
              hasLocalModel
                ? t("transcription.localFallback.description")
                : t("transcription.localFallback.unavailable")
            }
          >
            <Toggle
              checked={settings.localFallbackEnabled && hasLocalModel}
              disabled={!hasLocalModel}
              onChange={(value) => update("localFallbackEnabled", value)}
            />
          </SettingsRow>
          <SettingsRow
            label={t("transcription.contextualCorrection.label")}
            description={t("transcription.contextualCorrection.description")}
          >
            <Toggle
              checked={settings.contextualCorrectionEnabled}
              onChange={(value) => update("contextualCorrectionEnabled", value)}
            />
          </SettingsRow>
        </>
      )}

      {/* Live mode controls */}
      {settings.dictationMode === "live" && (
        <div className="space-y-4">
          <LiveProviderModelSelector settings={settings} update={update} />
          <SettingsRow
            label={t("transcription.live.enhancement.label", {
              defaultValue: "Polish text on stop",
            })}
            description={t("transcription.live.enhancement.description", {
              defaultValue:
                "Re-run AI enhancement when you stop. Replaces the live-typed text with the polished version (visible backspace + retype). Turn off to leave words exactly as spoken.",
            })}
          >
            <Toggle
              checked={settings.liveEnhancement}
              onChange={(v) => update("liveEnhancement", v)}
            />
          </SettingsRow>
          <p className="text-xs text-text-tertiary">
            {t("transcription.live.description", {
              defaultValue:
                "Live dictation streams audio directly to a provider for real-time transcription.",
            })}
          </p>
          <LiveConsentModal
            mode={settings.dictationMode}
            provider={settings.liveTranscriptionProvider || "openai"}
          />
        </div>
      )}

      <SettingsRow
        label={t("transcription.memory.label")}
        description={t("transcription.memory.description")}
      >
        <Toggle
          checked={settings.automaticMemoryEnabled}
          onChange={(value) => update("automaticMemoryEnabled", value)}
        />
      </SettingsRow>
      <SettingsRow
        label={t("transcription.memory.resetLabel")}
        description={t("transcription.memory.resetDescription")}
      >
        <Button
          variant="outline"
          size="sm"
          disabled={resettingMemory}
          onClick={() => void handleResetMemory()}
        >
          {t("transcription.memory.reset")}
        </Button>
      </SettingsRow>
    </SettingsSection>
  );
}
