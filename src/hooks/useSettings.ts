import { useState, useEffect, useCallback } from "react";
import { emit, listen } from "@tauri-apps/api/event";
import {
  getSetting,
  setSetting,
  getApiKey,
  setApiKey,
  getAgentName,
  setAgentName as setAgentNameApi,
  getCustomDictionary,
  setCustomDictionary as setCustomDictionaryApi,
  getAgentAliases,
  setAgentAliases as setAgentAliasesApi,
} from "@/services/tauriApi";
import type { EnhancementIntensity } from "@/config/prompts";
import type { DictionaryEntry } from "@/models/dictionary";
import { DEFAULT_PERSONAS, type Persona } from "@/models/persona";
import { replaceIfDeprecated } from "@/models/deprecatedModels";
import { startupMark } from "@/services/startupDiagnostics";

export interface Settings {
  // Transcription
  preferredLanguage: string;
  /** Transcription language mode. "auto" = detect from all languages;
   *  "single" = force preferredLanguage; "bilingual" = constrain to
   *  {preferredLanguage, secondaryLanguage}. */
  languageMode: "auto" | "single" | "bilingual";
  /** Secondary language code, used only in bilingual mode. "" when unset. */
  secondaryLanguage: string;
  cloudTranscriptionProvider: string;
  cloudTranscriptionModel: string;
  customDictionary: DictionaryEntry[];

  // Reasoning
  useReasoningModel: boolean;
  reasoningModel: string;
  reasoningProvider: string;
  enhancementIntensity: EnhancementIntensity;
  useCustomPrompt: boolean;
  customSystemPrompt: string;
  /** Conservative post-ASR correction using bounded dictionary and recent
   *  context. Independent from prose enhancement. */
  contextualCorrectionEnabled: boolean;

  // Hotkey
  dictationKey: string;
  activationMode: "tap" | "push";

  // Output
  autoPaste: boolean;
  soundEnabled: boolean;

  // Live dictation mode
  dictationMode: "standard" | "live";
  liveTranscriptionProvider: string;
  liveTranscriptionModel: string;
  /** If true, Live mode runs AI enhancement on the full transcript when you
   *  stop, then backspaces what was typed and re-types the polished version.
   *  If false, the live-typed text is left as-spoken (no post-stop snap). */
  liveEnhancement: boolean;
  /** Last Live-mode failure message (cleared on successful start). Shown in
   *  the Live readiness banner so silent failures are visible. */
  liveLastError: string;

  // Microphone
  selectedMicDeviceId: string;

  // Agent
  agentName: string;
  agentAliases: string[];

  // Conversations (live call copilot)
  personas: Persona[];
  activePersonaId: string;
  /** "auto" fires a suggestion whenever the other side stops talking;
   *  "hotkey" only fires on demand. Reuses the dictation hotkey — pressing
   *  it during an active conversation triggers a suggestion instead of
   *  starting dictation, since the two never happen at the same time. */
  conversationTriggerMode: "auto" | "hotkey";
  /** Microphone device for conversation capture. Empty = system default.
   *  Kept separate from `selectedMicDeviceId` (dictation) since a call may
   *  reasonably use a different input than everyday dictation. */
  conversationMicDeviceId: string;
  /** Reasoning model for conversation suggestions — deliberately independent
   *  of `reasoningProvider`/`reasoningModel` (dictation's AI enhancement).
   *  Turning enhancement off shouldn't take away the ability to pick a
   *  (possibly pricier) model for live suggestions. */
  conversationReasoningProvider: string;
  conversationReasoningModel: string;

  // Developer
  debugMode: boolean;

  // UI Language
  uiLanguage: string;

  // API keys
  openaiApiKey: string;
  anthropicApiKey: string;
  geminiApiKey: string;
  groqApiKey: string;
  mistralApiKey: string;
  qwenApiKey: string;
  openrouterApiKey: string;
}

/** The agent name shipped as the default before the app was renamed to
 *  Aral — see the migration in `load()`. */
const LEGACY_AGENT_NAME = "Whisperi";

