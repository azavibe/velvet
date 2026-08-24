import { useSyncExternalStore } from "react";
import {
  audioPlaybackController,
  type AudioPlaybackController,
  type AudioPlaybackSnapshot,
} from "@/services/audioPlayback";

export function useAudioPlayback(): AudioPlaybackSnapshot &
  Pick<AudioPlaybackController, "play" | "toggle" | "pause" | "resume" | "seek" | "restart" | "stop"> {
  const snapshot = useSyncExternalStore(
    audioPlaybackController.subscribe,
    audioPlaybackController.getSnapshot,
    audioPlaybackController.getSnapshot,
  );
  return {
    ...snapshot,
    play: audioPlaybackController.play.bind(audioPlaybackController),
    toggle: audioPlaybackController.toggle.bind(audioPlaybackController),
    pause: audioPlaybackController.pause.bind(audioPlaybackController),
    resume: audioPlaybackController.resume.bind(audioPlaybackController),
    seek: audioPlaybackController.seek.bind(audioPlaybackController),
    restart: audioPlaybackController.restart.bind(audioPlaybackController),
    stop: audioPlaybackController.stop.bind(audioPlaybackController),
  };
}
