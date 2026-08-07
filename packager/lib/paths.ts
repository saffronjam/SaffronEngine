import { join } from "node:path";

const packagerDir = join(import.meta.dir, "..");
export const repo = join(packagerDir, "..");
export const assetsDir = join(packagerDir, "assets");
export const engineDir = join(repo, "engine");
export const editorDir = join(repo, "editor");
export const shellDir = join(editorDir, "shell");
const buildDir = join(repo, "build");
export const distDir = join(buildDir, "dist");
export const toolsDir = join(buildDir, "tools");
