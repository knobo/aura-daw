/**
 * Fetch the user's theme files, parse them, install them.
 *
 * The reporting is the point: a theme file that does nothing is a thing the
 * user can fix, and silence would leave them guessing. One toast per file,
 * naming the file and the reason — whether the file was rejected outright
 * or merely had a key dropped.
 */

import { backend } from "../tauri";
import { toasts } from "../state/toasts.svelte";
import { parseUserTheme } from "./parse";
import { theme } from "./theme.svelte";
import type { Theme } from "./builtins/index";

export async function loadUserThemes(): Promise<void> {
  let files;
  try {
    files = await backend.listUserThemes();
  } catch {
    // No themes folder, no command, browser demo — the built-ins stand alone.
    return;
  }

  const loaded: Theme[] = [];
  for (const file of files) {
    const parsed = parseUserTheme(file.id, file.raw);
    if (!parsed.ok) {
      toasts.error("THEME FILE IGNORED", `${file.id}.json — ${parsed.reason}`);
      continue;
    }
    loaded.push(parsed.theme);
    if (parsed.dropped.length > 0) {
      toasts.info(
        "THEME PARTIALLY LOADED",
        `${file.id}.json — ignored: ${parsed.dropped.join(", ")}`,
      );
    }
  }
  theme.setUserThemes(loaded);
}
