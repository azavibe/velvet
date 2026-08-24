import { describe, expect, test } from "bun:test";
import {
  detectVoiceCommand,
  detectVoiceCommandWithDiagnostics,
  matchVoiceCommand,
} from "../src/config/voiceCommands";

const PERSONAS = [
  { id: "meeting", name: "Meeting" },
  { id: "sales", name: "Sales" },
  { id: "support", name: "Support" },
  { id: "language", name: "Language practice" },
];

function detect(text: string, aliases: string[] = ["Arrow"]) {
  return detectVoiceCommand(text, "Aral", aliases, PERSONAS);
}

describe("voice command detection", () => {
  test("starts notes from the phrasings people actually say", () => {
    for (const text of [
      "Aral, start notes",
      "Aral start notes.",
      "Aral, take a note",
      "Hey Aral, take notes",
      "Aral, new note",
      "Aral. Notes.",
      "aral, note taking",
    ]) {
      expect(detect(text)).toEqual({ kind: "start-note" });
    }
  });

  test("accepts only the short, wake-word-prefixed note ASR variants fuzzily", () => {
    for (const text of ["Aral, stark nodes", "Aral, star nodes", "Aral, start node"]) {
      expect(detect(text)).toEqual({ kind: "start-note" });
      expect(matchVoiceCommand(text, "Aral", ["Arrow"], PERSONAS)?.matchType).toBe("fuzzy");
    }
    expect(detect("Aral, please start nodes")).toEqual({ kind: "start-note" });
    expect(detect("Aral, could you start notes")).toEqual({ kind: "start-note" });
  });

  test("starts a conversation without naming a persona", () => {
    for (const text of [
      "Aral, start conversation",
      "Aral, start a conversation.",
      "Okay Aral, begin call",
      "Aral, call",
      "Aral, start call",
      "Aral, new chat",
    ]) {
      expect(detect(text)).toEqual({ kind: "start-conversation", personaId: null });
    }
  });

  test("naming a persona starts a conversation with it", () => {
    expect(detect("Aral, support")).toEqual({ kind: "start-conversation", personaId: "support" });
    expect(detect("Aral, start sales")).toEqual({ kind: "start-conversation", personaId: "sales" });
    expect(detect("Aral, switch to meeting")).toEqual({ kind: "start-conversation", personaId: "meeting" });
    expect(detect("Aral, language practice")).toEqual({ kind: "start-conversation", personaId: "language" });
  });

  test("stop wins over a persona whose name it contains", () => {
    // "meeting" is a persona, but "stop meeting" is unambiguously a stop.
    expect(detect("Aral, stop meeting")).toEqual({ kind: "stop" });
    expect(detect("Aral, stop")).toEqual({ kind: "stop" });
    expect(detect("Aral, end the conversation")).toEqual({ kind: "stop" });
    expect(detect("Aral, stop recording")).toEqual({ kind: "stop" });
  });

  test("aliases work the same as the configured name", () => {
    expect(detect("Arrow, start notes")).toEqual({ kind: "start-note" });
    expect(detect("Hey Arrow, support")).toEqual({ kind: "start-conversation", personaId: "support" });
  });

  test("real captures: no punctuation, and the name comes back mangled", () => {
    // Verbatim transcripts from a device, where speech-to-text neither
    // added a comma nor heard the name cleanly.
    expect(detect("Aral Support", [])).toEqual({ kind: "start-conversation", personaId: "support" });
    expect(detect("Aral Start Notes", [])).toEqual({ kind: "start-note" });
    expect(detect("Aro Start notes", [])).toEqual({ kind: "start-note" });
    // The recognizer doubles the wake word up on its own.
    expect(detect("Aro Aro Start notes", [])).toEqual({ kind: "start-note" });
    expect(detect("Aral Ahral Start Notes", [])).toEqual({ kind: "start-note" });
  });

  test("a mangled name still can't fire on non-command speech", () => {
    // The looseness on the name is only safe because the rest of the
    // utterance still has to be exactly a command.
    expect(detect("Aro is the name of our new product", [])).toBeNull();
    expect(detect("Ahral said he would start notes for the meeting", [])).toBeNull();
  });

  test("does not globally rewrite nodes or trigger on an ordinary interior mention", () => {
    expect(detect("I use nodes in this project")).toBeNull();
    expect(detect("Aral, I use nodes in this project")).toBeNull();
    expect(detect("Aral, start nodes for the meeting notes")).toBeNull();
    expect(detect("Aral, star the nodes in this graph")).toBeNull();
  });

  test("keeps exact support and conversation commands preferred", () => {
    expect(matchVoiceCommand("Aral, support", "Aral", [], PERSONAS)).toEqual({
      command: { kind: "start-conversation", personaId: "support" },
      matchType: "exact",
    });
    expect(matchVoiceCommand("Aral, conversation", "Aral", [], PERSONAS)).toEqual({
      command: { kind: "start-conversation", personaId: null },
      matchType: "exact",
    });
  });

  test("accepts greetings and multilingual surrounding text without loosening the command body", () => {
    expect(detect("Bonjour Aral, start notes")).toEqual({ kind: "start-note" });
    expect(detect("Привет Aral, conversation")).toEqual({
      kind: "start-conversation",
      personaId: null,
    });
    expect(detect("Hola Aral, nodes in this project")).toBeNull();
  });

  test("ordinary dictation is never a command", () => {
    for (const text of [
      "let's start a conversation about the roadmap",
      "I need to take notes on this later",
      "please stop the deployment",
      "we should support the new format",
      "I will call support after we discuss notes and conversation",
    ]) {
      expect(detect(text)).toBeNull();
    }
  });

  test("an agent question is not a command", () => {
    // Addressed to the agent, but the remainder isn't a command phrase —
    // this must fall through to chat mode, not silently open a window.
    expect(detect("Aral, what's the capital of France?")).toBeNull();
    expect(detect("Aral, summarize this for me")).toBeNull();
    expect(detect("Aral, start notes with the following headings")).toBeNull();
  });

  test("a bare address is not a command", () => {
    expect(detect("Aral")).toBeNull();
    expect(detect("Aral,")).toBeNull();
  });

  test("no agent name configured means no commands", () => {
    expect(detectVoiceCommand("start notes", "", [], PERSONAS)).toBeNull();
    expect(detectVoiceCommand("start notes", null, undefined, PERSONAS)).toBeNull();
  });

  test("uses the configured name instead of a built-in wake word", () => {
    expect(detectVoiceCommand("Clara, start notes", "Clara", [], PERSONAS)).toEqual({
      kind: "start-note",
    });
    expect(detectVoiceCommand("Aral, start notes", "Clara", [], PERSONAS)).toBeNull();
  });

  test("reports privacy-safe candidate, wake, action, and rejection diagnostics", () => {
    expect(detectVoiceCommandWithDiagnostics("Aro, stark nodes", "Aral", [], PERSONAS).diagnostics).toEqual({
      candidate: true,
      wakeMatch: "fuzzy",
      matchType: "fuzzy",
      action: "start-note",
      rejectionReason: null,
    });
    expect(detectVoiceCommandWithDiagnostics("Aral, call the support team", "Aral", [], PERSONAS).diagnostics).toEqual({
      candidate: true,
      wakeMatch: "exact",
      matchType: null,
      action: null,
      rejectionReason: "unsupported_command",
    });
  });

  test("keeps settings declarative and addressed questions non-command", () => {
    expect(detect("Aral, settings")).toEqual({ kind: "open-settings" });
    expect(detect("Aral, open preferences")).toEqual({ kind: "open-settings" });
    expect(detect("Aral, what settings do you have?")).toBeNull();
    expect(detect("Aral, what is your name?")).toBeNull();
  });
});
