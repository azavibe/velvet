import { describe, expect, test } from "bun:test";
import {
  reconcileSpeech,
  reconciliationPrompts,
  type ReconciliationContext,
} from "../src/services/contextualReconciliation";

const context: ReconciliationContext = {
  dictionary: [
    { term: "Asmodee", aliases: ["Asma Eye"], policy: "contextual" },
  ],
  agentName: "Aral",
  agentAliases: [],
  recentTexts: ["Send the revised contract to Asmodee before Friday."],
  language: "en",
};

describe("automatic contextual speech correction", () => {
  test("accepts a high-confidence dictionary-backed ASR correction", async () => {
    const result = await reconcileSpeech(
      "Send this to Asma Eye tomorrow.",
      context,
      async () => JSON.stringify({ text: "Send this to Asmodee tomorrow.", confidence: 0.97 }),
    );
    expect(result).toEqual({
      text: "Send this to Asmodee tomorrow.",
      status: "accepted",
      confidence: 0.97,
      evidence: "dictionary",
    });
  });

  test("accepts a bounded recent-context entity correction", async () => {
    const result = await reconcileSpeech(
      "Email the Asma Eye contract.",
      { ...context, dictionary: [] },
      async () => JSON.stringify({ text: "Email the Asmodee contract.", confidence: 0.95 }),
    );
    expect(result.status).toBe("accepted");
    expect(result.evidence).toBe("recent");
  });

  test("rejects paraphrasing even when the model is confident", async () => {
    const raw = "Send this to Asma Eye tomorrow.";
    const result = await reconcileSpeech(
      raw,
      context,
      async () => JSON.stringify({ text: "Please deliver it to Asmodee the next day.", confidence: 0.99 }),
    );
    expect(result.text).toBe(raw);
    expect(result.status).toBe("uncertain");
  });

  test("rejects unsupported guesses and low confidence", async () => {
    const unsupported = await reconcileSpeech(
      "Call Jon tomorrow.",
      context,
      async () => JSON.stringify({ text: "Call John tomorrow.", confidence: 0.99 }),
    );
    expect(unsupported.status).toBe("uncertain");
    const weak = await reconcileSpeech(
      "Send this to Asma Eye.",
      context,
      async () => JSON.stringify({ text: "Send this to Asmodee.", confidence: 0.7 }),
    );
    expect(weak.status).toBe("uncertain");
  });

  test("rejects invalid confidence and substring-only dictionary evidence", async () => {
    const invalidConfidence = await reconcileSpeech(
      "Send this to Asma Eye.",
      context,
      async () => JSON.stringify({ text: "Send this to Asmodee.", confidence: 97 }),
    );
    expect(invalidConfidence.status).toBe("failed");

    const substring = await reconcileSpeech(
      "Launch apple.",
      {
        ...context,
        dictionary: [{ term: "App", aliases: ["app"], policy: "contextual" }],
        recentTexts: [],
      },
      async () => JSON.stringify({ text: "Launch App.", confidence: 0.99 }),
    );
    expect(substring.status).toBe("uncertain");
  });

  test("falls back unchanged on malformed output or provider failure", async () => {
    const malformed = await reconcileSpeech("hello", context, async () => "hello");
    expect(malformed.status).toBe("failed");
    const failed = await reconcileSpeech("hello", context, async () => {
      throw new Error("offline");
    });
    expect(failed).toEqual({
      text: "hello",
      status: "failed",
      confidence: null,
      evidence: null,
    });

    const timedOut = await reconcileSpeech(
      "hello",
      context,
      () => new Promise(() => undefined),
      5,
    );
    expect(timedOut.status).toBe("failed");
  });

  test("bounds recent context and never asks for prose rewriting", () => {
    const prompts = reconciliationPrompts("raw", {
      ...context,
      recentTexts: Array.from({ length: 20 }, (_, index) => `${index}-${"x".repeat(300)}`),
    });
    const payload = JSON.parse(prompts.userPrompt);
    expect(payload.recent_text.length).toBeLessThanOrEqual(8);
    expect(prompts.userPrompt.length).toBeLessThan(2000);
    expect(prompts.systemPrompt).toContain("Do not rewrite grammar");
  });
});
