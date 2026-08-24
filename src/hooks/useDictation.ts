import { useEffect, useState } from "react";
import { useAudioRecording } from "./useAudioRecording";
import { useLiveDictation } from "./useLiveDictation";
import { getSetting, onSettingsChanged } from "@/services/tauriApi";

interface Options {
  onToast?: (props: {
    title: string;
    description: string;
    variant: "default" | "destructive" | "success";
  }) => void;
  /** See useAudioRecording — standard mode only. Live mode types as you
   *  speak, so there's no post-transcription point at which a command could
   *  be intercepted before its text has already been typed out. */
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

export function useDictation(opts: Options = {}) {
  const [mode, setMode] = useState<"standard" | "live">("standard");

  useEffect(() => {
    let cancelled = false;
    getSetting<"standard" | "live">("dictationMode").then((v) => {
      if (!cancelled) setMode(v ?? "standard");
    });
    const unlistenP = onSettingsChanged(() => {
      getSetting<"standard" | "live">("dictationMode").then((v) => {
        if (!cancelled) setMode(v ?? "standard");
      });
    });
    return () => {
      cancelled = true;
      unlistenP.then((u) => u());
    };
  }, []);

  const standard = useAudioRecording(opts);
  const { onVoiceCommand: _unusedInLive, ...liveOpts } = opts;
  const live = useLiveDictation(liveOpts);
  // If a session is in flight on either hook, keep returning that hook so the
  // overlay's stop/cancel buttons stay wired to it. Otherwise a mid-session
  // mode toggle in Settings would orphan the active session — the live hook
  // would keep typing into the focused window with no UI to stop it.
  if (live.phase !== "idle") return live;
  if (standard.phase !== "idle") return standard;
  return mode === "live" ? live : standard;
}
