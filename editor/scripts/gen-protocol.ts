import { spawn } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const editorDir = dirname(scriptDir);
const repoRoot = dirname(editorDir);
const engineDir = join(repoRoot, "engine");

// The TypeScript and Luau definitions plus the envelope, OpenRPC, and manifest schemas are emitted
// by the Rust workspace tooling bin from the saffron-protocol DTO crate.
const child = spawn("cargo", ["run", "-p", "xtask", "--", "gen-protocol"], {
  cwd: engineDir,
  env: process.env,
  stdio: "inherit",
});

child.on("error", (err) => {
  console.error(err);
  process.exit(1);
});

child.on("exit", (code, signal) => {
  if (signal) {
    console.error(`DTO protocol generator exited with signal ${signal}`);
    process.exit(1);
  }
  process.exit(code ?? 0);
});
