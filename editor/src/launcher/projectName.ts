/// The on-disk project name rule, mirroring the engine's `valid_project_name`
/// (`engine/crates/assets/src/project.rs`): 1–63 chars of lowercase letters, digits, and
/// hyphens, starting and ending with a letter or digit.
export function validProjectName(name: string): boolean {
  if (name.length < 1 || name.length > 63) {
    return false;
  }
  return /^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/.test(name);
}

/// Derive the on-disk slug from a display name: lowercase, whitespace/underscores to hyphens,
/// everything else outside `[a-z0-9-]` dropped, hyphen runs collapsed, edges trimmed, clamped to
/// the 63-char limit. Every non-empty result passes [`validProjectName`]; an empty result means
/// nothing usable survived and creation stays disabled.
export function deriveProjectSlug(displayName: string): string {
  return displayName
    .toLowerCase()
    .replace(/[\s_]+/g, "-")
    .replace(/[^a-z0-9-]/g, "")
    .replace(/-{2,}/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 63)
    .replace(/-+$/, "");
}
