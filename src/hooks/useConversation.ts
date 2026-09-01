import { useCallback, useEffect, useRef, useState } from "react";
import {
  startConversation,
  stopConversation,
  generateSuggestion,
  getConversationError,
  onConversationUtterance,
  onConversationSuggestion,
  onConversationError,
  onConversationStarted,
  onConversationStopped,
  getConversation,
  type ConversationUtteranceEvent,
  type ConversationSuggestionEvent,
} from "@/services/tauriApi";
import type { Persona } from "@/models/persona";
import type { Settings } from "@/hooks/useSettings";
import { extractAndStoreMemory } from "@/services/memory";

interface UseConversationOptions {
  settings: Settings;
  /** Reasoning model/provider/key to generate suggestions with — independent
   *  of AI enhancement's model, see conversationReasoningProvider/Model on
   *  Settings. */
  reasoningModel: string;
  reasoningProvider: string;
  reasoningApiKey: string;
  groqApiKey: string;
  /** Whether this instance should auto-fire a suggestion when the other
   *  side finishes a turn (trigger mode "auto"). Exactly one mounted
   *  instance per conversation should have this on — multiple auto-firing
   *  instances would generate duplicate suggestions for the same turn.
   *  The dedicated Conversation window is the auto-firing owner; any other
   *  place that mounts this hook just to track status/expose the hotkey
   *  should pass false. Defaults to true. */
  autoTrigger?: boolean;
  onToast?: (props: { title?: string; description?: string }) => void;
}

