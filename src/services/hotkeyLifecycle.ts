export type HotkeyEventState = "Pressed" | "Released" | string;

export interface HotkeyEvent {
  state: HotkeyEventState;
}

export type RegisterHotkey = (
  shortcut: string,
  callback: (event: HotkeyEvent) => void,
) => Promise<void>;

export type UnregisterHotkey = (shortcut: string) => Promise<void>;

export type HotkeyFailurePhase = "register" | "unregister";

export interface HotkeyRegistrationControllerOptions {
  register: RegisterHotkey;
  unregister: UnregisterHotkey;
  onFailure?: (phase: HotkeyFailurePhase) => void;
  onRegistered?: (shortcut: string) => void;
}

interface DesiredRegistration {
  shortcut: string;
  callback: (event: HotkeyEvent) => void;
  enabled: boolean;
}

/**
 * Serializes the global-shortcut plugin lifecycle. React effects can be
 * started and cleaned up asynchronously (especially under StrictMode), so a
 * stale unregister must never clear a newer registration reference.
 */
export class HotkeyRegistrationController {
  private readonly registerFn: RegisterHotkey;
  private readonly unregisterFn: UnregisterHotkey;
  private readonly onFailure?: (phase: HotkeyFailurePhase) => void;
  private readonly onRegistered?: (shortcut: string) => void;
  private registeredShortcut: string | null = null;
  private desired: DesiredRegistration | null = null;
  private generation = 0;
  private queue: Promise<void> = Promise.resolve();

  constructor(options: HotkeyRegistrationControllerOptions) {
    this.registerFn = options.register;
    this.unregisterFn = options.unregister;
    this.onFailure = options.onFailure;
    this.onRegistered = options.onRegistered;
  }

  update(
    shortcut: string,
    enabled: boolean,
    callback: (event: HotkeyEvent) => void,
  ): Promise<void> {
    const generation = ++this.generation;
    this.desired = { shortcut, enabled, callback };
    return this.enqueue(async () => {
      await this.unbind();
      if (generation !== this.generation) return;

      const current = this.desired;
      if (!current?.enabled || !current.shortcut) return;

      try {
        await this.registerFn(current.shortcut, current.callback);
      } catch {
        this.onFailure?.("register");
        return;
      }

      // A newer update may have arrived while register() was awaiting the
      // native plugin. Do not leave the stale callback installed.
      if (generation !== this.generation || this.desired !== current) {
        await this.safeUnregister(current.shortcut);
        return;
      }
      this.registeredShortcut = current.shortcut;
      this.onRegistered?.(current.shortcut);
    });
  }

  disable(): Promise<void> {
    this.generation += 1;
    this.desired = null;
    return this.enqueue(() => this.unbind());
  }

  retry(): Promise<void> {
    const current = this.desired;
    if (!current?.enabled || !current.shortcut) return Promise.resolve();
    return this.update(current.shortcut, true, current.callback);
  }

  private enqueue(operation: () => Promise<void>): Promise<void> {
    const next = this.queue.then(operation, operation);
    this.queue = next.catch(() => undefined);
    return next;
  }

  private async unbind(): Promise<void> {
    const shortcut = this.registeredShortcut;
    this.registeredShortcut = null;
    if (!shortcut) return;
    await this.safeUnregister(shortcut);
  }

  private async safeUnregister(shortcut: string): Promise<void> {
    try {
      await this.unregisterFn(shortcut);
    } catch {
      this.onFailure?.("unregister");
    }
  }
}
