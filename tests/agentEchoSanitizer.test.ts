import { describe, expect, test } from "bun:test";
import { sanitizeAgentWakeEcho } from "../src/services/agentEchoSanitizer";
import { detectVoiceCommand } from "../src/config/voiceCommands";

describe("configured agent wake-word echo sanitizer", () => {
  test("collapses repeated leading names while preserving a command invocation", () => {
    const cleaned = sanitizeAgentWakeEcho("Agenda, Agenda, start notes.", "Agenda");
    expect(cleaned).toBe("Agenda, start notes.");
    expect(detectVoiceCommand(cleaned, "Agenda", [], [])).toEqual({ kind: "start-note" });
  });

  test("handles fuzzy repeated leading variants and the optional Agent prefix", () => {
    expect(sanitizeAgentWakeEcho("Agent Tom, Thumb, start note.", "Tom")).toBe(
      "Agent Tom, start note.",
    );
    expect(sanitizeAgentWakeEcho("Aro Aral start notes", "Aral")).toBe("Aro start notes");
  });

  test("keeps one trailing mention when the name is repeated", () => {
    expect(sanitizeAgentWakeEcho("Please ask Agenda, Agenda.", "Agenda")).toBe(
      "Please ask Agenda.",
    );
    expect(sanitizeAgentWakeEcho("Please ask Agenda.", "Agenda")).toBe(
      "Please ask Agenda.",
    );
  });

  test("removes a lone trailing echo after a complete sentence", () => {
    expect(sanitizeAgentWakeEcho("The meeting starts at three. Agenda", "Agenda")).toBe(
      "The meeting starts at three.",
    );
    expect(sanitizeAgentWakeEcho("会议三点开始。Agenda", "Agenda")).toBe("会议三点开始。");
  });

  test("preserves one meaningful leading address and interior mentions", () => {
    expect(sanitizeAgentWakeEcho("Agenda, what is your name?", "Agenda")).toBe(
      "Agenda, what is your name?",
    );
    expect(sanitizeAgentWakeEcho("I asked Agenda about the roadmap.", "Agenda")).toBe(
      "I asked Agenda about the roadmap.",
    );
  });

  test("uses runtime aliases, punctuation, and casing", () => {
    expect(sanitizeAgentWakeEcho("CLARA — clara, support", "Agenda", ["Clara"])).toBe(
      "CLARA, support",
    );
  });

  test("does not globally rewrite sound-alikes or empty speech", () => {
    expect(sanitizeAgentWakeEcho("I use nodes in this project.", "Notes")).toBe(
      "I use nodes in this project.",
    );
    expect(sanitizeAgentWakeEcho("   ", "Agenda")).toBe("");
    expect(sanitizeAgentWakeEcho("Agenda Agenda", null)).toBe("Agenda Agenda");
  });
});
