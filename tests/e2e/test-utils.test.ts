import { expect, test } from "bun:test";
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
