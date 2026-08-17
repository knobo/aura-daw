/**
 * The preference registry: every user preference is declared HERE, once, as
 * data — kind, default, constraints, category, and the copy the preferences
 * dialog shows. Adding a preference is one entry in PrefValues + PREF_SCHEMA;
 * the store picks up persistence and the dialog renders the control from the
 * declaration. Nothing else needs to change.
 *
 * Pure TypeScript on purpose (no runes, no store imports): the schema is
 * importable from anywhere — including node-side tests — without dragging in
 * reactive state, and can never participate in an import cycle.
 */

import type { McpPolicyMode } from "../types/ipc";

export type PrefCategoryId = "editing" | "interface" | "library" | "ai";

export const PREF_CATEGORIES: readonly { id: PrefCategoryId; label: string }[] = [
  { id: "editing", label: "EDITING" },
  { id: "interface", label: "INTERFACE" },
  { id: "library", label: "LIBRARY" },
  { id: "ai", label: "AI AGENTS" },
];

/** The typed shape of every preference value. The single source of truth. */
export interface PrefValues {
  /** Start playback automatically when a MIDI clip opens in the piano roll. */
  clipOpenAutoplay: boolean;
  /** Flash notes (piano roll, keys, clip bodies) as the playhead hits them. */
  noteFlash: boolean;
  /** Interface zoom factor (CSS `zoom` on the shell). */
  uiZoom: number;
  /** How many recent projects the project menu lists. Stored history is
   * capped separately at 20; this only changes how many rows are shown. */
  recentProjectsMax: number;
  /** MCP policy mode pushed to the agent server at startup. */
  mcpDefaultMode: McpPolicyMode;
  /** Absolute folders listed under the library panel's SAMPLES root. The
   * default library dir is supplied by the backend and is not stored here. */
  librarySampleFolders: string[];
  /** Hear a click on every beat while playing or recording. */
  metronome: boolean;
  /** Linear click gain 0..1. */
  metronomeGain: number;
  /** Bars of click before a take arms. "0" = record immediately. */
  countInBars: "0" | "1" | "2" | "4";
  /** How close counts as hitting a note. Cents per option: TOLERANCE_CENTS. */
  pitchTolerance: PitchTolerance;
  /** Shifts detected pitch against the timeline, ±50 ms, to cancel the
   * interface's round-trip delay. Signed: the correction goes both ways. */
  pitchLatencyOffsetMs: number;
  /** Whether the pitch lane pins the playhead or scrolls with the timeline. */
  pitchLaneFollow: "fixed" | "free";
  /** Game mode: snap the trail onto the target when the note is hit. */
  pitchTrailSnap: boolean;
  /** Key held to rehearse (record silence into the take). "none" disables. */
  pitchRehearseKey: "h" | "j" | "none";
  /** Active theme id. A plain string, not a union: user themes are
   * discovered at runtime, so the catalogue is not knowable here. */
  theme: string;
}

/** Tolerance option ids, in order of strictness. */
export type PitchTolerance = "forgiving" | "standard" | "strict" | "pro";

/**
 * Cents per tolerance option. ±50 is the standard research criterion for
 * "the right pitch class" — half a semitone, past which a listener hears a
 * different note. The rest bracket it for people who want an easier or a
 * harder target.
 */
export const TOLERANCE_CENTS: Record<PitchTolerance, number> = {
  forgiving: 100,
  standard: 50,
  strict: 33,
  pro: 20,
};

export type PrefId = keyof PrefValues;

export interface PrefOption<T extends string> {
  value: T;
  label: string;
}

interface BaseDef {
  category: PrefCategoryId;
  label: string;
  /** One-line explanation shown under the control in the dialog. */
  blurb: string;
}

export type BooleanDef = BaseDef & { kind: "boolean"; default: boolean };
export type NumberDef = BaseDef & {
  kind: "number";
  default: number;
  min: number;
  max: number;
  step: number;
  /** Render hint: "%" makes the dialog show value·100; "ms" is a suffix. */
  unit?: "%" | "ms";
};
export type EnumDef<T extends string> = BaseDef & {
  kind: "enum";
  default: T;
  options: readonly PrefOption<T>[];
};

