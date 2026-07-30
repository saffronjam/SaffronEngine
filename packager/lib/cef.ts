import { $ } from "bun";
import { stat } from "node:fs/promises";
import { join } from "node:path";

const RESOURCES = ["icudtl.dat", "resources.pak", "v8_context_snapshot.bin", "chrome_100_percent.pak"];

async function sizeOf(path: string): Promise<number> {
  return stat(path)
    .then((info) => info.size)
    .catch(() => -1);
}

/// icudtl.dat is ~10 MB, so anything tiny is a truncated extraction rather than a real file.
async function intact(cefDir: string): Promise<boolean> {
  for (const file of RESOURCES) {
    if ((await sizeOf(join(cefDir, file))) <= 0) return false;
  }
  return (await sizeOf(join(cefDir, "icudtl.dat"))) >= 1_000_000;
}

/// An interrupted cef-dll-sys extraction leaves 0-byte icudtl.dat/*.pak that it never re-provisions
/// (it only downloads when the dir is absent), and CEF aborts with "Couldn't mmap icu data file".
/// Purge and rebuild once on a truncated resource; throw if it recurs.
export async function verifyCefRuntime(shellDir: string, profile: "release" | "debug"): Promise<void> {
  const cefDir = join(shellDir, "target", profile);
  if (await intact(cefDir)) return;

  await $`cargo clean -p cef-dll-sys`.cwd(shellDir).quiet();
  if (profile === "release") {
    await $`cargo build --release`.cwd(shellDir).quiet();
  } else {
    await $`cargo build`.cwd(shellDir).quiet();
  }

  if (!(await intact(cefDir))) {
    throw new Error(`CEF runtime under ${cefDir} is still truncated after re-provision`);
  }
}
