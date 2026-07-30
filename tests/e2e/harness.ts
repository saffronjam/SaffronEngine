// End-to-end harness for SaffronAnima: boots a real engine headlessly and drives it over
// the JSON-over-unix-socket control plane — the same wire the editor and `sa` CLI use.
// Tests are plain TypeScript on `bun test`.
//
// Each Engine launches the saffron-host binary pointed at a per-run control socket, rendering
// offscreen so no window and no compositor are involved. Engine stdout+stderr (incl. validation
// messages) is captured into `.log` for assertions.

import { spawn, type ChildProcess } from "node:child_process";
import net from "node:net";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type { CommandParamsMap, ControlFailureDto } from "@saffron/protocol";

// Every command the control plane serves. `call` takes only these, so a renamed or retired command
// fails to typecheck instead of failing at runtime against a live host.
export type CommandName = keyof CommandParamsMap;

// One command's request payload: its named params, or the positional `args` form the engine folds
// into those names.
export type CommandParams = CommandParamsMap[CommandName] | { args: unknown[] };

const HERE = dirname(fileURLToPath(import.meta.url));
export const REPO = join(HERE, "..", "..");
export const ENGINE_BIN =
  process.env.SAFFRON_ANIMA_BIN ?? join(REPO, "engine", "target", "debug", "saffron-host");

const delay = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

export const IS_MACOS = process.platform === "darwin";

// A rejected engine command carrying the exact shared control failure.
export class EngineCallError extends Error {
  constructor(
    readonly command: string,
    readonly failure: ControlFailureDto,
  ) {
    super(`${command}: ${failure.message}`);
    this.name = "EngineCallError";
  }
}

// macOS has no Wayland compositor; the offscreen host needs none. It needs MoltenVK's ICD plus
// Homebrew's validation-layer manifest and dynamic-library directory. Applied only when Vulkan
// discovery is not already configured, so an explicit override still wins.
export function macosVulkanEnv(): Record<string, string> {
  const candidates = [
    "/opt/homebrew/etc/vulkan/icd.d/MoltenVK_icd.json",
    "/usr/local/etc/vulkan/icd.d/MoltenVK_icd.json",
  ];
  const layerCandidates = [
    {
      manifest: "/opt/homebrew/opt/vulkan-validationlayers/share/vulkan/explicit_layer.d",
      library: "/opt/homebrew/opt/vulkan-validationlayers/lib",
    },
    {
      manifest: "/usr/local/opt/vulkan-validationlayers/share/vulkan/explicit_layer.d",
      library: "/usr/local/opt/vulkan-validationlayers/lib",
    },
  ];
  const icd = candidates.find((p) => existsSync(p)) ?? candidates[0];
  const env: Record<string, string> = {};
  if (process.env.VK_ICD_FILENAMES === undefined) env.VK_ICD_FILENAMES = icd;
  const layer = layerCandidates.find(
    ({ manifest, library }) => existsSync(manifest) && existsSync(library),
  );
  if (process.env.VK_LAYER_PATH === undefined && layer !== undefined) {
    env.VK_LAYER_PATH = layer.manifest;
    env.DYLD_FALLBACK_LIBRARY_PATH = process.env.DYLD_FALLBACK_LIBRARY_PATH
      ? `${layer.library}:${process.env.DYLD_FALLBACK_LIBRARY_PATH}`
      : layer.library;
  }
  return env;
}

async function waitFor(ready: () => boolean, timeoutMs: number, what: string): Promise<void> {
  const start = Date.now();
  while (!ready()) {
    if (Date.now() - start > timeoutMs) {
      throw new Error(`timeout waiting for ${what}`);
    }
    await delay(50);
  }
}

// A booted engine plus a typed control client. Always call shutdown() (afterAll/finally).
export class Engine {
  readonly socketPath: string;
  // The per-boot app-data root the host writes its `userdata/` (and any scratch project) into.
  // The harness owns a fresh temp dir per boot and removes it on shutdown, so runs are isolated
  // and never pollute the source tree. A caller that passes its own `SAFFRON_APPDATA_DIR` owns it.
  readonly appdata: string;
  private proc: ChildProcess;
  private exited = false;
  private buf = "";
  private nextId = 1;
  private ownsAppdata: boolean;

  private constructor(
    proc: ChildProcess,
    socketPath: string,
    appdata: string,
    ownsAppdata: boolean,
  ) {
    this.proc = proc;
    this.socketPath = socketPath;
    this.appdata = appdata;
    this.ownsAppdata = ownsAppdata;
  }

  // Everything the engine has written to stdout+stderr so far.
  get log(): string {
    return this.buf;
  }