const DEFAULTS: Settings = {
  preferredLanguage: "auto",
  languageMode: "auto",
  secondaryLanguage: "",
  cloudTranscriptionProvider: "openai",
  cloudTranscriptionModel: "gpt-4o-mini-transcribe",
  customDictionary: [],
  useReasoningModel: true,
  reasoningModel: "gpt-5-mini",
  reasoningProvider: "openai",
  enhancementIntensity: "standard",
  useCustomPrompt: false,
  customSystemPrompt: "",
  contextualCorrectionEnabled: false,
  autoPaste: true,
  soundEnabled: true,
  dictationKey: "",
  activationMode: "tap",
  dictationMode: "standard",
  liveTranscriptionProvider: "openai",
  liveTranscriptionModel: "gpt-4o-mini-transcribe",
  liveEnhancement: true,
  liveLastError: "",
  selectedMicDeviceId: "",
  agentName: "Aral",
  agentAliases: [],
  personas: DEFAULT_PERSONAS,
  activePersonaId: DEFAULT_PERSONAS[0].id,
  conversationTriggerMode: "auto",
  conversationMicDeviceId: "",
  conversationReasoningProvider: "openai",
  conversationReasoningModel: "gpt-5-mini",
  debugMode: false,
  uiLanguage: "",  // Empty string = auto-detect
  openaiApiKey: "",
  anthropicApiKey: "",
  geminiApiKey: "",
  groqApiKey: "",
  mistralApiKey: "",
  qwenApiKey: "",
  openrouterApiKey: "",
};

/** Keys stored via setSetting() (not special handlers like agent name / API keys). */
const STORE_KEYS = [
  "preferredLanguage", "languageMode", "secondaryLanguage",
  "cloudTranscriptionProvider", "cloudTranscriptionModel",
  "dictationMode", "liveTranscriptionProvider", "liveTranscriptionModel", "liveEnhancement", "liveLastError",
  "useReasoningModel", "reasoningModel", "reasoningProvider", "enhancementIntensity",
  "useCustomPrompt", "customSystemPrompt",
  "contextualCorrectionEnabled",
  "autoPaste", "soundEnabled", "dictationKey", "activationMode",
  "selectedMicDeviceId", "debugMode", "uiLanguage",
  "personas", "activePersonaId", "conversationTriggerMode", "conversationMicDeviceId",
  "conversationReasoningProvider", "conversationReasoningModel",
] as const satisfies readonly (keyof Settings)[];

const API_PROVIDERS = ["openai", "anthropic", "gemini", "groq", "mistral", "qwen", "openrouter"] as const;

