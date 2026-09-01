import { useState, useCallback, useRef, useEffect } from "react";
import {
  startRecording as apiStartRecording,
  stopRecording as apiStopRecording,
  pasteText,
  onAudioLevel,
  onRecordingError,
  getSetting,
  saveTranscription,
} from "@/services/tauriApi";
import { playStartSound, playStopSound } from "@/utils/sounds";
import {
  classifyStartFailure,
  surfaceMicWarning,
  clearMicWarning,
} from "@/utils/micWarning";
import {
  loadTranscriptionSettings,
  buildTranscriptionDictionary,
  transcribe,
  retryCommandTranscription,
  enhance,
  reconcile,
  formatOutput,
} from "./useTranscriptionPipeline";
import { applyAlwaysDictionaryCorrections } from "@/models/dictionary";
import { sanitizeAgentWakeEcho } from "@/services/agentEchoSanitizer";
import { extractAndStoreMemory } from "@/services/memory";

/** The backend owns prompt-echo suppression; preserve non-empty dictionary terms
 * because they may be exactly what the user spoke. */
function isEmptyTranscription(text: string): boolean {
  return !text.trim();
}

type RecordingPhase = "idle" | "recording" | "processing";

interface UseAudioRecordingOptions {
  onToast?: (props: {
    title: string;
    description: string;
    variant: "default" | "destructive" | "success";
  }) => void;
  /** Given the provider-normalized text, run it as an app command if it is one.
   *  Returning true means it was handled as a command, so the text is an
   *  instruction rather than dictation and must not be enhanced, pasted, or
   *  saved to history. The persisted name/aliases travel with the completed
   *  transcription so a stale React render cannot use an old wake word. */
  onVoiceCommand?: (
    text: string,
    context: {
      agentName: string;
      agentAliases: string[];
      retryTranscription: (prompt: string) => Promise<string | null>;
      applicationLanguage: string | null;
      durationMs: number | null;
    },
  ) => Promise<boolean>;
}

