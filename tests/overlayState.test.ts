import { describe, expect, test } from "bun:test";
import {
  getOverlayMotionMode,
  getOverlayVisualPhase,
} from "../src/components/overlayState";

describe("floating overlay visual phases", () => {
  test("maps recording, processing, idle, and recoverable error states", () => {
    expect(getOverlayVisualPhase("idle", false)).toBe("idle");
    expect(getOverlayVisualPhase("recording", false)).toBe("recording");
    expect(getOverlayVisualPhase("processing", false)).toBe("processing");
    expect(getOverlayVisualPhase("polishing", false)).toBe("polishing");
    expect(getOverlayVisualPhase("idle", true)).toBe("error");
    expect(getOverlayVisualPhase("recording", true)).toBe("recording");
  });

  test("animates only active phases and disables motion for reduced-motion users", () => {
    expect(getOverlayMotionMode("idle", false)).toBe("none");
    expect(getOverlayMotionMode("error", false)).toBe("none");
    expect(getOverlayMotionMode("recording", false)).toBe("listening");
    expect(getOverlayMotionMode("processing", false)).toBe("processing");
    expect(getOverlayMotionMode("recording", true)).toBe("none");
    expect(getOverlayMotionMode("processing", true)).toBe("none");
  });
});

test("overlay keeps one clipped 32px visual inside an invisible 44px target", async () => {
  const source = await Bun.file("src/components/DictationOverlay.tsx").text();
  expect(source.match(/data-overlay-visible-circle=/g)).toHaveLength(1);
  expect(source).toContain('data-overlay-visible-circle="32px"');
  expect(source).toContain('data-overlay-hit-target="44px"');
  expect(source).toContain('data-overlay-outer-stroke="36px"');
  expect(source).toContain("w-11 h-11 items-center justify-center border-0 bg-transparent");
  expect(source).toContain("w-8 h-8 shrink-0 items-center justify-center overflow-hidden rounded-full border-0");
  expect(source).toContain('data-overlay-internal-effect="centered-clipped"');
  expect(source).toContain("transform-origin: 50% 50%");
  expect(source).toContain("circle at 36% 30%");
  expect(source).toContain("overlay-processing-pulse");
  expect(source).toContain("overlay-listening-wave");
  expect(source).toContain("#a855f7");
  expect(source).toContain("overlay-processing-dots");
  expect(source).toContain("@media (prefers-reduced-motion: reduce)");
  expect(source).not.toContain("overlay-processing-gradient");
  expect(source).not.toContain("overlay-listening-ripple");
  expect(source).not.toContain("w-11 h-11 rounded-full");
  expect(source).not.toContain("rotate(");
});
