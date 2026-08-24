import { expect, test } from "bun:test";
import {
  AudioPlaybackController,
  type AudioElementLike,
  type AudioPlaybackSource,
} from "../src/services/audioPlayback";
import { resolveHistoryAudioSources } from "../src/services/historyAudioSources";

class FakeAudio implements AudioElementLike {
  preload = "";
  src = "";
  currentTime = 0;
  duration = 12;
  paused = true;
  rejectPlay = false;
  loadCount = 0;
  private readonly handlers = new Map<string, Set<EventListener>>();

  addEventListener(type: string, listener: EventListener): void {
    const handlers = this.handlers.get(type) ?? new Set<EventListener>();
    handlers.add(listener);
    this.handlers.set(type, handlers);
  }

  removeEventListener(type: string, listener: EventListener): void {
    this.handlers.get(type)?.delete(listener);
  }

  play(): Promise<void> {
    if (this.rejectPlay) return Promise.reject(new Error("unsupported_audio"));
    this.paused = false;
    this.emit("play");
    return Promise.resolve();
  }

  pause(): void {
    this.paused = true;
    this.emit("pause");
  }

  load(): void {
    this.loadCount += 1;
  }

  removeAttribute(name: string): void {
    if (name === "src") this.src = "";
  }

  emit(type: string): void {
    for (const listener of this.handlers.get(type) ?? []) listener(new Event(type));
  }

  listenerCount(): number {
    let count = 0;
    for (const handlers of this.handlers.values()) count += handlers.size;
    return count;
  }
}

function source(key: string, url = `recording://${key}`): AudioPlaybackSource {
  return { key, url, durationMs: 12_000 };
}

function factory(created: FakeAudio[]): () => FakeAudio {
  return () => {
    const audio = new FakeAudio();
    created.push(audio);
    return audio;
  };
}

test("lazily plays one URL and supports pause, resume, seek, restart, and stop", async () => {
  const created: FakeAudio[] = [];
  const controller = new AudioPlaybackController(factory(created));

  expect(created).toHaveLength(0);
  await controller.play({ key: "dictation", resolve: async () => [source("dictation")] });

  expect(created).toHaveLength(1);
  expect(created[0].preload).toBe("metadata");
  expect(created[0].src).toBe("recording://dictation");
  expect(controller.getSnapshot().status).toBe("playing");

  controller.pause();
  expect(controller.getSnapshot().status).toBe("paused");
  await controller.resume();
  expect(controller.getSnapshot().status).toBe("playing");
  controller.seek(4.5);
  expect(created[0].currentTime).toBe(4.5);
  await controller.restart();
  expect(created[0].currentTime).toBe(0);

  controller.stop();
  expect(controller.getSnapshot().status).toBe("idle");
  expect(controller.getSnapshot().key).toBeNull();
  expect(created[0].src).toBe("");
  expect(created[0].listenerCount()).toBe(0);
});

test("a single long segment remains one lazy URL-backed media source", async () => {
  const created: FakeAudio[] = [];
  const controller = new AudioPlaybackController(factory(created));
  await controller.play({
    key: "long-note-segment",
    resolve: async () => [{
      key: "long-note-segment",
      url: "recording://asset/9001",
      durationMs: 6 * 60 * 60 * 1000,
    }],
  });

  expect(created).toHaveLength(1);
  expect(created[0].src).toBe("recording://asset/9001");
  expect(created[0].preload).toBe("metadata");
  expect(controller.getSnapshot().queueLength).toBe(1);
});

test("switching requests cancels a pending resolver and leaves one active source", async () => {
  const created: FakeAudio[] = [];
  const controller = new AudioPlaybackController(factory(created));
  let resolveFirst!: (sources: AudioPlaybackSource[]) => void;
  const first = controller.play({
    key: "first",
    resolve: () => new Promise((resolve) => { resolveFirst = resolve; }),
  });

  await Promise.resolve();
  const second = controller.play({ key: "second", resolve: async () => [source("second")] });
  await second;
  resolveFirst([source("first")]);
  await first;

  expect(controller.getSnapshot().key).toBe("second");
  expect(controller.getSnapshot().status).toBe("playing");
  expect(created).toHaveLength(1);
  expect(created[0].src).toBe("recording://second");
});

