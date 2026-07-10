import { $ } from "bun";
import { stat } from "node:fs/promises";
import { join } from "node:path";

const RESOURCES = ["icudtl.dat", "resources.pak", "v8_context_snapshot.bin", "chrome_100_percent.pak"];

async function sizeOf(path: string): Promise<number> {
  return stat(path)
    .then((info) => info.size)
    .catch(() => -1);
}

/// The CEF resource data (following symlinks) must be present and non-empty; icudtl.dat is ~10 MB, so
/// anything tiny is a truncated extraction rather than a real file.
async function intact(cefDir: string): Promise<boolean> {
  for (const file of RESOURCES) {
    if ((await sizeOf(join(cefDir, file))) <= 0) return false;
  }
  return (await sizeOf(join(cefDir, "icudtl.dat"))) >= 1_000_000;
}

/// cef-dll-sys stages the CEF runtime next to the shell binary. An interrupted extraction leaves
/// 0-byte icudtl.dat/*.pak, which it never re-provisions on its own (it only downloads when the dir
/// is absent, never re-checking an existing one) and CEF then aborts at startup with "Couldn't mmap
/// icu data file". On a truncated resource, purge cef-dll-sys and rebuild once to force a clean
/// re-provision; throw if it recurs.
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