export function useSettings() {
  const [settings, setSettings] = useState<Settings>(DEFAULTS);
  const [loaded, setLoaded] = useState(false);

  // Load all settings from tauri-plugin-store on mount
  useEffect(() => {
    let cancelled = false;

    async function load() {
      startupMark("settings load started", {
        store_keys: STORE_KEYS.length,
        providers: API_PROVIDERS.length,
      });
      // Fetch store-backed settings in parallel
      const storeResults = await Promise.all(
        STORE_KEYS.map((key) => getSetting<Settings[typeof key]>(key)),
      );

      // Fetch special settings
      const [agentNameVal, agentAliases, customDictionary, ...apiKeys] = await Promise.all([
        getAgentName(),
        getAgentAliases(),
        getCustomDictionary(),
        ...API_PROVIDERS.map((p) => getApiKey(p)),
      ]);

      if (cancelled) return;

      // Build resolved settings object
      const resolved: Settings = { ...DEFAULTS };
      STORE_KEYS.forEach((key, i) => {
        if (storeResults[i] != null) {
          (resolved as unknown as Record<string, unknown>)[key] = storeResults[i];
        }
      });
      // Migration: installs predating languageMode have no stored value. Derive
      // it from the existing preferredLanguage so a user who had a fixed
      // language keeps "single" instead of silently flipping to "auto".
      const languageModeIdx = STORE_KEYS.indexOf("languageMode");
      if (storeResults[languageModeIdx] == null) {
        resolved.languageMode = resolved.preferredLanguage === "auto" ? "auto" : "single";
      }
      // Migration: older builds seeded bare "en", which has no entry in
      // languageRegistry.json (only "en-US"/"en-GB"), so the selector showed
      // no selection and getLanguageInstruction fell back to the generic
      // template. Normalize and persist so the pipeline sees the fixed value.
      if (resolved.preferredLanguage === "en") {
        resolved.preferredLanguage = "en-US";
        setSetting("preferredLanguage", "en-US");
      }
      if (resolved.secondaryLanguage === "en") {
        resolved.secondaryLanguage = "en-US";
        setSetting("secondaryLanguage", "en-US");
      }
      // Migration: installs that already saved a personas list (from testing
      // before "Meeting" existed) won't pick up new defaults automatically —
      // only truly missing settings get backfilled. Add it once if absent.
      if (
        Array.isArray(resolved.personas) &&
        !resolved.personas.some((p) => p.id === "meeting")
      ) {
        const meeting = DEFAULT_PERSONAS.find((p) => p.id === "meeting");
        if (meeting) {
          resolved.personas = [meeting, ...resolved.personas];
          setSetting("personas", resolved.personas);
        }
      }

      // Migration: providers retire models, and a stored id pointing at a
      // shut-down model fails the API call rather than degrading — the user
      // just sees enhancement or suggestions stop working. Remap to the
      // provider's own recommended replacement.
      for (const key of ["reasoningModel", "conversationReasoningModel"] as const) {
        const replacement = replaceIfDeprecated(resolved[key]);
        if (replacement !== resolved[key]) {
          resolved[key] = replacement;
          setSetting(key, replacement);
        }
      }

      resolved.agentName = agentNameVal;
      // Migration: the app was renamed Whisperi → Aral, but only the default
      // changed — an install that had the old name persisted would keep
      // addressing an agent by the old app's name. Voice commands key off
      // this exact name, so a stale value makes them silently do nothing.
      if (resolved.agentName === LEGACY_AGENT_NAME) {
        resolved.agentName = DEFAULTS.agentName;
        setAgentNameApi(DEFAULTS.agentName);
      }
      resolved.agentAliases = agentAliases;
      resolved.customDictionary = customDictionary;
      API_PROVIDERS.forEach((provider, i) => {
        (resolved as unknown as Record<string, unknown>)[`${provider}ApiKey`] = apiKeys[i];
      });

      // Persist defaults to store for keys that were missing, so the
      // recording pipeline (which reads from the store independently)
      // always sees the same values the UI shows.
      STORE_KEYS.forEach((key, i) => {
        if (storeResults[i] == null) {
          setSetting(key, resolved[key]);
        }
      });

      setSettings(resolved);
      setLoaded(true);
      startupMark("settings load complete", { configured_api_keys: apiKeys.filter(Boolean).length });
    }

    load();
    return () => { cancelled = true; };
  }, []);

  // Listen for settings changes from other windows
  useEffect(() => {
    const unlisten = listen<{ key: string; value: unknown }>(
      "settings-changed",
      (event) => {
        const { key, value } = event.payload;
        setSettings((prev) => ({ ...prev, [key]: value }));
      }
    );

    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // Helper to update a single setting (persist to store + update state)
  const update = useCallback(
    <K extends keyof Settings>(key: K, value: Settings[K]) => {
      setSettings((prev) => ({ ...prev, [key]: value }));

      // Notify other windows about the change
      emit("settings-changed", { key, value });

      // Persist based on key type
      if (key === "agentName") {
        setAgentNameApi(value as string);
      } else if (key === "agentAliases") {
        setAgentAliasesApi(value as string[]);
      } else if (key === "customDictionary") {
        setCustomDictionaryApi(value as DictionaryEntry[]);
      } else if (key.endsWith("ApiKey")) {
        const provider = key.replace("ApiKey", "");
        setApiKey(provider, value as string);
      } else {
        setSetting(key, value);
      }
    },
    []
  );

  return { settings, update, loaded };
}
