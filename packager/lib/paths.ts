import { join } from "node:path";

// This file is packager/lib/paths.ts, so the repo root is two levels up and the packaged assets
// (AppRun, .desktop, icon) live at packager/assets.
export const packagerDir = join(import.meta.dir, "..");
export const repo = join(packagerDir, "..");
export const assetsDir = join(packagerDir, "assets");
export const engineDir = join(repo, "engine");
export const editorDir = join(repo, "editor");
export const shellDir = join(editorDir, "shell");
export const buildDir = join(repo, "build");
export const distDir = join(buildDir, "dist");
export const toolsDir = join(buildDir, "tools");
