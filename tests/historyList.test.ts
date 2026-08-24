import { expect, test } from "bun:test";
import { mergeUniqueById, uniqueHistoryItems } from "../src/components/settings/historyList";

test("overlapping History pages merge each source once", () => {
  expect(
    mergeUniqueById(
      [{ id: 1, value: "old" }, { id: 2, value: "two" }],
      [{ id: 1, value: "duplicate" }, { id: 3, value: "three" }],
    ),
  ).toEqual([
    { id: 1, value: "old" },
    { id: 2, value: "two" },
    { id: 3, value: "three" },
  ]);
});

test("a source with multiple audio assets still renders as one History item", () => {
  expect(
    uniqueHistoryItems([
      { id: "c-7", audioAssetIds: [11] },
      { id: "c-7", audioAssetIds: [11, 12] },
      { id: "d-8", audioAssetIds: [] },
    ]),
  ).toEqual([
    { id: "c-7", audioAssetIds: [11] },
    { id: "d-8", audioAssetIds: [] },
  ]);
});
