import { useCallback, useEffect, useRef, useState } from "react";
import {
  startNoteCapture,
  pauseNoteCapture,
  resumeNoteCapture,
  stopNoteCapture,
  getNoteCaptureError,
  onNoteUtterance,
  onNoteError,
  onNoteStarted,
  onNoteStopped,
  getNote,
  type NoteUtteranceEvent,
} from "@/services/tauriApi";
import type { Settings } from "@/hooks/useSettings";
import { extractAndStoreMemory } from "@/services/memory";

interface UseNoteCaptureOptions {
  groqApiKey: string;
  micDeviceId?: string;
  settings: Settings;
  onToast?: (props: { title?: string; description?: string }) => void;
}

/** Mirrors useConversation's shape (broadcast events so any window stays in
 *  sync) but single-channel with explicit pause/resume instead of
 *  personas/suggestions. See commands/notes.rs for the backend side. */
export function useNoteCapture({ groqApiKey, micDeviceId, settings, onToast }: UseNoteCaptureOptions) {
  const [noteId, setNoteId] = useState<number | null>(null);
  const [isPaused, setIsPaused] = useState(false);
  const [utterances, setUtterances] = useState<NoteUtteranceEvent[]>([]);

  const noteIdRef = useRef<number | null>(null);
  noteIdRef.current = noteId;
  const settingsRef = useRef(settings);
  settingsRef.current = settings;
  const memoryExtractionInFlightRef = useRef(new Set<number>());

  // See useConversation.ts's identical onToastRef for why: an inline
  // `onToast` gets a new identity every render, and every incoming
  // utterance causes a re-render — if that identity were a dependency of
  // the listener-wiring effect below, each utterance would tear down and
  // re-register the `listen()` calls, and an event landing in that async
  // gap would be silently dropped.
  const onToastRef = useRef(onToast);
  onToastRef.current = onToast;

  const scheduleMemoryExtraction = useCallback((stoppedId: number) => {
    if (
      !settingsRef.current.automaticMemoryEnabled
      || memoryExtractionInFlightRef.current.has(stoppedId)
    ) return;
    memoryExtractionInFlightRef.current.add(stoppedId);
    globalThis.setTimeout(() => {
      void getNote(stoppedId)
        .then((note) => extractAndStoreMemory(
          note.raw_transcript,
          "note",
          stoppedId,
          settingsRef.current,
        ))
        .catch(() => undefined)
        .finally(() => memoryExtractionInFlightRef.current.delete(stoppedId));
    }, 1_000);
  }, []);

  useEffect(() => {
    const unlistenUtterance = onNoteUtterance((payload) => {
      if (payload.note_id !== noteIdRef.current) return;
      setUtterances((prev) => [...prev, payload]);
    });
    const unlistenError = onNoteError((payload) => {
      if (payload.note_id !== noteIdRef.current) return;
      onToastRef.current?.({ description: payload.message });
    });
    const unlistenStarted = onNoteStarted((id) => {
      if (noteIdRef.current != null) return;
      setUtterances([]);
      setIsPaused(false);
      setNoteId(id);
    });
    const unlistenStopped = onNoteStopped(() => {
      const stoppedId = noteIdRef.current;
      setNoteId(null);
      setIsPaused(false);
      if (stoppedId != null) scheduleMemoryExtraction(stoppedId);
    });

    return () => {
      unlistenUtterance.then((fn) => fn());
      unlistenError.then((fn) => fn());
      unlistenStarted.then((fn) => fn());
      unlistenStopped.then((fn) => fn());
    };
  }, [scheduleMemoryExtraction]);

  const start = useCallback(
    async (existingNoteId?: number) => {
      if (!groqApiKey) {
        onToastRef.current?.({ description: "Notes need a Groq API key (used for transcription)." });
        return;
      }
      setUtterances([]);
      setIsPaused(false);
      const id = await startNoteCapture(micDeviceId || undefined, groqApiKey, existingNoteId);
      setNoteId(id);

      // Same rationale as useConversation.start: the capture thread builds
      // its cpal stream asynchronously, so a setup failure (no mic) only
      // shows up a moment later.
      setTimeout(() => {
        getNoteCaptureError()
          .then((err) => {
            if (err) onToastRef.current?.({ description: err });
          })
          .catch(() => {});
      }, 750);

      return id;
    },
    [groqApiKey, micDeviceId],
  );

  const pause = useCallback(async () => {
    await pauseNoteCapture();
    setIsPaused(true);
  }, []);

  const resume = useCallback(async () => {
    await resumeNoteCapture();
    setIsPaused(false);
  }, []);

  const stop = useCallback(async () => {
    const stoppedId = noteIdRef.current;
    if (stoppedId == null) return;
    await stopNoteCapture();
    scheduleMemoryExtraction(stoppedId);
    setNoteId(null);
    setIsPaused(false);
  }, [scheduleMemoryExtraction]);

  return {
    noteId,
    isActive: noteId != null,
    isPaused,
    utterances,
    start,
    pause,
    resume,
    stop,
  };
}
