import { describe, expect, test } from "bun:test";
import type { ControlFailureDto } from "../protocol/sa-types";
import { InvokeError, parseFailure } from ".";
import { ControlError, isBusyLoading } from "../control/client";

describe("structured control failures", () => {
  test("preserves a vegetation graph diagnostic through both error classes", () => {
    const failure: ControlFailureDto = {
      code: "diagnostic",
      message: "graph candidates limit exceeded: requested 16, limit 4",
      diagnostic: {
        domain: "vegetation-graph",
        detail: {
          category: "limit",
          resource: "candidates",
          requested: "16",
          limit: "4",
        },
      },
    };

    const decoded = parseFailure(JSON.stringify(failure));
    const invoked = new InvokeError(decoded);
    const controlled = new ControlError(invoked.failure);

    expect(controlled.failure).toEqual(failure);
    expect(controlled.diagnostic?.detail).toEqual(failure.diagnostic.detail);
    expect(controlled.message).toBe(failure.message);
  });

  test("recognizes busy loading by the generated discriminator", () => {
    const error = new ControlError({
      code: "busy-loading",
      message: "engine busy loading project",
    });
    expect(isBusyLoading(error)).toBe(true);
  });

  test("rejects the string error representation", () => {
    expect(parseFailure(JSON.stringify("command failed"))).toEqual({
      code: "malformed-reply",
      message: "native bridge returned an invalid failure object",
    });
  });

  test("rejects unknown failure fields", () => {
    expect(parseFailure('{"code":"command","message":"nope","extra":true}').code).toBe(
      "malformed-reply",
    );
  });
});
