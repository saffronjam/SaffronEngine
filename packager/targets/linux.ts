import { $ } from "bun";
import { chmod, cp, mkdir, readdir, rename, rm, stat, symlink } from "node:fs/promises";
import { join } from "node:path";
import { note } from "@clack/prompts";
import { assetsDir, distDir, editorDir, engineDir, repo, shellDir, toolsDir } from "../lib/paths";
import { verifyCefRuntime } from "../lib/cef";
import { step } from "../lib/ui";

const STAGE = join(repo, "build", "appimage");
const APPDIR = join(STAGE, "AppDir");
const OUT = join(distDir, "Saffron_Anima-x86_64.AppImage");
const APPIMAGETOOL_URL =
  "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage";

const CEF_RUNTIME = /\.(so|so\.\d+|pak|bin|dat|json)$/;

export async function packageLinux(): Promise<void> {
  await step("Building host", () =>
    $`cargo build --release --bin saffron-host`.cwd(engineDir).quiet(),
  );
  await step("Compiling shaders", () =>
    $`cargo run -p xtask -- shaders --profile release`.cwd(engineDir).quiet(),
  );
  await step("Building frontend", async () => {
    await $`bun install`.cwd(editorDir).quiet();
    await $`bun run build`.cwd(editorDir).quiet();
  });
  await step("Building CEF shell", () => $`cargo build --release`.cwd(shellDir).quiet());
  await step("Verifying CEF runtime", () => verifyCefRuntime(shellDir, "release"));
  await step("Staging AppDir", () => stageAppDir());
  await step("Packing AppImage", () => packImage());

  note(OUT, "AppImage");
}

async function stageAppDir(): Promise<void> {
  await rm(APPDIR, { recursive: true, force: true });
  const bin = join(APPDIR, "usr/bin");
  const data = join(APPDIR, "usr/share/saffron-anima");
  const applications = join(APPDIR, "usr/share/applications");
  const icons = join(APPDIR, "usr/share/icons/hicolor/scalable/apps");
  for (const dir of [bin, join(data, "assets"), applications, icons, distDir]) {
    await mkdir(dir, { recursive: true });
  }

  await installExe(join(engineDir, "target/release/saffron-host"), join(bin, "saffron-host"));
  await installExe(join(shellDir, "target/release/saffron-editor-shell"), join(bin, "saffron-editor-shell"));
  await bundleCxxRuntime(join(bin, "saffron-host"), bin);

  // CEF resolves libcef.so and its resource packs beside the shell binary, so the runtime staged by
  // cef-dll-sys is copied there; dereference turns its symlinks into real files.
  const release = join(shellDir, "target/release");
  for (const entry of await readdir(release)) {
    if (CEF_RUNTIME.test(entry)) {
      await cp(join(release, entry), join(bin, entry), { dereference: true, preserveTimestamps: true, force: true });
    }
  }
  await cp(join(release, "locales"), join(bin, "locales"), {
    recursive: true,
    dereference: true,
    preserveTimestamps: true,
  }).catch(() => {});
  const crashpad = join(release, "chrome_crashpad_handler");
  if (await Bun.file(crashpad).exists()) await installExe(crashpad, join(bin, "chrome_crashpad_handler"));

  for (const file of ["icudtl.dat", "resources.pak", "v8_context_snapshot.bin"]) {
    const size = await stat(join(bin, file)).then((info) => info.size).catch(() => 0);
    if (size === 0) throw new Error(`${file} is empty after copy — CEF staging in ${release} is corrupt`);
  }

  for (const dir of ["models", "fonts", "icons"]) {
    await cp(join(engineDir, "assets", dir), join(data, "assets", dir), { recursive: true, dereference: true });
  }
  await cp(join(engineDir, "target/release/shaders"), join(data, "assets/shaders"), { recursive: true, dereference: true });
  await cp(join(editorDir, "dist"), join(data, "ui"), { recursive: true, dereference: true });

  await installExe(join(assetsDir, "linux/AppRun"), join(APPDIR, "AppRun"));
  for (const dest of [join(APPDIR, "saffron-anima.desktop"), join(applications, "saffron-anima.desktop")]) {
    await cp(join(assetsDir, "linux/saffron-anima.desktop"), dest);
  }
  for (const dest of [join(APPDIR, "saffron-anima.svg"), join(icons, "saffron-anima.svg")]) {
    await cp(join(assetsDir, "linux/saffron-anima.svg"), dest);
  }
  await rm(join(APPDIR, ".DirIcon"), { force: true });
  await symlink("saffron-anima.svg", join(APPDIR, ".DirIcon"));
}

async function installExe(src: string, dest: string): Promise<void> {
  await cp(src, dest, { dereference: true, force: true });
  await chmod(dest, 0o755);
}

// The host links the toolbox's LLVM C++ runtime (Jolt via cxx) and a stock system ships libstdc++,
// not libc++, so libc++/libc++abi ride along beside the host binary (AppRun's LD_LIBRARY_PATH
// covers usr/bin).
async function bundleCxxRuntime(hostBin: string, destDir: string): Promise<void> {
  const { stdout } = await $`ldd ${hostBin}`.nothrow().quiet();
  for (const line of stdout.toString().split("\n")) {
    const match = line.match(/\b(libc\+\+(?:abi)?\.so\S*)\s*=>\s*(\/\S+)/);
    if (match) {
      await cp(match[2], join(destDir, match[1]), { dereference: true, preserveTimestamps: true, force: true });
    }
  }
}

async function packImage(): Promise<void> {
  await mkdir(toolsDir, { recursive: true });
  const onPath = Bun.which("appimagetool");
  // --appimage-extract-and-run needs no FUSE, so the downloaded tool runs inside the toolbox.
  const tool = onPath ? [onPath] : [await ensureAppimagetool(), "--appimage-extract-and-run"];
  // A running AppImage keeps the old file mmap'd, so overwriting it in place hits ETXTBSY; pack to a
  // temp file and rename, which swaps the directory entry without touching the live inode.
  const tmp = `${OUT}.new`;
  await rm(tmp, { force: true });
  await $`${tool} ${APPDIR} ${tmp}`.env({ ...process.env, ARCH: "x86_64" }).quiet();
  await rename(tmp, OUT);
}

async function ensureAppimagetool(): Promise<string> {
  const bin = join(toolsDir, "appimagetool-x86_64.AppImage");
  if (!(await Bun.file(bin).exists())) {
    await $`curl -fL -o ${bin} ${APPIMAGETOOL_URL}`;
    await $`chmod +x ${bin}`;
  }
  return bin;
}