/**
 * An enum whose options come from a named RUNTIME catalog rather than a
 * static list — the theme picker, whose entries include whatever JSON files
 * the user has dropped in their config folder. The schema still holds only
 * data (the catalog's name); the dialog resolves it. That is what keeps this
 * module free of reactive imports and import cycles.
 */
export type ChoiceDef = BaseDef & {
  kind: "choice";
  default: string;
  catalog: "themes";
};

/** An ordered list of absolute filesystem paths (the library's sample
 * folders). Rendered by the dialog as a removable list plus an "add" button
 * driven by the native directory picker. */
export type PathListDef = BaseDef & {
  kind: "pathList";
  default: string[];
  /** Copy on the dialog's add button, e.g. "ADD FOLDER". */
  addLabel: string;
};

/** Maps a value type to the def shape that can declare it. The [V] tuple
 * wrapper stops the conditional from distributing over union values (an
 * enum like McpPolicyMode must yield ONE EnumDef over the whole union).
 * string[] is matched BEFORE the bare-string (enum) arm so the intent is
 * readable at a glance. */
type DefFor<V> = [V] extends [boolean]
  ? BooleanDef
  : [V] extends [number]
    ? NumberDef
    : [V] extends [string[]]
      ? PathListDef
      : // `string extends V` is true only for the WIDEST string — a literal
        // union like "0"|"1" fails it and falls through to EnumDef.
        [string] extends [V]
        ? ChoiceDef
        : [V] extends [string]
          ? EnumDef<V>
          : never;

export type PrefDef = { [K in PrefId]: DefFor<PrefValues[K]> }[PrefId];

export const PREF_SCHEMA: { readonly [K in PrefId]: DefFor<PrefValues[K]> } = {
  clipOpenAutoplay: {
    kind: "boolean",
    default: false,
    category: "editing",
    label: "Play clip when opened",
    blurb: "Start looping playback the moment a MIDI clip opens in the piano roll.",
  },
  noteFlash: {
    kind: "boolean",
    default: true,
    category: "interface",
    label: "Flash notes on playback",
    blurb: "Light up notes in the piano roll, keys and clips as the playhead hits them.",
  },
  uiZoom: {
    kind: "number",
    default: 1,
    min: 0.8,
    max: 2.0,
    step: 0.1,
    unit: "%",
    category: "interface",
    label: "Interface zoom",
    blurb: "Scale the whole interface. Also Ctrl/Cmd + and −.",
  },
  recentProjectsMax: {
    kind: "number",
    default: 8,
    min: 1,
    max: 20,
    step: 1,
    category: "interface",
    label: "Recent projects",
    blurb: "How many recently opened projects the project menu lists.",
  },
  librarySampleFolders: {
    kind: "pathList",
    default: [],
    addLabel: "ADD FOLDER",
    category: "library",
    label: "Sample folders",
    blurb: "Folders browsed under SAMPLES in the library panel. The default AURA library folder is always listed first.",
  },
  metronome: {
    kind: "boolean",
    default: false,
    category: "editing",
    label: "Metronome",
    blurb: "Click on every beat while the transport is rolling. Also the CLICK chip in the transport bar.",
  },
  metronomeGain: {
    kind: "number",
    default: 0.35,
    min: 0,
    max: 1,
    step: 0.05,
    category: "editing",
    label: "Metronome volume",
    blurb: "How loud the click is, relative to the mix.",
  },
  countInBars: {
    kind: "enum",
    default: "0",
    category: "editing",
    label: "Count-in",
    blurb: "Bars of click before a take starts recording. The playhead stays put.",
    options: [
      { value: "0", label: "OFF" },
      { value: "1", label: "1 BAR" },
      { value: "2", label: "2 BARS" },
      { value: "4", label: "4 BARS" },
    ],
  },
  pitchTolerance: {
    kind: "enum",
    default: "standard",
    category: "editing",
    label: "Pitch tolerance",
    blurb:
      "How close you have to be for a note to count as hit. ±50 cents is the standard research criterion for the right pitch class.",
    options: [
      { value: "forgiving", label: "FORGIVING ±100" },
      { value: "standard", label: "STANDARD ±50" },
      { value: "strict", label: "STRICT ±33" },
      { value: "pro", label: "PRO ±20" },
    ],
  },
  pitchLatencyOffsetMs: {
    kind: "number",
    default: 0,
    min: -50,
    max: 50,
    step: 1,
    unit: "ms",
    category: "editing",
    label: "Pitch latency offset",
    blurb: "Shifts detected pitch against the timeline, to compensate for your interface's round-trip delay.",
  },
  pitchLaneFollow: {
    kind: "enum",
    default: "fixed",
    category: "interface",
    label: "Pitch lane scrolling",
    blurb:
      "FIXED pins the playhead and scrolls the lane toward you while you sing. FREE scrolls like the rest of the timeline.",
    options: [
      { value: "fixed", label: "FIXED" },
      { value: "free", label: "FREE" },
    ],
  },
  pitchTrailSnap: {
    kind: "boolean",
    default: false,
    category: "interface",
    label: "Snap pitch trail",
    blurb:
      "Game mode: snaps your line onto the target note when you hit it. Off shows your true pitch, which is more useful for practice.",
  },
  pitchRehearseKey: {
    kind: "enum",
    default: "h",
    category: "editing",
    label: "Rehearse hold key",
    blurb: "Hold this key while recording to keep the transport rolling without writing anything to the take.",
    options: [
      { value: "h", label: "H" },
      { value: "j", label: "J" },
      { value: "none", label: "OFF" },
    ],
  },
  mcpDefaultMode: {
    kind: "enum",
    default: "confirmDestructive",
    category: "ai",
    label: "Agent access at startup",
    blurb: "MCP policy applied when the app starts. Changing it here applies immediately too.",
    options: [
      { value: "readOnly", label: "READ ONLY" },
      { value: "confirmDestructive", label: "CONFIRM" },
      { value: "full", label: "FULL" },
    ],
  },
  theme: {
    kind: "choice",
    default: "aura-dark",
    catalog: "themes",
    category: "interface",
    label: "Theme",
    blurb:
      "Colours and contrast for the whole interface. Custom themes are JSON files in the folder below; add or edit one and restart AURA to see it here.",
  },
};

