import { describe, expect, test } from "bun:test";
import {
  memoryExtractionPrompts,
  parseMemoryCandidates,
} from "../src/services/memory";

describe("bounded long-term memory extraction", () => {
  test("parses only bounded, high-confidence structured candidates", () => {
    const memories = Array.from({ length: 25 }, (_, index) => ({
      kind: "entity",
      canonical_text: `Project ${index}`,
      subject: `Project ${index}`,
      predicate: null,
      object: null,
      aliases: Array.from({ length: 15 }, (__, alias) => `P${index}-${alias}`),
      confidence: 0.9,
    }));
    memories[1].confidence = 0.2;
    const parsed = parseMemoryCandidates(JSON.stringify({ memories }));
    expect(parsed.length).toBe(19);
    expect(parsed[0].aliases.length).toBe(12);
    expect(parsed.some((item) => item.canonical_text === "Project 1")).toBe(false);
  });

  test("rejects malformed and oversized model output", () => {
    expect(parseMemoryCandidates("not json")).toEqual([]);
    expect(parseMemoryCandidates(JSON.stringify({ memories: [{
      kind: "fact",
      canonical_text: "x".repeat(501),
      confidence: 0.99,
    }] }))).toEqual([]);
  });

  test("bounds source text and explicitly excludes secrets and transient tasks", () => {
    const prompts = memoryExtractionPrompts("x".repeat(5_000));
    const payload = JSON.parse(prompts.userPrompt);
    expect(payload.speech.length).toBe(4_000);
    expect(prompts.systemPrompt).toContain("Do not infer");
    expect(prompts.systemPrompt).toContain("secrets");
    expect(prompts.systemPrompt).toContain("transient requests");
  });
});
