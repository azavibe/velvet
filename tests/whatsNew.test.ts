import { describe, expect, test } from "bun:test";
import { whatsNewReleaseKey } from "../src/config/whatsNew";

describe("What's New release identity", () => {
  test("combines the application version with the release-note revision", () => {
    expect(whatsNewReleaseKey("0.8.8")).toBe("0.8.8:2026-08-24");
  });

  test("changes when the application version changes", () => {
    expect(whatsNewReleaseKey("0.8.9")).not.toBe(whatsNewReleaseKey("0.8.8"));
  });

  test("release manifests and changelog agree on the current version", async () => {
    const packageJson = await Bun.file("package.json").json();
    const tauriConfig = await Bun.file("src-tauri/tauri.conf.json").json();
    const cargoToml = await Bun.file("src-tauri/Cargo.toml").text();
    const cargoLock = await Bun.file("src-tauri/Cargo.lock").text();
    const changelog = await Bun.file("docs/CHANGELOG.md").text();
    const version = packageJson.version as string;

    expect(tauriConfig.version).toBe(version);
    expect(cargoToml).toMatch(
      new RegExp(`\\[package\\][\\s\\S]*?version = "${version.replaceAll(".", "\\.")}"`),
    );
    expect(cargoLock).toMatch(
      new RegExp(`name = "whisperi"\\r?\\nversion = "${version.replaceAll(".", "\\.")}"`),
    );
    expect(changelog).toMatch(
      new RegExp(`## \\[${version.replaceAll(".", "\\.")}\\][\\s\\S]*?### Highlights`),
    );
  });
});
