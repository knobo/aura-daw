/**
 * The Composer's five-tier colour ramp, as theme tokens.
 *
 * The roles come from the backend (`NoteRole`); this file only says which
 * EXISTING token each one borrows and how strongly to paint it. No literals:
 * the ramp has to work in all eight built-in themes and in a user theme
 * nobody has written yet, so it is expressed as "the accent that already means
 * this" rather than as colours of its own.
 *
 * Pure TypeScript (no runes, no DOM) so the canvas painters and a node test can
 * both use it — the same reason `theme/tokens.ts` is written that way.
 */

import type { ThemeTokens } from "../theme/tokens";
import type { NoteRole } from "../types/ipc";

export interface RoleInk {
  /** A theme token value. */
  color: string;
  /** How strongly to paint a row of the grid with it (0 = not at all). */
  rowAlpha: number;
  /** How strongly to paint the key in the gutter. */
  keyAlpha: number;
}

/**
 * Chord tones are the app's own accent (they are the answer); colour goes to
 * the green/violet side, tension to amber, and the one note that fights the
 * chord to red — the same red the app uses for "no" everywhere else.
 */
export function roleInk(role: NoteRole, t: ThemeTokens): RoleInk {
  switch (role) {
    case "root":
      return { color: t.cyan, rowAlpha: 0.1, keyAlpha: 0.45 };
    case "third":
      return { color: t.green, rowAlpha: 0.085, keyAlpha: 0.4 };
    case "fifth":
      return { color: t.cyan, rowAlpha: 0.055, keyAlpha: 0.25 };
    case "seventh":
      return { color: t.violet, rowAlpha: 0.075, keyAlpha: 0.35 };
    case "extension":
      return { color: t.green, rowAlpha: 0.035, keyAlpha: 0.16 };
    case "scale":
      return { color: t.textDim, rowAlpha: 0.03, keyAlpha: 0.12 };
    case "tension":
      return { color: t.amber, rowAlpha: 0.035, keyAlpha: 0.18 };
    case "avoid":
      return { color: t.red, rowAlpha: 0.05, keyAlpha: 0.22 };
    default:
      return { color: t.textFaint, rowAlpha: 0, keyAlpha: 0 };
  }
}

/** One-word gloss for a role, for a tooltip or a status line. */
export function roleGloss(role: NoteRole): string {
  switch (role) {
    case "root":
      return "root";
    case "third":
      return "third — decides major or minor";
    case "fifth":
      return "fifth";
    case "seventh":
      return "seventh — the chord's character";
    case "extension":
      return "available colour";
    case "scale":
      return "in the key";
    case "tension":
      return "chromatic — needs to resolve";
    case "avoid":
      return "avoid note — it fights the chord";
    default:
      return "outside the key";
  }
}
