import { describe, expect, test } from "bun:test";
import {
  dispatchCompletedVoiceCommand,
  type VoiceCommandDispatchPort,
} from "../src/services/voiceCommandDispatch";

const PERSONAS = [
  { id: "support", name: "Support" },
  { id: "sales", name: "Sales" },
];

function recordingPort(
  calls: Array<[string, string | null | undefined]>,
): VoiceCommandDispatchPort {
  let nextId = 1;
  return {
    dispatchCaptureIntent: async (kind, personaId) => {
      calls.push([kind, personaId]);
      return nextId++;
    },
  };
}

describe("completed transcription voice-command dispatch", () => {
  test("routes exact and fuzzy completed transcripts through the action boundary", async () => {
    const cases: Array<{
      text: string;
      expected: [string, string | null | undefined];
    }> = [
      { text: "Clara, start notes", expected: ["note", undefined] },
      { text: "Clara, note", expected: ["note", undefined] },
      { text: "Clara, stark nodes", expected: ["note", undefined] },
      { text: "Clara, support", expected: ["conversation", "support"] },
      { text: "Clara, conversation", expected: ["conversation", null] },
      { text: "Clara, call", expected: ["conversation", null] },
      { text: "Clara, start call", expected: ["conversation", null] },
      { text: "Clara, settings", expected: ["settings", undefined] },
    ];

    for (const { text, expected } of cases) {
      const calls: Array<[string, string | null | undefined]> = [];
      const result = await dispatchCompletedVoiceCommand(
        text,
        "Clara",
        [],
        PERSONAS,
        recordingPort(calls),
      );
      expect(result.handled).toBe(true);
      expect(result.intentId).toBe(1);
      expect(calls).toEqual([expected]);
    }
  });

  test("uses the persisted name supplied with each completed transcription", async () => {
    const calls: Array<[string, string | null | undefined]> = [];
    const port = recordingPort(calls);
    const beforeChange = await dispatchCompletedVoiceCommand(
      "Clara, note",
      "Aral",
      [],
      PERSONAS,
      port,
    );
    const afterChange = await dispatchCompletedVoiceCommand(
      "Clara, note",
      "Clara",
      [],
      PERSONAS,
      port,
    );

    expect(beforeChange.handled).toBe(false);
    expect(afterChange.handled).toBe(true);
    expect(calls).toEqual([["note", undefined]]);
  });

  test("repeated and close commands each create one distinct intent", async () => {
    const calls: Array<[string, string | null | undefined]> = [];
    const port = recordingPort(calls);
    const [first, second] = await Promise.all([
      dispatchCompletedVoiceCommand("Clara, note", "Clara", [], PERSONAS, port),
      dispatchCompletedVoiceCommand("Clara, support", "Clara", [], PERSONAS, port),
    ]);
    const repeated = await dispatchCompletedVoiceCommand(
      "Clara, note",
      "Clara",
      [],
      PERSONAS,
      port,
    );

    expect([first.intentId, second.intentId, repeated.intentId]).toEqual([1, 2, 3]);
    expect(calls).toEqual([
      ["note", undefined],
      ["conversation", "support"],
      ["note", undefined],
    ]);
  });

  test("a consumed command is not pasted or saved as ordinary dictation", async () => {
    let pasted = 0;
    let saved = 0;
    const complete = async (text: string) => {
      const result = await dispatchCompletedVoiceCommand(
        text,
        "Clara",
        [],
        PERSONAS,
        recordingPort([]),
      );
      if (!result.handled) {
        pasted += 1;
        saved += 1;
      }
    };

    await complete("Clara, start notes");
    await complete("I use nodes in this project and will call support later");
    expect({ pasted, saved }).toEqual({ pasted: 1, saved: 1 });
  });

  test("ordinary addressed sentences remain ordinary speech", async () => {
    for (const text of [
      "Clara, I use nodes in this project",
      "Clara, add notes about the support call",
      "Clara, our conversation mentioned calls",
      "Clara, call the support team tomorrow",
      "Clara, all tasks are complete",
    ]) {
      const calls: Array<[string, string | null | undefined]> = [];
      const result = await dispatchCompletedVoiceCommand(
        text,
        "Clara",
        [],
        PERSONAS,
        recordingPort(calls),
      );
      expect(result.handled).toBe(false);
      expect(calls).toHaveLength(0);
    }
  });

  test("command authorization runs before arbitrary custom-dictionary replacement", async () => {
    const source = await Bun.file("src/hooks/useAudioRecording.ts").text();
    const commandIndex = source.indexOf("onVoiceCommandRef.current(providerText");
    const dictionaryIndex = source.indexOf("applyAlwaysDictionaryCorrections(");
    expect(commandIndex).toBeGreaterThan(-1);
    expect(dictionaryIndex).toBeGreaterThan(commandIndex);

    const calls: Array<[string, string | null | undefined]> = [];
    const result = await dispatchCompletedVoiceCommand(
      "Clara, start widgets",
      "Clara",
      [],
      PERSONAS,
      recordingPort(calls),
    );
    expect(result.handled).toBe(false);
    expect(calls).toHaveLength(0);
  });

  test("failed window dispatch is surfaced to the completed-transcription caller", async () => {
    const port: VoiceCommandDispatchPort = {
      dispatchCaptureIntent: async () => {
        throw new Error("conversation_window_creation_failed");
      },
    };
    await expect(
      dispatchCompletedVoiceCommand("Clara, note", "Clara", [], PERSONAS, port),
    ).rejects.toThrow("conversation_window_creation_failed");
  });

  test("one focused retry can recover before durable dispatch", async () => {
    const calls: Array<[string, string | null | undefined]> = [];
    let retries = 0;
    const result = await dispatchCompletedVoiceCommand(
      "Clara settinx",
      "Clara",
      [],
      PERSONAS,
      recordingPort(calls),
      undefined,
      async () => {
        retries++;
        return "Clara settings";
      },
      "en",
    );
    expect(retries).toBe(1);
    expect(result.handled).toBe(true);
    expect(calls).toEqual([["settings", undefined]]);
  });

  test("short wake-elided ASR can recover the configured name before dispatch", async () => {
    const calls: Array<[string, string | null | undefined]> = [];
    const result = await dispatchCompletedVoiceCommand(
      "Support.",
      "Agenda",
      [],
      PERSONAS,
      recordingPort(calls),
      undefined,
      async () => "Agenda, support.",
      "en",
      true,
    );
    expect(result.handled).toBe(true);
    expect(calls).toEqual([["conversation", "support"]]);
  });
});
