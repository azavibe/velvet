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
  type NoteUtteranceEvent,
} from "@/services/tauriApi";

interface UseNoteCaptureOptions {
  groqApiKey: string;
  micDeviceId?: string;
  onToast?: (props: { title?: string; description?: string }) => void;
}

/** Mirrors useConversation's shape (broadcast events so any window stays in
 *  sync) but single-channel with explicit pause/resume instead of
 *  personas/suggestions. See commands/notes.rs for the backend side. */
export function useNoteCapture({ groqApiKey, micDeviceId, onToast }: UseNoteCaptureOptions) {
  const [noteId, setNoteId] = useState<number | null>(null);
  const [isPaused, setIsPaused] = useState(false);
  const [utterances, setUtterances] = useState<NoteUtteranceEvent[]>([]);

  const noteIdRef = useRef<number | null>(null);
  noteIdRef.current = noteId;

  useEffect(() => {
    const unlistenUtterance = onNoteUtterance((payload) => {
      if (payload.note_id !== noteIdRef.current) return;
      setUtterances((prev) => [...prev, payload]);
    });
    const unlistenError = onNoteError((payload) => {
      if (payload.note_id !== noteIdRef.current) return;
      onToast?.({ description: payload.message });
    });
    const unlistenStarted = onNoteStarted((id) => {
      if (noteIdRef.current != null) return;
      setUtterances([]);
      setIsPaused(false);
      setNoteId(id);
    });
    const unlistenStopped = onNoteStopped(() => {
      setNoteId(null);
      setIsPaused(false);
    });

    return () => {
      unlistenUtterance.then((fn) => fn());
      unlistenError.then((fn) => fn());
      unlistenStarted.then((fn) => fn());
      unlistenStopped.then((fn) => fn());
    };
  }, [onToast]);

  const start = useCallback(
    async (existingNoteId?: number) => {
      if (!groqApiKey) {
        onToast?.({ description: "Notes need a Groq API key (used for transcription)." });
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
            if (err) onToast?.({ description: err });
          })
          .catch(() => {});
      }, 750);

      return id;
    },
    [groqApiKey, micDeviceId, onToast],
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
    if (noteIdRef.current == null) return;
    await stopNoteCapture();
    setNoteId(null);
    setIsPaused(false);
  }, []);

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