/**
 * Validate an untrusted value (disk state, caller input) against a def.
 * Returns a valid value of the def's type, or undefined — never throws.
 * Numbers are clamped to [min, max] and snapped to the step grid so stored
 * junk like 97 degrades to the nearest legal value instead of being lost.
 */
export function coercePref<D extends PrefDef>(def: D, value: unknown): D["default"] | undefined {
  switch (def.kind) {
    case "boolean":
      return typeof value === "boolean" ? value : undefined;
    case "choice":
      // Existence is checked where the catalog lives (the theme store), which
      // falls back to the default for an id it does not know.
      return typeof value === "string" && value.trim() !== "" ? (value as D["default"]) : undefined;
    case "enum":
      return def.options.some((o) => o.value === value) ? (value as D["default"]) : undefined;
    case "number": {
      if (typeof value !== "number" || !Number.isFinite(value)) return undefined;
      const clamped = Math.min(def.max, Math.max(def.min, value));
      // Snap in scaled-integer space: the float grid (min + n·step) drifts
      // enough to flip ties (1.25 → 1.2 instead of 1.3).
      const scale = 10 ** (String(def.step).split(".")[1] ?? "").length;
      const minInt = Math.round(def.min * scale);
      const stepInt = Math.round(def.step * scale);
      return (minInt + Math.round((clamped * scale - minInt) / stepInt) * stepInt) / scale;
    }
    case "pathList": {
      // Reject a non-array whole (the schema's own convention); inside an
      // array, drop junk entries and duplicates rather than losing the list.
      if (!Array.isArray(value)) return undefined;
      const paths = value.filter((p): p is string => typeof p === "string" && p.length > 0);
      return [...new Set(paths)] as D["default"];
    }
  }
}

/** A fresh { id → default } object covering the whole schema. Array defaults
 * are COPIED: the store's value must never alias the schema's own default. */
export function schemaDefaults(): PrefValues {
  return Object.fromEntries(
    (Object.keys(PREF_SCHEMA) as PrefId[]).map((id) => {
      const d = PREF_SCHEMA[id].default;
      return [id, Array.isArray(d) ? [...d] : d];
    }),
  ) as unknown as PrefValues;
}
