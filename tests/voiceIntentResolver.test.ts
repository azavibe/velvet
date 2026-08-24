import { describe, expect, test } from "bun:test";
import { resolveVoiceIntent } from "../src/services/voiceIntentResolver";
import { dispatchCompletedVoiceCommand } from "../src/services/voiceCommandDispatch";
import { CaptureIntentConsumer } from "../src/services/captureIntentConsumer";
import type { CaptureIntent } from "../src/services/tauriApi";

const PERSONAS = [
  { id: "support", name: "Support" },
  { id: "sales", name: "Sales" },
];

describe("bounded voice intent resolver", () => {
  test("recovers configured names, Agent prefix, and observed ASR forms locally", async () => {
    const cases = [
      ["Agent Tom, start note.", "Tom", "start-note"],
      ["Thumb, Stark Nose. Thank you.", "Tom", "start-note"],
      ["Agenda Noty", "Agenda", "start-note"],
      ["Agenda Kolejny", "Agenda", "start-conversation"],
      ["Agenda call", "Agenda", "start-conversation"],
      ["Agenda start call", "Agenda", "start-conversation"],
      ["Agenda start conversation", "Agenda", "start-conversation"],
      ["Agenda settings", "Agenda", "open-settings"],
      ["Agenda open settings", "Agenda", "open-settings"],
      ["Agenda support", "Agenda", "start-conversation"],
      ["Agenda stop", "Agenda", "stop"],
    ] as const;
    for (const [text, name, action] of cases) {
      const result = await resolveVoiceIntent(text, name, [], PERSONAS);
      expect(result.detection.match?.command.kind).toBe(action);
      expect(result.retryAttempted).toBe(false);
    }
  });

  test("uses the current runtime name and explicit aliases only", async () => {
    expect((await resolveVoiceIntent("Tom, notes", "Tom", [], PERSONAS)).detection.match).not.toBeNull();
    expect((await resolveVoiceIntent("Tom, notes", "Maya", [], PERSONAS)).detection.match).toBeNull();
    expect((await resolveVoiceIntent("Assistant, notes", "Maya", ["Assistant"], PERSONAS)).detection.match).not.toBeNull();
  });

  test("performs at most one command-focused retry and can recover", async () => {
    let retries = 0;
    const result = await resolveVoiceIntent(
      "Agenda settinx",
      "Agenda",
      [],
      PERSONAS,
      async (prompt) => {
        retries++;
        expect(prompt).toContain("Agenda");
        expect(prompt).toContain("settings");
        expect(prompt).toContain("Application language: en");
        return "Agenda, settings";
      },
      undefined,
      "en",
    );
    expect(retries).toBe(1);
    expect(result.stage).toBe("retry");
    expect(result.detection.match?.command).toEqual({ kind: "open-settings" });
  });

  test("unsupported, failed, and timed-out retries fall back without loops", async () => {
    for (const retry of [
      async () => null,
      async () => { throw new Error("provider_failed"); },
      async () => { throw new Error("command_retry_timeout"); },
    ]) {
      let calls = 0;
      const result = await resolveVoiceIntent(
        "Agenda settinx",
        "Agenda",
        [],
        PERSONAS,
        async (prompt) => { calls++; return retry(prompt); },
      );
      expect(calls).toBe(1);
      expect(result.detection.match).toBeNull();
      expect(result.retryAttempted).toBe(true);
    }
    const unsupported = await resolveVoiceIntent("Agenda settinx", "Agenda", [], PERSONAS);
    expect(unsupported.retryAttempted).toBe(false);
  });

  test("retry prompt includes registered personas and cannot loosen Stop", async () => {
    let promptSeen = "";
    const support = await resolveVoiceIntent(
      "Agenda suppxrtx",
      "Agenda",
      [],
      PERSONAS,
      async (prompt) => {
        promptSeen = prompt;
        return "Agenda support";
      },
    );
    expect(promptSeen).toContain("Support");
    expect(support.detection.match?.command).toEqual({ kind: "start-conversation", personaId: "support" });

    const stop = await resolveVoiceIntent(
      "Agenda stappx",
      "Agenda",
      [],
      PERSONAS,
      async () => "Agenda stop",
    );
    expect(stop.detection.match).toBeNull();
  });

  test("short bare actions may retry for an elided wake but never execute directly", async () => {
    let retries = 0;
    const recovered = await resolveVoiceIntent(
      "Start notes.",
      "Agenda",
      [],
      PERSONAS,
      async () => {
        retries++;
        return "Agenda, start notes.";
      },
      undefined,
      "en",
      true,
    );
    expect(retries).toBe(1);
    expect(recovered.detection.match?.command).toEqual({ kind: "start-note" });

    const noRetry = await resolveVoiceIntent("Start notes.", "Agenda", [], PERSONAS);
    expect(noRetry.detection.match).toBeNull();
    expect(noRetry.retryAttempted).toBe(false);
  });

  test("wake recovery must return the same low-risk action and never recovers Stop", async () => {
    const changed = await resolveVoiceIntent(
      "Start notes.",
      "Agenda",
      [],
      PERSONAS,
      async () => "Agenda, conversation.",
      undefined,
      "en",
      true,
    );
    expect(changed.detection.match).toBeNull();

    let stopRetries = 0;
    const stop = await resolveVoiceIntent(
      "Stop.",
      "Agenda",
      [],
      PERSONAS,
      async () => { stopRetries++; return "Agenda, stop."; },
      undefined,
      "en",
      true,
    );
    expect(stopRetries).toBe(0);
    expect(stop.detection.match).toBeNull();
  });

  test("ordinary speech and direct questions neither execute nor retry", async () => {
    const texts = [
      "Tom, I took notes during the call.",
      "Agenda calls are scheduled for tomorrow.",
      "Agenda for today includes several items.",
      "I use nodes in this project.",
      "Agenda, what is your name?",
      "Agenda, what settings do you have?",
      "Agenda, what agents are available?",
      "This long dictated passage contains notes support calls and conversations but is ordinary speech.",
    ];
    for (const text of texts) {
      let retries = 0;
      const result = await resolveVoiceIntent(text, "Agenda", ["Tom"], PERSONAS, async () => {
        retries++;
        return "Agenda notes";
      });
      expect(result.detection.match).toBeNull();
      expect(retries).toBe(0);
    }
  });

  test("completed ASR dispatches a durable intent and target acknowledges it once", async () => {
    const queue: CaptureIntent[] = [];
    let nextId = 1;
    const result = await dispatchCompletedVoiceCommand(
      "Thumb, Stark Nose. Thank you.",
      "Tom",
      [],
      PERSONAS,
      {
        dispatchCaptureIntent: async (kind, personaId) => {
          const intent = { id: nextId++, kind, persona_id: personaId ?? null };
          queue.push(intent);
          return intent.id;
        },
      },
    );
    const handled: CaptureIntent[] = [];
    const consumer = new CaptureIntentConsumer({
      getPendingCaptureIntent: async () => queue[0] ?? null,
      acknowledgeCaptureIntent: async (id) => {
        if (queue[0]?.id !== id) return false;
        queue.shift();
        return true;
      },
    });
    await consumer.consume(true, async (intent) => { handled.push(intent); });
    expect(result.handled).toBe(true);
    expect(handled).toEqual([{ id: 1, kind: "note", persona_id: null }]);
    expect(queue).toHaveLength(0);
  });
});
