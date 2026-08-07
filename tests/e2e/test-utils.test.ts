import { expect, test } from "bun:test";
import { EngineCallError } from "./harness.ts";
import { Cleaner } from "./test-utils.ts";

test("Cleaner runs cleanup in reverse registration order and drains once", async () => {
  const cleaner = new Cleaner();
  const order: string[] = [];
  cleaner.defer(() => order.push("A"));
  cleaner.defer(async () => {
    await Promise.resolve();
    order.push("B");
  });
  cleaner.defer(() => order.push("C"));

  await cleaner.cleanup();
  await cleaner.cleanup();

  expect(order).toEqual(["C", "B", "A"]);
});

test("Cleaner attempts every cleanup before reporting failures", async () => {
  const cleaner = new Cleaner();
  const completed: string[] = [];
  cleaner.defer(() => completed.push("first"));
  cleaner.defer(() => {
    throw new Error("middle failed");
  });
  cleaner.defer(() => completed.push("last"));

  await expect(cleaner.cleanup()).rejects.toThrow("middle failed");
  expect(completed).toEqual(["last", "first"]);
});

test("EngineCallError preserves the complete shared diagnostic", () => {
  const failure = {
    code: "diagnostic" as const,
    message: "graph candidates limit exceeded: requested 16, limit 4",
    diagnostic: {
      domain: "vegetation-graph" as const,
      detail: {
        category: "limit" as const,
        resource: "candidates",
        requested: "16",
        limit: "4",
      },
    },
  };

  const error = new EngineCallError("vegetation-compile-biome", failure);
  expect(error.failure).toEqual(failure);
  expect(error.message).toBe(
    "vegetation-compile-biome: graph candidates limit exceeded: requested 16, limit 4",
  );
});
