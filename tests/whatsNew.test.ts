import { describe, expect, test } from "bun:test";
import { whatsNewReleaseKey } from "../src/config/whatsNew";

describe("What's New release identity", () => {
  test("combines the application version with the release-note revision", () => {
    expect(whatsNewReleaseKey("0.8.8")).toBe("0.8.8:2026-08-24");
  });

  test("changes when the application version changes", () => {
    expect(whatsNewReleaseKey("0.8.9")).not.toBe(whatsNewReleaseKey("0.8.8"));
  });
});
