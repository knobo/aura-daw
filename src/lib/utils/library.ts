/**
 * Pure helpers behind the library panel. Chrome only: the ONE path operation
 * the thin-renderer rule allows here is trimming a trailing segment for the
 * "up one folder" button — no filesystem access ever happens frontend-side
 * (ADR 0006; scanning is `library_scan`).
 */

/**
 * The parent of an absolute path, or null when it is already a root.
 * Handles both separators because the backend hands back whatever the host
 * OS uses.
 */
export function parentDir(path: string): string | null {
  const trimmed = path.replace(/[/\\]+$/, "");
  if (!trimmed) return null;
  const cut = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  if (cut < 0) return null;
  if (cut === 0) return "/"; // "/drums" -> "/"
  const parent = trimmed.slice(0, cut);
  // "C:" -> a Windows drive root, not a relative path.
  return /^[A-Za-z]:$/.test(parent) ? `${parent}\\` : parent;
}