test("repeated play clicks while a source resolves remain idempotent", async () => {
  const created: FakeAudio[] = [];
  const controller = new AudioPlaybackController(factory(created));
  let resolveSource!: (sources: AudioPlaybackSource[]) => void;
  let resolveCount = 0;
  const request = {
    key: "same-source",
    resolve: () => {
      resolveCount += 1;
      return new Promise<AudioPlaybackSource[]>((resolve) => {
        resolveSource = resolve;
      });
    },
  };

  const first = controller.toggle(request);
  await Promise.resolve();
  await controller.toggle(request);
  expect(resolveCount).toBe(1);
  expect(created).toHaveLength(0);

  resolveSource([source("same-source")]);
  await first;
  expect(created).toHaveLength(1);
  expect(controller.getSnapshot().status).toBe("playing");
});

test("ordered queues advance to the next segment without preloading it", async () => {
  const created: FakeAudio[] = [];
  const controller = new AudioPlaybackController(factory(created));
  await controller.play({
    key: "note-all",
    resolve: async () => [source("segment-1"), source("segment-2"), source("segment-3")],
  });

  expect(created).toHaveLength(1);
  expect(created[0].src).toBe("recording://segment-1");
  expect(controller.getSnapshot().queueIndex).toBe(0);
  created[0].emit("ended");
  await Promise.resolve();
  expect(created[0].src).toBe("recording://segment-2");
  expect(controller.getSnapshot().queueIndex).toBe(1);
  expect(controller.getSnapshot().queueLength).toBe(3);

  created[0].emit("ended");
  await Promise.resolve();
  expect(created[0].src).toBe("recording://segment-3");
  created[0].emit("ended");
  expect(controller.getSnapshot().status).toBe("ended");
  expect(created[0].src).toBe("");
  expect(created[0].listenerCount()).toBe(0);
});

test("Play All skips one failed segment without duplicating or blocking the queue", async () => {
  const created: FakeAudio[] = [];
  const controller = new AudioPlaybackController(() => {
    const audio = new FakeAudio();
    audio.rejectPlay = created.length === 0;
    created.push(audio);
    return audio;
  });

  await controller.play({
    key: "note-all-with-failure",
    resolve: async () => [source("segment-1"), source("segment-2"), source("segment-3")],
  });
  await Promise.resolve();

  expect(created).toHaveLength(2);
  expect(created[0].src).toBe("");
  expect(created[0].listenerCount()).toBe(0);
  expect(created[1].src).toBe("recording://segment-2");
  expect(controller.getSnapshot()).toMatchObject({
    status: "playing",
    queueIndex: 1,
    queueLength: 3,
  });

  created[1].emit("ended");
  await Promise.resolve();
  expect(created[1].src).toBe("recording://segment-3");
  expect(controller.getSnapshot().queueIndex).toBe(2);
});

test("Play All URL resolution skips one unavailable asset and preserves ordering", async () => {
  const calls: number[] = [];
  const sources = await resolveHistoryAudioSources(
    [{ id: 1 }, { id: 2 }, { id: 3 }],
    async (id) => {
      calls.push(id);
      if (id === 2) throw new Error("audio_missing");
      return { url: `recording://asset/${id}`, duration_ms: id * 1000 };
    },
    true,
  );

  expect(calls).toEqual([1, 2, 3]);
  expect(sources.map((item) => item.key)).toEqual(["asset-1", "asset-3"]);
  expect(sources.map((item) => item.url)).toEqual([
    "recording://asset/1",
    "recording://asset/3",
  ]);
});

test("failed and stopped requests clear the source and all handlers", async () => {
  const created: FakeAudio[] = [];
  const controller = new AudioPlaybackController(() => {
    const audio = new FakeAudio();
    audio.rejectPlay = true;
    created.push(audio);
    return audio;
  });
  await controller.play({ key: "failed", resolve: async () => [source("failed")] });
  expect(controller.getSnapshot().status).toBe("error");
  expect(controller.getSnapshot().error).toBe("unsupported_audio");
  expect(created[0].src).toBe("");
  expect(created[0].listenerCount()).toBe(0);

  let resolvePending!: (sources: AudioPlaybackSource[]) => void;
  const pending = controller.play({
    key: "pending",
    resolve: () => new Promise((resolve) => { resolvePending = resolve; }),
  });
  controller.stop();
  resolvePending([source("never-played")]);
  await pending;
  expect(controller.getSnapshot().status).toBe("idle");
  expect(controller.getSnapshot().key).toBeNull();
});

test("the frontend playback layer contains URL references only, never complete audio buffers", async () => {
  const sourceText = await Bun.file("src/services/audioPlayback.ts").text();
  expect(sourceText).not.toContain("Uint8Array");
  expect(sourceText).not.toContain("arrayBuffer");
  expect(sourceText).not.toContain("Blob");
  expect(sourceText).not.toContain("fetch(");
});