export function useAudioRecording({ onToast, onVoiceCommand }: UseAudioRecordingOptions = {}) {
  const [phase, setPhase] = useState<RecordingPhase>("idle");
  const [audioLevel, setAudioLevel] = useState(0);
  const [transcript, setTranscript] = useState("");
  const unlistenRef = useRef<(() => void)[]>([]);
  const recordingStartRef = useRef<number | null>(null);
  // Synchronous re-entrancy guard for stop(): the `phase` check alone is
  // stale under two rapid hotkey-release events (React commits state async),
  // which let one recording transcribe and paste twice.
  const stopInFlightRef = useRef(false);
  const phaseRef = useRef<RecordingPhase>(phase);
  phaseRef.current = phase;
  // Read through a ref so `stop`'s identity doesn't change with the caller's
  // callback identity (same reasoning as useConversation's onToastRef).
  const onVoiceCommandRef = useRef(onVoiceCommand);
  onVoiceCommandRef.current = onVoiceCommand;

  // Subscribe to audio-level and recording-error events
  useEffect(() => {
    let cancelled = false;

    async function subscribe() {
      const unlistenLevel = await onAudioLevel((level) => {
        if (!cancelled) setAudioLevel(level);
      });
      const unlistenError = await onRecordingError((error) => {
        if (!cancelled && phaseRef.current === "recording") {
          setPhase("idle");
          recordingStartRef.current = null;
          setAudioLevel(0);
          // Native stop clears both the cpal owner and the tray indicator even
          // when the audio thread itself raised the error.
          void apiStopRecording().catch(() => {});
          onToast?.({
            title: "Recording Error",
            description: error,
            variant: "destructive",
          });
        }
      });
      if (!cancelled) {
        unlistenRef.current = [unlistenLevel, unlistenError];
      } else {
        unlistenLevel();
        unlistenError();
      }
    }

    subscribe();
    return () => {
      cancelled = true;
      unlistenRef.current.forEach((fn) => fn());
      unlistenRef.current = [];
    };
  }, [onToast]);

  const start = useCallback(
    async (deviceId?: string) => {
      if (phase !== "idle") return;
      // Capture before the await so the duration reflects "user pressed
      // hotkey" rather than "Rust finished initializing cpal". The ~50–100ms
      // device-startup overhead is acceptable per the spec; cleared on failure
      // so a failed start doesn't poison the next recording.
      recordingStartRef.current = performance.now();
      try {
        await apiStartRecording(deviceId);
        setPhase("recording");
        void clearMicWarning();
        const soundEnabled = await getSetting<boolean>("soundEnabled");
        if (soundEnabled !== false) playStartSound();
      } catch (e) {
        recordingStartRef.current = null;
        const warning = await classifyStartFailure(deviceId);
        if (warning) {
          await surfaceMicWarning(warning);
        } else {
          onToast?.({
            title: "Failed to start recording",
            description: String(e),
            variant: "destructive",
          });
        }
      }
    },
    [phase, onToast],
  );

  const stop = useCallback(async () => {
    if (phase !== "recording" || stopInFlightRef.current) return;
    stopInFlightRef.current = true;
    setPhase("processing");
    // Capture duration as soon as we know the user released the hotkey,
    // before any awaits that would inflate the recorded length.
    const durationMs =
      recordingStartRef.current !== null
        ? Math.round(performance.now() - recordingStartRef.current)
        : null;
    recordingStartRef.current = null;

    try {
      const soundEnabled = await getSetting<boolean>("soundEnabled");
      if (soundEnabled !== false) playStopSound();

      const audioData = await apiStopRecording();
      setAudioLevel(0);

      const settings = await loadTranscriptionSettings();
      const transcriptionDict = buildTranscriptionDictionary(
        settings.dictionary,
        settings.agentName,
        settings.agentAliases,
      );

      let providerText: string;
      let detectedLanguage: string | null;
      let transcriptionEngine: "cloud" | "local" | "fallback:local";
      try {
        const result = await transcribe(audioData, settings, transcriptionDict);
        providerText = result.text;
        detectedLanguage = result.detectedLanguage;
        transcriptionEngine = result.engine;
      } catch (e) {
        await saveTranscription(
          "",
          null,
          "failed",
          settings.agentName,
          "transcription_failed",
          durationMs,
          audioData,
        );
        throw e;
      }
      if (import.meta.env.DEV || settings.debugMode) {
        console.debug("[Agenda] transcription received", {
          characters: providerText.length,
          detectedLanguage: detectedLanguage ?? null,
        });
      }

      if (isEmptyTranscription(providerText)) {
        await saveTranscription(
          "",
          null,
          "failed",
          settings.agentName,
          "empty_transcription",
          durationMs,
          audioData,
        );
        console.log("[Agenda] Empty transcription archived for retry.");
        if (transcriptionEngine !== "cloud") {
          throw new Error(
            "The local model did not recognize any speech. The recording was saved in History.",
          );
        }
        setPhase("idle");
        return;
      }

      const reconciliation = await reconcile(
        providerText,
        settings,
        detectedLanguage,
      );
      if (import.meta.env.DEV || settings.debugMode) {
        console.debug("[Agenda] contextual reconciliation", {
          status: reconciliation.status,
          confidence: reconciliation.confidence,
          evidence: reconciliation.evidence,
        });
      }

      const reconciledText = reconciliation.text;
      const commandText = sanitizeAgentWakeEcho(
        reconciledText,
        settings.agentName,
        settings.agentAliases,
      );
      const agentEchoRemoved = commandText !== reconciledText;
      if (agentEchoRemoved && (import.meta.env.DEV || settings.debugMode)) {
        console.debug("[Agenda] code=agent_wake_echo_removed", {
          beforeCharacters: reconciledText.length,
          afterCharacters: commandText.length,
        });
      }

      // "Agenda, start notes" is an instruction to the app, not text to type.
      // Detect against provider-normalized text before arbitrary user
      // dictionary replacements. Command-scoped ASR tolerance belongs in the
      // recognizer; a custom nodes→notes rule must not gain permission to
      // launch UI actions. Bails before enhancement, paste, and history.
      if (onVoiceCommandRef.current) {
        const handled = await onVoiceCommandRef.current(commandText, {
          agentName: settings.agentName,
          agentAliases: settings.agentAliases,
          retryTranscription: (prompt) => retryCommandTranscription(audioData, settings, prompt),
          applicationLanguage: settings.uiLanguage ?? null,
          durationMs,
        });
        if (handled) {
          if (import.meta.env.DEV) console.debug("[voice-command] code=command_consumed");
          setPhase("idle");
          return;
        }
      }

      const correctedText = applyAlwaysDictionaryCorrections(
        commandText,
        settings.dictionary,
      );

      let finalText = correctedText;
      let rawAiResponse: string | null = null;
      try {
        const result = await enhance(
          correctedText,
          settings,
          detectedLanguage,
        );
        finalText = result.finalText;
        rawAiResponse = result.rawAiResponse;
      } catch (e) {
        console.error("[Agenda] Enhancement error:", e);
        if (settings.debugMode) {
          finalText = `${correctedText}\n\n[Enhancement Error]\n${e}`;
        }
      }

      const outputText = formatOutput(
        providerText,
        finalText,
        rawAiResponse,
        !!settings.debugMode,
      );
      setTranscript(outputText);

      if (settings.autoPaste !== false) {
        await pasteText(outputText);
      }

      const processingMethods = [
        transcriptionEngine,
        reconciliation.status === "accepted" ? "reconciliation" : null,
        agentEchoRemoved ? "agent-echo" : null,
        rawAiResponse !== null ? "ai" : null,
        finalText === correctedText && correctedText !== commandText ? "dictionary" : null,
      ].filter(Boolean);

      const transcriptionId = await saveTranscription(
        providerText,
        finalText !== providerText ? finalText : null,
        processingMethods.join("+") || "none",
        settings.agentName,
        null,
        durationMs,
        audioData,
        reconciliation.status === "accepted" ? reconciliation.text : null,
        reconciliation.status,
        reconciliation.confidence,
        reconciliation.evidence,
      );

      // Learn only from provider speech, never from correction/enhancement
      // output. Extraction is best-effort and cannot delay normal dictation.
      if (settings.automaticMemoryEnabled) {
        void extractAndStoreMemory(
          providerText,
          "dictation",
          transcriptionId,
          settings,
        );
      }

      setPhase("idle");
    } catch (e) {
      // Backend gate for silent/too-short clips (AudioError::NoSpeech) — an
      // accidental tap or silence hold must reset quietly, not raise a toast.
      if (String(e).includes("No speech detected")) {
        console.log("[Agenda] No speech detected, skipping.");
        setAudioLevel(0);
        setPhase("idle");
        return;
      }
      console.error("[Agenda] Transcription failed:", e);
      onToast?.({
        title: "Transcription Failed",
        description: String(e),
        variant: "destructive",
      });
      setPhase("idle");
    } finally {
      stopInFlightRef.current = false;
    }
  }, [phase, onToast]);

  const toggle = useCallback(
    async (deviceId?: string) => {
      if (phase === "idle") {
        await start(deviceId);
      } else if (phase === "recording") {
        await stop();
      }
      // If processing, ignore toggle
    },
    [phase, start, stop],
  );

  const cancel = useCallback(async () => {
    if (phase === "recording") {
      try {
        await apiStopRecording();
      } catch {
        // ignore
      }
      recordingStartRef.current = null;
      setAudioLevel(0);
      setPhase("idle");
    }
  }, [phase]);

  return {
    phase,
    isRecording: phase === "recording",
    isProcessing: phase === "processing",
    audioLevel,
    transcript,
    start,
    stop,
    toggle,
    cancel,
  };
}