export function useConversation({
  settings,
  reasoningModel,
  reasoningProvider,
  reasoningApiKey,
  groqApiKey,
  autoTrigger = true,
  onToast,
}: UseConversationOptions) {
  const [conversationId, setConversationId] = useState<number | null>(null);
  const [transcript, setTranscript] = useState<ConversationUtteranceEvent[]>([]);
  const [suggestion, setSuggestion] = useState<ConversationSuggestionEvent | null>(null);
  const [isGenerating, setIsGenerating] = useState(false);

  const activePersona = settings.personas.find((p) => p.id === settings.activePersonaId) ?? settings.personas[0];
  const activePersonaRef = useRef<Persona | undefined>(activePersona);
  activePersonaRef.current = activePersona;

  const conversationIdRef = useRef<number | null>(null);
  conversationIdRef.current = conversationId;

  const reasoningRef = useRef({ reasoningModel, reasoningProvider, reasoningApiKey });
  reasoningRef.current = { reasoningModel, reasoningProvider, reasoningApiKey };

  const triggerModeRef = useRef(settings.conversationTriggerMode);
  triggerModeRef.current = settings.conversationTriggerMode;
  const settingsRef = useRef(settings);
  settingsRef.current = settings;
  const memoryExtractionInFlightRef = useRef(new Set<number>());

  // Callers (ConversationWindow) pass an inline `onToast` that gets a new
  // identity every render. Reading it through a ref — instead of putting it
  // in dependency arrays — keeps `forceSuggestion` and the listener-wiring
  // effect below stable across renders. Without this, every incoming
  // utterance triggers a state update → re-render → new `onToast` →
  // dependent effect tears down and re-registers its `listen()` calls,
  // which are async round-trips to the backend; an event landing in that
  // gap is silently dropped. That's the mechanism behind "the first couple
  // of sentences transcribed, then nothing" — not a capture bug.
  const onToastRef = useRef(onToast);
  onToastRef.current = onToast;

  const scheduleMemoryExtraction = useCallback((stoppedId: number) => {
    if (
      !autoTrigger
      || !settingsRef.current.automaticMemoryEnabled
      || memoryExtractionInFlightRef.current.has(stoppedId)
    ) return;
    memoryExtractionInFlightRef.current.add(stoppedId);
    // The capture consumer can persist its final VAD chunk shortly after
    // the stopped event. Give it one bounded grace period before reading.
    globalThis.setTimeout(() => {
      void getConversation(stoppedId)
        .then((detail) => extractAndStoreMemory(
          detail.utterances
            .map((utterance) => `${utterance.channel}: ${utterance.text}`)
            .join("\n"),
          "conversation",
          stoppedId,
          settingsRef.current,
        ))
        .catch(() => undefined)
        .finally(() => memoryExtractionInFlightRef.current.delete(stoppedId));
    }, 1_000);
  }, [autoTrigger]);

  const forceSuggestion = useCallback(async () => {
    const id = conversationIdRef.current;
    const persona = activePersonaRef.current;
    if (id == null || !persona) return;
    const { reasoningModel: model, reasoningProvider: provider, reasoningApiKey: apiKey } = reasoningRef.current;
    if (!apiKey) {
      onToastRef.current?.({ description: "No reasoning API key configured for suggestions." });
      return;
    }
    setIsGenerating(true);
    try {
      await generateSuggestion(id, persona.systemPrompt, persona.name, model, provider, apiKey);
      // The suggestion itself arrives via the conversation-suggestion event,
      // not the return value — this just kicks it off.
    } catch (e) {
      onToastRef.current?.({ description: e instanceof Error ? e.message : String(e) });
    } finally {
      setIsGenerating(false);
    }
  }, []);

  // Live event wiring — utterances, suggestions, errors.
  useEffect(() => {
    const unlistenUtterance = onConversationUtterance((payload) => {
      if (payload.conversation_id !== conversationIdRef.current) return;
      setTranscript((prev) => {
        const next = [...prev, payload];
        next.sort((a, b) => a.started_at_ms - b.started_at_ms);
        return next;
      });
      // A finished "them" utterance IS a turn boundary — the backend's VAD
      // already segments on the other side's pause, so there's no separate
      // client-side turn-detection needed here.
      if (autoTrigger && payload.channel === "them" && triggerModeRef.current === "auto") {
        forceSuggestion();
      }
    });
    const unlistenSuggestion = onConversationSuggestion((payload) => {
      if (payload.conversation_id !== conversationIdRef.current) return;
      setSuggestion(payload);
    });
    const unlistenError = onConversationError((payload) => {
      if (payload.conversation_id !== conversationIdRef.current) return;
      onToastRef.current?.({ description: payload.message });
    });
    // Broadcast regardless of which window issued the command — lets any
    // window's UI (and its hotkey registration) pick up a conversation
    // started elsewhere, or notice one it thought was running has ended.
    const unlistenStarted = onConversationStarted((payload) => {
      if (conversationIdRef.current != null) return;
      setTranscript([]);
      setSuggestion(null);
      setConversationId(payload.conversation_id);
    });
    const unlistenStopped = onConversationStopped((stoppedId) => {
      if (stoppedId === conversationIdRef.current) setConversationId(null);
      scheduleMemoryExtraction(stoppedId);
    });

    return () => {
      unlistenUtterance.then((fn) => fn());
      unlistenSuggestion.then((fn) => fn());
      unlistenError.then((fn) => fn());
      unlistenStarted.then((fn) => fn());
      unlistenStopped.then((fn) => fn());
    };
  }, [forceSuggestion, scheduleMemoryExtraction]);

  const start = useCallback(async () => {
    if (!groqApiKey) {
      onToastRef.current?.({ description: "Conversations need a Groq API key (used for both transcription and, optionally, suggestions)." });
      return;
    }
    setTranscript([]);
    setSuggestion(null);
    const id = await startConversation(
      settings.conversationMicDeviceId || undefined,
      groqApiKey,
      activePersonaRef.current?.name,
    );
    setConversationId(id);

    // Capture threads build their cpal stream asynchronously after start()
    // returns, so a setup failure (e.g. no loopback-capable output device)
    // only shows up in state a moment later — check once rather than
    // leaving the user staring at a transcript that will never fill in.
    setTimeout(() => {
      getConversationError()
        .then((err) => {
          if (err) onToastRef.current?.({ description: err });
        })
        .catch(() => {});
    }, 750);

    return id;
  }, [groqApiKey, settings.conversationMicDeviceId]);

  const stop = useCallback(async (title?: string) => {
    const id = conversationIdRef.current;
    if (id == null) return;
    await stopConversation(id, title);
    scheduleMemoryExtraction(id);
    setConversationId(null);
  }, [scheduleMemoryExtraction]);

  return {
    conversationId,
    isActive: conversationId != null,
    transcript,
    suggestion,
    isGenerating,
    activePersona,
    start,
    stop,
    forceSuggestion,
  };
}
