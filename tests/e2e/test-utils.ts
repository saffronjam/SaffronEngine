import { existsSync, readFileSync, rmSync } from "node:fs";
import { Engine } from "./harness.ts";

type Cleanup = () => void | Promise<void>;

/** Runs registered cleanup functions once, in reverse registration order. */
export class Cleaner {
  private readonly cleanups: Cleanup[] = [];

  defer(cleanup: Cleanup): void {
    this.cleanups.push(cleanup);
  }

  track<T>(value: T, cleanup: (value: T) => void | Promise<void>): T {
    this.defer(() => cleanup(value));
    return value;
  }

  async cleanup(): Promise<void> {
    const errors: unknown[] = [];
    await this.drain(errors);
    if (errors.length === 1) {
      throw errors[0];
    }
    if (errors.length > 1) {
      throw new AggregateError(errors, `${errors.length} cleanup operations failed`);
    }
  }

  private async drain(errors: unknown[]): Promise<void> {
    const cleanup = this.cleanups.pop();
    if (!cleanup) {
      return;
    }
    try {
      await cleanup();
    } catch (error) {
      errors.push(error);
    }
    await this.drain(errors);
  }
}

/** Boots an engine and immediately registers its shutdown. */
export async function bootEngine(
  cleaner: Cleaner,
  env: Record<string, string> = {},
): Promise<Engine> {
  const engine = await Engine.boot(env);
  cleaner.defer(() => engine.shutdown());
  return engine;
}

interface ScenePreparation {
  width?: number;
  height?: number;
  camera?: Record<string, unknown>;
}

/** Applies the common deterministic viewport and optional camera setup for scene tests. */
export async function prepareScene(
  engine: Engine,
  preparation: ScenePreparation = {},
): Promise<void> {
  await engine.call("set-viewport-size", {
    view: "scene",
    width: preparation.width ?? 480,
    height: preparation.height ?? 270,
  });
  if (preparation.camera) {
    await engine.call("set-camera", preparation.camera);
  }
}

/** Registers an entity for destruction and returns the original value. */
export function trackEntity<T extends string | { id: string }>(
  cleaner: Cleaner,
  engine: Engine,
  entity: T,
): T {
  const id = typeof entity === "string" ? entity : entity.id;
  cleaner.defer(() => engine.call("destroy-entity", { entity: id }).then(() => undefined));
  return entity;
}

/** Captures the scene viewport and registers the temporary PNG for cleanup. */
export async function captureViewport(
  engine: Engine,
  cleaner: Cleaner,
  tag: string,
  settleMs = 200,
): Promise<Buffer> {
  const path = `/tmp/saffron-e2e-${process.pid}-${tag}.png`;
  cleaner.defer(() => rmSync(path, { force: true }));
  rmSync(path, { force: true });
  await engine.call("screenshot", { target: "viewport", path });
  await waitForFile(engine, path, tag, Date.now() + 10_000);
  await engine.settle(settleMs);
  return readFileSync(path);
}

async function waitForFile(
  engine: Engine,
  path: string,
  tag: string,
  deadline: number,
): Promise<void> {
  if (existsSync(path)) {
    return;
  }
  if (Date.now() > deadline) {
    throw new Error(`screenshot ${tag} never landed at ${path}`);
  }
  await engine.settle(100);
  await waitForFile(engine, path, tag, deadline);
}
