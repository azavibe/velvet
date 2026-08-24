export type DictationOverlayPhase = "idle" | "recording" | "processing" | "polishing";
export type OverlayVisualPhase = DictationOverlayPhase | "error";
export type OverlayMotionMode = "none" | "listening" | "processing";

export function getOverlayVisualPhase(
  phase: DictationOverlayPhase,
  hasError: boolean,
): OverlayVisualPhase {
  return hasError && phase === "idle" ? "error" : phase;
}

export function getOverlayMotionMode(
  phase: OverlayVisualPhase,
  prefersReducedMotion: boolean,
): OverlayMotionMode {
  if (prefersReducedMotion || phase === "idle" || phase === "error") return "none";
  return phase === "recording" ? "listening" : "processing";
}