  // Lines the validation layers flagged as errors (empty = clean). The engine's debug
  // messenger prints them as `<ts>  ERROR  vulkan  [validation] …` (ANSI off when piped).
  validationErrors(): string[] {
    return this.buf.split("\n").filter((line) => /ERROR\s+vulkan\s+\[validation\]/.test(line));
  }

  static async boot(env: Record<string, string> = {}): Promise<Engine> {
    const stamp = `${process.pid}-${Date.now()}`;

    // A per-boot app-data root under the temp dir so a booted project (e.g. SAFFRON_SCRATCH_PROJECT)
    // writes its userdata/ there and never pollutes the source tree — the host runs with cwd=REPO,
    // where the default relative appdata/ would otherwise land. A caller that sets its own
    // SAFFRON_APPDATA_DIR owns cleanup; otherwise the harness removes this dir on shutdown.
    const ownsAppdata = env.SAFFRON_APPDATA_DIR === undefined;
    const appdata = ownsAppdata
      ? mkdtempSync(join(tmpdir(), "saffron-e2e-appdata-"))
      : env.SAFFRON_APPDATA_DIR;

    const socketPath = `/tmp/saffron-e2e-${stamp}.sock`;
    const proc = spawn(ENGINE_BIN, [], {
      cwd: REPO,
      env: {
        ...process.env,
        ...(IS_MACOS ? macosVulkanEnv() : {}),
        // The offscreen (no-window) host on every platform: it needs no compositor, so runs are
        // isolated by construction, and device selection is free to take the discrete GPU — a
        // windowed boot has to qualify on present support, which a headless compositor denies to
        // a discrete adapter. It is also the mode the editor drives.
        SAFFRON_EDITOR_NATIVE_VIEWPORT: "1",
        SAFFRON_CONTROL_SOCK: socketPath,
        SAFFRON_APPDATA_DIR: appdata,
        ...env,
      },
      stdio: ["ignore", "pipe", "pipe"],
    });
    const engine = new Engine(proc, socketPath, appdata, ownsAppdata);
    proc.stdout?.on("data", (d) => (engine.buf += d.toString()));
    proc.stderr?.on("data", (d) => (engine.buf += d.toString()));
    proc.on("exit", () => (engine.exited = true));

    await waitFor(() => engine.exited || existsSync(socketPath), 30_000, "control socket");
    if (engine.exited) {
      engine.cleanupAppdata();
      throw new Error(`engine exited before the control socket appeared:\n${engine.buf}`);
    }
    // The bootstrap load (env/scratch project) is non-blocking, so the socket answers before the
    // project is up. When a project is expected, wait for it to reach `ready` so a test never races
    // the load (an empty scene or a `busy-loading` reply).
    const expectsProject =
      (env.SAFFRON_PROJECT ?? "") !== "" || (env.SAFFRON_SCRATCH_PROJECT ?? "") !== "";
    if (expectsProject) {
      try {
        await engine.awaitProjectReady();
      } catch (err) {
        engine.cleanupAppdata();
        throw new Error(`project did not load on boot: ${String(err)}\n${engine.buf}`);
      }
    }
    return engine;
  }

  private cleanupAppdata(): void {
    if (this.ownsAppdata) {
      rmSync(this.appdata, { recursive: true, force: true });
    }
  }

  // Send one control command; resolves its `result`, rejects on `ok:false` or transport error.
  call<T = unknown>(cmd: CommandName, params: CommandParams = {}): Promise<T> {
    return new Promise<T>((resolve, reject) => {
      const socket = net.connect({ path: this.socketPath });
      const id = this.nextId++;
      let data = "";
      // Default 15s; overridable for slow environments (a cold pipeline cache on a proxied
      // GPU driver can stall the host's control drain past 15s during first-render PSO
      // compilation) via SAFFRON_E2E_CALL_TIMEOUT_MS.
      const timer = setTimeout(
        () => {
          socket.destroy();
          const tail = this.buf.split("\n").slice(-40).join("\n");
          reject(new Error(`timeout calling ${cmd}\n--- engine log tail ---\n${tail}`));
        },
        Number(process.env.SAFFRON_E2E_CALL_TIMEOUT_MS) || 15_000,
      );
      socket.on("connect", () => socket.write(JSON.stringify({ id, cmd, params }) + "\n"));
      socket.on("data", (chunk) => {
        data += chunk.toString();
        const nl = data.indexOf("\n");
        if (nl < 0) {
          return;
        }
        clearTimeout(timer);
        socket.end();
        let envelope:
          | { id: unknown; ok: true; result: T }
          | { id: unknown; ok: false; error: ControlFailureDto };
        try {
          envelope = JSON.parse(data.slice(0, nl));
        } catch (err) {
          reject(err as Error);
          return;
        }
        if (envelope.ok === false) {
          reject(new EngineCallError(cmd, envelope.error));
        } else {
          resolve(envelope.result);
        }
      });
      socket.on("error", (err) => {
        clearTimeout(timer);
        reject(err);
      });
    });
  }

