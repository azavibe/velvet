export type AudioPlaybackStatus = "idle" | "loading" | "playing" | "paused" | "ended" | "error";

export interface AudioPlaybackSnapshot {
  status: AudioPlaybackStatus;
  key: string | null;
  queueIndex: number;
  queueLength: number;
  currentTime: number;
  duration: number | null;
  error: string | null;
}

/** A source is deliberately a URL, not an audio byte buffer. */
export interface AudioPlaybackSource {
  key: string;
  url: string;
  durationMs?: number | null;
}

/** The controller knows nothing about History or Tauri. TTS can use this same request shape later. */
export interface AudioPlaybackRequest {
  key: string;
  resolve: () => Promise<AudioPlaybackSource[]>;
}

export interface AudioElementLike {
  preload: string;
  src: string;
  currentTime: number;
  duration: number;
  paused: boolean;
  addEventListener(type: string, listener: EventListener): void;
  removeEventListener(type: string, listener: EventListener): void;
  play(): Promise<void>;
  pause(): void;
  load(): void;
  removeAttribute(name: string): void;
}

export type AudioElementFactory = () => AudioElementLike;
export type AudioPlaybackListener = () => void;

const INITIAL_SNAPSHOT: AudioPlaybackSnapshot = {
  status: "idle",
  key: null,
  queueIndex: 0,
  queueLength: 0,
  currentTime: 0,
  duration: null,
  error: null,
};

const browserAudioFactory: AudioElementFactory = () => new Audio();

function safePlaybackError(error: unknown): string {
  const message = error instanceof Error ? error.message : typeof error === "string" ? error : "";
  return /^[a-z0-9_]{1,80}$/i.test(message) ? message : "audio_playback_failed";
}

/**
 * One lazy HTMLAudioElement with explicit teardown. The application uses the
 * exported singleton below, so History and future TTS share one media engine.
 */
export class AudioPlaybackController {
  private readonly createAudio: AudioElementFactory;
  private audio: AudioElementLike | null = null;
  private readonly listeners = new Set<AudioPlaybackListener>();
  private readonly eventHandlers = new Map<string, EventListener>();
  private sources: AudioPlaybackSource[] = [];
  private generation = 0;
  private snapshot: AudioPlaybackSnapshot = INITIAL_SNAPSHOT;

  constructor(createAudio: AudioElementFactory = browserAudioFactory) {
    this.createAudio = createAudio;
  }

  getSnapshot = (): AudioPlaybackSnapshot => this.snapshot;

  subscribe = (listener: AudioPlaybackListener): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  async play(request: AudioPlaybackRequest): Promise<void> {
    const generation = ++this.generation;
    this.releaseAudio();
    this.sources = [];
    this.update({
      status: "loading",
      key: request.key,
      queueIndex: 0,
      queueLength: 0,
      currentTime: 0,
      duration: null,
      error: null,
    });

    try {
      const sources = await request.resolve();
      if (generation !== this.generation) return;
      if (sources.length === 0) throw new Error("no_audio_sources");

      this.sources = sources.slice();
      this.update({ queueLength: this.sources.length });
      await this.loadCurrent(generation);
    } catch (error) {
      if (generation === this.generation) {
        this.fail(generation, safePlaybackError(error));
      }
    }
  }

  async toggle(request: AudioPlaybackRequest): Promise<void> {
    if (this.snapshot.key !== request.key) {
      await this.play(request);
      return;
    }
    if (this.snapshot.status === "loading") return;
    if (this.snapshot.status === "playing") {
      this.pause();
    } else if (this.snapshot.status === "paused") {
      await this.resume();
    } else {
      await this.play(request);
    }
  }

  pause(): void {
    if (!this.audio || this.snapshot.status !== "playing") return;
    this.audio.pause();
    this.update({ status: "paused" });
  }

  async resume(): Promise<void> {
    if (!this.audio || this.snapshot.status !== "paused") return;
    const generation = this.generation;
    try {
      await this.audio.play();
      if (generation === this.generation) this.update({ status: "playing", error: null });
    } catch (error) {
      if (generation === this.generation) this.fail(generation, safePlaybackError(error));
    }
  }

  seek(seconds: number): void {
    if (!this.audio || !Number.isFinite(seconds)) return;
    const duration = this.snapshot.duration;
    const upperBound = duration != null && Number.isFinite(duration) ? duration : Math.max(0, this.audio.duration);
    const next = Math.min(Math.max(0, seconds), Number.isFinite(upperBound) ? upperBound : Math.max(0, seconds));
    try {
      this.audio.currentTime = next;
      this.update({ currentTime: next });
    } catch {
      this.fail(this.generation, "audio_seek_failed");
    }
  }

  async restart(): Promise<void> {
    if (!this.audio) return;
    this.seek(0);
    if (this.snapshot.status === "ended" || this.snapshot.status === "paused" || this.snapshot.status === "error") {
      await this.resume();
    }
  }

