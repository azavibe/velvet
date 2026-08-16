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
  type ConversationUtteranceEvent,
  type ConversationSuggestionEvent,
} from "@/services/tauriApi";
import type { Persona } from "@/models/persona";
import type { Settings } from "@/hooks/useSettings";

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

  const forceSuggestion = useCallback(async () => {
    const id = conversationIdRef.current;
    const persona = activePersonaRef.current;
    if (id == null || !persona) return;
    const { reasoningModel: model, reasoningProvider: provider, reasoningApiKey: apiKey } = reasoningRef.current;
    if (!apiKey) {
      onToast?.({ description: "No reasoning API key configured for suggestions." });
      return;
    }
    setIsGenerating(true);
    try {
      await generateSuggestion(id, persona.systemPrompt, persona.name, model, provider, apiKey);
      // The suggestion itself arrives via the conversation-suggestion event,
      // not the return value — this just kicks it off.
    } catch (e) {
      onToast?.({ description: e instanceof Error ? e.message : String(e) });
    } finally {
      setIsGenerating(false);
    }
  }, [onToast]);

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
      onToast?.({ description: payload.message });
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
      if (stoppedId !== conversationIdRef.current) return;
      setConversationId(null);
    });

    return () => {
      unlistenUtterance.then((fn) => fn());
      unlistenSuggestion.then((fn) => fn());
      unlistenError.then((fn) => fn());
      unlistenStarted.then((fn) => fn());
      unlistenStopped.then((fn) => fn());
    };
  }, [forceSuggestion, onToast]);

  const start = useCallback(async () => {
    if (!groqApiKey) {
      onToast?.({ description: "Conversations need a Groq API key (used for both transcription and, optionally, suggestions)." });
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
          if (err) onToast?.({ description: err });
        })
        .catch(() => {});
    }, 750);

    return id;
  }, [groqApiKey, settings.conversationMicDeviceId, onToast]);

  const stop = useCallback(async (title?: string) => {
    const id = conversationIdRef.current;
    if (id == null) return;
    await stopConversation(id, title);
    setConversationId(null);
  }, []);

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
