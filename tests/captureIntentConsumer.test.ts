import { describe, expect, test } from "bun:test";
import {
  CaptureIntentConsumer,
  type CaptureIntentConsumerPort,
} from "../src/services/captureIntentConsumer";
import type { CaptureIntent } from "../src/services/tauriApi";

function queuedPort(initial: CaptureIntent[]) {
  const queue = [...initial];
  const acknowledgements: number[] = [];
  let reads = 0;
  const port: CaptureIntentConsumerPort = {
    getPendingCaptureIntent: async () => {
      reads += 1;
      return queue[0] ?? null;
    },
    acknowledgeCaptureIntent: async (intentId) => {
      if (queue[0]?.id !== intentId) return false;
      acknowledgements.push(intentId);
      queue.shift();
      return true;
    },
  };
  return { port, queue, acknowledgements, reads: () => reads };
}

describe("durable capture-intent consumption", () => {
  test("a slow React mount leaves the intent pending until the target is ready", async () => {
    const state = queuedPort([{ id: 1, kind: "note", persona_id: null }]);
    const handled: number[] = [];
    const consumer = new CaptureIntentConsumer(state.port);

    await consumer.consume(false, async (intent) => { handled.push(intent.id); });
    expect(state.reads()).toBe(0);
    expect(state.queue).toHaveLength(1);

    await consumer.consume(true, async (intent) => { handled.push(intent.id); });
    await consumer.consume(true, async (intent) => { handled.push(intent.id); });
    expect(handled).toEqual([1]);
    expect(state.acknowledgements).toEqual([1]);
  });

  test("an already-open target drains two close commands in order", async () => {
    const state = queuedPort([
      { id: 10, kind: "note", persona_id: null },
      { id: 11, kind: "conversation", persona_id: "support" },
    ]);
    const handled: Array<[number, string, string | null]> = [];
    const consumer = new CaptureIntentConsumer(state.port);
    await consumer.consume(true, async (intent) => {
      handled.push([intent.id, intent.kind, intent.persona_id]);
    });

    expect(handled).toEqual([
      [10, "note", null],
      [11, "conversation", "support"],
    ]);
    expect(state.acknowledgements).toEqual([10, 11]);
  });

  test("duplicate readiness notifications cannot consume the same intent twice", async () => {
    const state = queuedPort([{ id: 20, kind: "conversation", persona_id: null }]);
    let release!: () => void;
    const gate = new Promise<void>((resolve) => { release = resolve; });
    let handled = 0;
    const consumer = new CaptureIntentConsumer(state.port);
    const handle = async () => {
      handled += 1;
      await gate;
    };

    const first = consumer.consume(true, handle);
    const duplicate = consumer.consume(true, handle);
    await Promise.resolve();
    expect(handled).toBe(1);
    release();
    await Promise.all([first, duplicate]);
    expect(state.acknowledgements).toEqual([20]);
  });

  test("a failed action is not acknowledged or silently cleared", async () => {
    const state = queuedPort([{ id: 30, kind: "note", persona_id: null }]);
    const consumer = new CaptureIntentConsumer(state.port);
    await expect(
      consumer.consume(true, async () => { throw new Error("note_capture_failed"); }),
    ).rejects.toThrow("note_capture_failed");
    expect(state.queue).toHaveLength(1);
    expect(state.acknowledgements).toHaveLength(0);
  });
});