  stop(): void {
    this.generation += 1;
    this.releaseAudio();
    this.sources = [];
    this.update(INITIAL_SNAPSHOT);
  }

  private ensureAudio(): AudioElementLike {
    if (this.audio) return this.audio;
    const audio = this.createAudio();
    audio.preload = "metadata";
    this.audio = audio;
    this.addEvent(audio, "loadedmetadata", this.handleMetadata);
    this.addEvent(audio, "durationchange", this.handleMetadata);
    this.addEvent(audio, "timeupdate", this.handleTimeUpdate);
    this.addEvent(audio, "play", this.handlePlay);
    this.addEvent(audio, "pause", this.handlePause);
    this.addEvent(audio, "ended", this.handleEnded);
    this.addEvent(audio, "error", this.handleError);
    return audio;
  }

  private addEvent(audio: AudioElementLike, type: string, handler: EventListener): void {
    this.eventHandlers.set(type, handler);
    audio.addEventListener(type, handler);
  }

  private async loadCurrent(generation: number): Promise<void> {
    if (generation !== this.generation) return;
    const sourceIndex = this.snapshot.queueIndex;
    const source = this.sources[sourceIndex];
    if (!source) {
      this.finish(generation);
      return;
    }
    const audio = this.ensureAudio();
    audio.preload = "metadata";
    audio.src = source.url;
    audio.load();
    this.update({
      status: "loading",
      currentTime: 0,
      duration: source.durationMs != null ? Math.max(0, source.durationMs / 1000) : null,
      error: null,
    });

    try {
      await audio.play();
      if (generation === this.generation && this.audio === audio) this.update({ status: "playing" });
    } catch (error) {
      if (
        generation === this.generation &&
        this.audio === audio &&
        this.snapshot.queueIndex === sourceIndex
      ) {
        this.handleSourceFailure(generation, safePlaybackError(error));
      }
    }
  }

  private handleMetadata: EventListener = () => {
    if (!this.audio) return;
    const duration = this.audio.duration;
    if (Number.isFinite(duration) && duration >= 0) this.update({ duration });
  };

  private handleTimeUpdate: EventListener = () => {
    if (!this.audio || !Number.isFinite(this.audio.currentTime)) return;
    this.update({ currentTime: Math.max(0, this.audio.currentTime) });
  };

  private handlePlay: EventListener = () => {
    if (this.audio) this.update({ status: "playing", error: null });
  };

  private handlePause: EventListener = () => {
    if (this.audio && this.snapshot.status !== "loading" && this.snapshot.status !== "ended") {
      this.update({ status: "paused" });
    }
  };

  private handleEnded: EventListener = () => {
    const generation = this.generation;
    if (this.snapshot.queueIndex + 1 < this.sources.length) {
      this.update({
        status: "loading",
        queueIndex: this.snapshot.queueIndex + 1,
        currentTime: 0,
        duration: null,
      });
      void this.loadCurrent(generation);
      return;
    }
    this.finish(generation);
  };

  private handleError: EventListener = () => {
    this.handleSourceFailure(this.generation, "audio_playback_failed");
  };

  private handleSourceFailure(generation: number, error: string): void {
    if (generation !== this.generation) return;
    if (this.snapshot.queueIndex + 1 >= this.sources.length) {
      this.fail(generation, error);
      return;
    }

    // A broken note segment must not permanently block Play All. Release the
    // failed element before advancing so there is still exactly one active
    // HTMLAudioElement and no stale error event can skip another segment.
    this.releaseAudio();
    this.update({
      status: "loading",
      queueIndex: this.snapshot.queueIndex + 1,
      currentTime: 0,
      duration: null,
      error: null,
    });
    void this.loadCurrent(generation);
  }

  private finish(generation: number): void {
    if (generation !== this.generation) return;
    const key = this.snapshot.key;
    const currentTime = this.snapshot.duration ?? this.snapshot.currentTime;
    this.releaseAudio();
    this.sources = [];
    this.update({
      status: "ended",
      key,
      queueIndex: 0,
      queueLength: 0,
      currentTime,
      duration: this.snapshot.duration,
      error: null,
    });
  }

  private fail(generation: number, error: string): void {
    if (generation !== this.generation) return;
    const key = this.snapshot.key;
    this.releaseAudio();
    this.sources = [];
    this.update({
      status: "error",
      key,
      queueIndex: 0,
      queueLength: 0,
      currentTime: 0,
      duration: null,
      error: error || "audio_playback_failed",
    });
  }

  private releaseAudio(): void {
    const audio = this.audio;
    if (!audio) return;
    for (const [type, handler] of this.eventHandlers) audio.removeEventListener(type, handler);
    this.eventHandlers.clear();
    audio.pause();
    audio.removeAttribute("src");
    audio.load();
    this.audio = null;
  }

  private update(patch: Partial<AudioPlaybackSnapshot>): void {
    this.snapshot = { ...this.snapshot, ...patch };
    for (const listener of this.listeners) listener();
  }
}

export const audioPlaybackController = new AudioPlaybackController();
