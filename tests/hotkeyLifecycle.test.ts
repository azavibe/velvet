import { describe, expect, test } from "bun:test";
import {
  HotkeyRegistrationController,
  type HotkeyEvent,
} from "../src/services/hotkeyLifecycle";

const event: HotkeyEvent = { state: "Pressed" };

describe("HotkeyRegistrationController", () => {
  test("serializes replacement and cleanup when an old registration is still awaiting native setup", async () => {
    const calls: string[] = [];
    let releaseFirst!: () => void;
    const firstRegistration = new Promise<void>((resolve) => {
      releaseFirst = resolve;
    });

    const controller = new HotkeyRegistrationController({
      register: async (shortcut) => {
        calls.push(`register:${shortcut}`);
        if (shortcut === "Ctrl+1") await firstRegistration;
      },
      unregister: async (shortcut) => {
        calls.push(`unregister:${shortcut}`);
      },
    });

    const first = controller.update("Ctrl+1", true, () => {});
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    expect(calls).toEqual(["register:Ctrl+1"]);
    const disabled = controller.disable();
    const second = controller.update("Ctrl+2", true, () => {});
    releaseFirst();

    await Promise.all([first, disabled, second]);
    expect(calls).toEqual([
      "register:Ctrl+1",
      "unregister:Ctrl+1",
      "register:Ctrl+2",
    ]);

    await controller.disable();
    expect(calls).toEqual([
      "register:Ctrl+1",
      "unregister:Ctrl+1",
      "register:Ctrl+2",
      "unregister:Ctrl+2",
    ]);
  });

  test("does not duplicate ordinary callback updates and retries the active registration", async () => {
    const calls: string[] = [];
    const controller = new HotkeyRegistrationController({
      register: async (shortcut, callback) => {
        calls.push(`register:${shortcut}`);
        callback(event);
      },
      unregister: async (shortcut) => {
        calls.push(`unregister:${shortcut}`);
      },
    });

    await controller.update("Ctrl+1", true, () => {});
    await controller.retry();
    expect(calls).toEqual([
      "register:Ctrl+1",
      "unregister:Ctrl+1",
      "register:Ctrl+1",
    ]);
  });

  test("surfaces native registration failures without throwing from the lifecycle queue", async () => {
    const failures: string[] = [];
    const controller = new HotkeyRegistrationController({
      register: async () => {
        throw new Error("native registration rejected");
      },
      unregister: async () => {},
      onFailure: (phase) => failures.push(phase),
    });

    await controller.update("Ctrl+1", true, () => {});
    expect(failures).toEqual(["register"]);
  });
});