  // Fetch a thumbnail, transparently retrying the `pending` reply the engine sends while its
  // worker thread generates a cold-cache entry (mirrors the editor's backoff). Resolves the final
  // PNG reply; rejects on an engine error or after `timeoutMs`.
  async getThumbnail<T = { base64: string; width: number; height: number; format: string }>(
    cmd: "get-thumbnail" | "view-asset",
    params: Record<string, unknown>,
    timeoutMs = 10_000,
  ): Promise<T> {
    const start = Date.now();
    let delayMs = 30;
    for (;;) {
      const reply = await this.call<T & { pending?: boolean }>(cmd, params);
      if (!reply.pending) {
        return reply;
      }
      if (Date.now() - start > timeoutMs) {
        throw new Error(`timeout waiting for ${cmd} to resolve (still pending)`);
      }
      await delay(delayMs);
      delayMs = Math.min(delayMs * 2, 500);
    }
  }

  // Let the engine run a few render frames so deferred GPU work + validation surface.
  async settle(ms = 300): Promise<void> {
    await delay(ms);
  }

  // Polls `project-status` until the non-blocking loader reaches `ready`; rejects on `failed` or
  // timeout. Every project bring-up is async — the bootstrap and each lifecycle command kick the
  // load, which then runs across frames — so a test must await this before touching the loaded
  // scene or catalog. `project-status` is allow-listed during `Loading`.
  async awaitProjectReady(timeoutMs = 30_000): Promise<void> {
    const start = Date.now();
    for (;;) {
      let status: {
        phase: string;
        error: string;
        stage?: string;
        currentItem?: string;
        done?: number;
        total?: number;
      } = { phase: "loading", error: "" };
      try {
        status = await this.call<{
          phase: string;
          error: string;
          stage?: string;
          currentItem?: string;
          done?: number;
          total?: number;
        }>("project-status");
      } catch {
        // Socket briefly busy mid-load; retry.
      }
      if (status.phase === "ready") {
        return;
      }
      if (status.phase === "failed") {
        throw new Error(`project load failed: ${status.error}`);
      }
      if (Date.now() - start > timeoutMs) {
        throw new Error(
          `timeout waiting for project ready (phase=${status.phase} stage=${status.stage} ` +
            `${status.done}/${status.total} item=${status.currentItem})`,
        );
      }
      await delay(50);
    }
  }

  // Kick a project open + await the load.
  async loadProject(path: string): Promise<void> {
    await this.call("load-project", { path });
    await this.awaitProjectReady();
  }

  // Kick a project open by folder/path + await the load.
  async openProject(path: string): Promise<void> {
    await this.call("open-project", { path });
    await this.awaitProjectReady();
  }

  // Kick a fresh-project create + await the load.
  async newProject(params: Record<string, unknown>): Promise<void> {
    await this.call("new-project", params);
    await this.awaitProjectReady();
  }

  // Kick a reload of the active project + await the load.
  async reloadProject(): Promise<void> {
    await this.call("reload-project", {});
    await this.awaitProjectReady();
  }

  // Import a glTF/OBJ as a .smodel asset, then instantiate it into the scene, returning the placed
  // root entity. The standard "get a model into the scene" path: import bakes the asset, instantiate
  // places it.
  async importEntity(path: string, name?: string): Promise<{ id: string; name: string }> {
    const model = await this.call<{ id: string }>("import-model", { path });
    const params = name === undefined ? { asset: model.id } : { asset: model.id, name };
    return this.call<{ id: string; name: string }>("instantiate-model", params);
  }

  // The rig descendant of an instantiated model: a skinned model wraps its node forest under a
  // container root, so the SkinnedMesh (and the auto-fit BonePhysics) live on a child. Returns the
  // first entity carrying a SkinnedMesh, falling back to `root` for a non-skinned model.
  async rig(root: string): Promise<string> {
    const { entities } = await this.call<{ entities: { id: string }[] }>("list-entities");
    for (const e of entities) {
      const info = await this.call<{ components: Record<string, unknown> }>("inspect", {
        entity: e.id,
      });
      if (info.components.SkinnedMesh) {
        return e.id;
      }
    }
    return root;
  }

  async shutdown(): Promise<void> {
    try {
      await this.call("quit");
    } catch {
      // already gone, or quit raced the socket close
    }
    this.proc.kill("SIGTERM");
    await delay(100);
    this.cleanupAppdata();
  }
}
