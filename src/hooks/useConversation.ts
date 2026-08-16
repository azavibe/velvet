import { useCallback, useEffect, useRef, useState } from "react";
import {
  startConversation,
  stopConversation,
  generateSuggestion,
  onConversationUtterance,
  onConversationSuggestion,
  onConversationError,
  type ConversationUtteranceEvent,
  type ConversationSuggestionEvent,
} from "@/services/tauriApi";
import type { Persona } from "@/models/persona";
import type { Settings } from "@/hooks/useSettings";

interface UseConversationOptions {
  settings: Settings;
  /** Reasoning model/provider/key to generate suggestions with — reuses
   *  whatever the user has configured for AI enhancement, since a
   *  suggestion is just another reasoning call. */
  reasoningModel: string;
  reasoningProvider: string;
  reasoningApiKey: string;
  groqApiKey: string;
  onToast?: (props: { title?: string; description?: string }) => void;
}

export function useConversation({
  settings,
  reasoningModel,
  reasoningProvider,
  reasoningApiKey,
  groqApiKey,
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
      if (payload.channel === "them" && triggerModeRef.current === "auto") {
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

    return () => {
      unlistenUtterance.then((fn) => fn());
      unlistenSuggestion.then((fn) => fn());
      unlistenError.then((fn) => fn());
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
