# Unified Browser Audition (Step 7) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Double-click any browser row to hear it, on one shared gesture every
browser inherits, gated behind a new `browserAudition` preference that
defaults off.

**Architecture:** One `AuditionTarget` tagged union describes "what would this
row sound like"; a pure resolver in `src/lib/utils/audition-target.ts` turns a
row's data into one, and a singleton store in
`src/lib/state/audition.svelte.ts` plays it through whichever backend
primitive already exists (`sampler_preview_note`, `plugin_preview_note`,
`library_audition`). `BrowserRow` gains one `ondblclick` prop and every
browser passes a target-producing handler; nothing about single-click
changes. No new IPC, no Rust, no ops.

**Tech Stack:** Svelte 5 runes, TypeScript, Vitest (`unit` node project +
`dom` jsdom project), Tauri IPC via `src/lib/tauri.ts`.

**Spec:** [`docs/superpowers/specs/2026-08-20-plugin-manager-design.md`](../specs/2026-08-20-plugin-manager-design.md) §8.2
(the audition ask) and §8.3 (why it is its own Step 7). Track backlog:
[`docs/backlog/plugin-manager.md`](../../backlog/plugin-manager.md).

## Global Constraints

Copy these into your head before Task 1; every task inherits them.

- **Thin renderer (ADR 0006).** No new authoritative state, no business
  logic, no time math frontend-side. This whole plan is UI + gesture + an
  existing-command call, which is exactly what the rule allows.
- **An audition is never a project edit.** No op, no dirty flag, no
  `plugin_instantiate`, no `set_track_instrument`. If a row cannot be heard
  without an edit, it is not auditionable — say so, do not edit.
- **Frozen command names stay frozen.** This plan calls only
  `sampler_preview_note`, `plugin_preview_note`, `plugin_preview_note_off`,
  `library_audition`, `library_audition_stop`, all already on
  `src/lib/tauri.ts`. **No new IPC command is added anywhere in this plan.**
- **No `OP_FORMAT_VERSION` bump**, no `schemaVersion` move.
- **Theme tokens only** in every `<style>` block. Raw colour literals fail
  `src/lib/theme/no-literals.test.ts` in CI. Use an existing token or a
  `theme-exempt:` comment.
- **Do not re-implement the focus ring or `prefers-reduced-motion`** — both
  are already global in `src/app.css`.
- **`display: contents` is banned** (WebKitGTK). Use a real flex container.
- **Before writing any `*.dom.test.ts`**, read
  [`2026-08-18-dom-test-environment.md`](2026-08-18-dom-test-environment.md).
  jsdom has no Pointer Capture, `getBoundingClientRect()` returns zeros,
  `scrollIntoView` is missing, and Svelte 5 `$state` proxies throw on
  `structuredClone`.
- **`CSS` is not a global in the `unit` (node) project.** Stub `CSS.escape`
  explicitly in any node test that touches a `[data-*]` selector.
- **Foreground, timeout-guarded test runs only:**
  `timeout 300 npx vitest run`. No backgrounding.
- **The dated-count convention:** the task that changes the frontend test
  count updates README.md and CONTRIBUTING.md in the same commit, with the
  date. Measure the number, never copy one from a briefing.
- **Reply to the owner in Norwegian**; all repo documentation stays English.

## Rulings this plan makes (and why)

Two things §8.2 leaves open. Both are settled here so no task has to guess.

**R-1: the pref gates the new double-click gesture only. Existing
single-click and hold-to-play audition behaviour is untouched.** §8.2 says
in one breath "single click keeps its current meaning everywhere, so nothing
existing breaks" and "audition is off until asked for". Those collide for
`SamplesRoot` (single click already auditions today) and `InstrumentBrowser`
(hold-to-play already sounds today). Gating those behind a default-off pref
would silently delete two shipped features, which is a bigger break than the
noise the pref exists to prevent. So: the pref gates the *new* gesture.
Whether the owner also wants the old single-click audition behind the pref is
an ear-check question, recorded in the backlog — not a guess this plan makes.

**R-2: the chip in the toolbar *is* the preference, not a second session-level
mute.** §8.2 asks for "a mute toggle in the shell's chip row too". Two
independent switches that can disagree ("the pref says on, the chip says
muted") is a state-machine the user has to hold in their head. The chip
writes `prefs.values.browserAudition` directly, so there is exactly one truth
and the Preferences dialog and the chip always agree.

**R-3: a catalog plugin row that has no live, track-bound instance is not
auditionable in this step.** `plugin_preview_note` requires an instance that
is already bound to a MIDI track (`plugins/mod.rs`'s
`midi_track_bound_to_plugin`); reaching a bare catalog descriptor means
`plugin_instantiate`, which commits `Op::PluginAdd` — a project edit with an
undo entry, forbidden by the constraint above. The document-free preview slot
§8.2 describes is real backend work in the audio graph and is filed as its own
backlog job by Task 7. Owner decision, 2026-08-23. In this step such a row
reports "no live instance to audition" and stays silent.

---

## File structure

| File | Status | Responsibility |
|---|---|---|
| `src/lib/prefs/schema.ts` | modify | Declare the `browserAudition` preference |
| `src/lib/utils/audition-target.ts` | create | The `AuditionTarget` union + the pure resolvers. No I/O, no runes |
| `src/lib/utils/audition-target.test.ts` | create | Pins the resolvers (node project) |
| `src/lib/state/audition.svelte.ts` | create | The singleton: is it enabled, what is sounding, play/stop |
| `src/lib/state/audition.test.ts` | create | Pins the store against a mocked backend (node project) |
| `src/lib/components/browser/AuditionChip.svelte` | create | The toolbar toggle, shared by `BrowserShell` and `PluginManager` |
| `src/lib/components/browser/BrowserRow.svelte` | modify | One new `ondblclick` prop |
| `src/lib/components/browser/BrowserShell.svelte` | modify | Renders the chip; Shift+Enter keyboard parity |
| `src/lib/components/browser/browser-audition.dom.test.ts` | create | Mounted: the gesture fires, the gate holds |
| `src/lib/components/library/SamplesRoot.svelte` | modify | Wire audio rows |
| `src/lib/components/library/PresetsRoot.svelte` | modify | Wire instrument + patch rows |
| `src/lib/components/instruments/InstrumentBrowser.svelte` | modify | Wire instrument rows |
| `src/lib/components/plugins/PluginManager.svelte` | modify | Wire rack + browse rows, add the chip to its own toolbar |
| `src/lib/components/plugins/ZynPatchBrowser.svelte` | modify | Route its hand-rolled rows through the store |
| `README.md`, `CONTRIBUTING.md` | modify | Dated frontend test count |
| `docs/backlog/plugin-manager.md` | modify | Step 7 shipped; file the preview-slot follow-up |
| `docs/LANDED.md` | modify | Record the outcome |
| `next-prompt.md` | modify | Release the claim |

---

### Task 1: The `browserAudition` preference

**Files:**
- Modify: `src/lib/prefs/schema.ts`
- Test: `src/lib/prefs/schema.test.ts`

**Interfaces:**
- Consumes: nothing.
- Produces: `PrefValues["browserAudition"]: boolean`, default `false`,
  category `"library"`. Every later task reads it as
  `prefs.values.browserAudition`.

- [ ] **Step 1: Write the failing test**

Append to `src/lib/prefs/schema.test.ts`, inside the existing
`describe("PREF_SCHEMA integrity", ...)` block:

```ts
  it("browserAudition is a boolean that defaults off", () => {
    const def = PREF_SCHEMA.browserAudition;
    expect(def.kind).toBe("boolean");
    // Default OFF is the whole point: a browser that makes noise you did
    // not ask for is the fastest way to lose someone (design §8.2).
    expect(def.default).toBe(false);
    expect(def.category).toBe("library");
  });
```

- [ ] **Step 2: Run it and watch it fail**

```
timeout 300 npx vitest run src/lib/prefs/schema.test.ts
```

Expected: FAIL — TypeScript/runtime error, `PREF_SCHEMA.browserAudition` is
`undefined`.

- [ ] **Step 3: Declare the preference**

In `src/lib/prefs/schema.ts`, add to the `PrefValues` interface, immediately
after the `clipOpenAutoplay` field (the precedent this follows — same shape,
same default):

```ts
  /** Double-click a browser row to hear it. Off until asked for. */
  browserAudition: boolean;
```

And add to the `PREF_SCHEMA` object, keeping the same key order as
`PrefValues`:

```ts
  browserAudition: {
    kind: "boolean",
    default: false,
    category: "library",
    label: "Audition on double-click",
    blurb:
      "Double-click a row in any browser to hear it — a sample plays, an instrument or plugin sounds C3. Never edits the project.",
  },
```

- [ ] **Step 4: Run the whole prefs suite**

```
timeout 300 npx vitest run src/lib/prefs
```

Expected: PASS, including the pre-existing "every default survives its own
coercion" test that iterates every id.

- [ ] **Step 5: Commit**

```bash
git add src/lib/prefs/schema.ts src/lib/prefs/schema.test.ts
git commit -m "feat(prefs): browserAudition, default off"
```

---

### Task 2: The pure target resolvers

**Files:**
- Create: `src/lib/utils/audition-target.ts`
- Test: `src/lib/utils/audition-target.test.ts`

**Interfaces:**
- Consumes: `PluginInstanceInfo` and `TrackState` from `../types/ipc` (check
  the exact exported names there before writing the import — do not invent
  them).
- Produces, and every later task uses exactly these names:
  - `AUDITION_KEY = 60` (C3, the pitch §8.2 asks for)
  - `type AuditionTarget` — the tagged union below
  - `resolvePluginInstanceTarget(instanceId, tracks): AuditionTarget`
  - `resolveDescriptorTarget(uid, instances, tracks): AuditionTarget`

- [ ] **Step 1: Write the failing test**

Create `src/lib/utils/audition-target.test.ts`:

```ts
/**
 * Audition targets: the pure half of "what would this row sound like".
 * The store (state/audition.svelte.ts) does the I/O; everything decidable
 * from data alone is decided here, so it can be tested without a backend.
 */
import { describe, expect, it } from "vitest";
import {
  AUDITION_KEY,
  resolveDescriptorTarget,
  resolvePluginInstanceTarget,
} from "./audition-target";
import type { PluginInstanceInfo, TrackState } from "../types/ipc";

const track = (id: string, instrumentId: string | null): TrackState =>
  ({
    id,
    name: id,
    kind: "midi",
    gainDb: 0,
    pan: 0,
    muted: false,
    soloed: false,
    armed: false,
    automationMode: "read",
    color: "#888888",
    instrumentId,
  }) as unknown as TrackState;

const inst = (id: string, uid: string, status = "active"): PluginInstanceInfo =>
  ({ id, uid, name: uid, format: "lv2", status }) as unknown as PluginInstanceInfo;

describe("resolvePluginInstanceTarget", () => {
  it("targets C3 on an instance bound to a midi track", () => {
    const t = [track("t1", "plugin:p1")];
    expect(resolvePluginInstanceTarget("p1", t)).toEqual({
      kind: "pluginInstance",
      instanceId: "p1",
      key: AUDITION_KEY,
    });
  });

  it("refuses an instance that is on no midi track", () => {
    // plugin_preview_note routes through the instance's bound midi track;
    // an insert-only or unplaced instance has no note input at all.
    const t = [track("t1", null)];
    const got = resolvePluginInstanceTarget("p1", t);
    expect(got.kind).toBe("silent");
    if (got.kind === "silent") expect(got.reason).toMatch(/not on a midi track/i);
  });
});

describe("resolveDescriptorTarget", () => {
  it("borrows a live, track-bound instance of the same plugin", () => {
    const instances = [inst("p1", "urn:zyn")];
    const tracks = [track("t1", "plugin:p1")];
    expect(resolveDescriptorTarget("urn:zyn", instances, tracks)).toEqual({
      kind: "pluginInstance",
      instanceId: "p1",
      key: AUDITION_KEY,
    });
  });

  it("ignores an instance of the same plugin that is not track-bound", () => {
    const instances = [inst("p1", "urn:zyn")];
    const tracks = [track("t1", null)];
    expect(resolveDescriptorTarget("urn:zyn", instances, tracks).kind).toBe("silent");
  });

  it("ignores a non-active instance", () => {
    const instances = [inst("p1", "urn:zyn", "failed")];
    const tracks = [track("t1", "plugin:p1")];
    expect(resolveDescriptorTarget("urn:zyn", instances, tracks).kind).toBe("silent");
  });

  it("refuses a catalog plugin with nothing live, and never proposes an edit", () => {
    // R-3: instantiating to preview would commit Op::PluginAdd. Silence is
    // the correct answer until the backend preview slot exists.
    const got = resolveDescriptorTarget("urn:nothing", [], []);
    expect(got.kind).toBe("silent");
    if (got.kind === "silent") expect(got.reason).toMatch(/no live instance/i);
  });

  it("prefers the first active bound instance when several match", () => {
    const instances = [inst("p1", "urn:zyn", "failed"), inst("p2", "urn:zyn")];
    const tracks = [track("t1", "plugin:p1"), track("t2", "plugin:p2")];
    const got = resolveDescriptorTarget("urn:zyn", instances, tracks);
    expect(got).toEqual({ kind: "pluginInstance", instanceId: "p2", key: AUDITION_KEY });
  });
});
```

- [ ] **Step 2: Run it and watch it fail**

```
timeout 300 npx vitest run src/lib/utils/audition-target.test.ts
```

Expected: FAIL — cannot resolve `./audition-target`.

- [ ] **Step 3: Write the module**

Create `src/lib/utils/audition-target.ts`:

```ts
/**
 * What would this browser row sound like?
 *
 * One tagged union, resolved from data alone — no I/O, no runes, no
 * backend. `state/audition.svelte.ts` plays what this returns.
 *
 * The rule these resolvers exist to enforce: **an audition is never a
 * project edit.** A row that can only be heard by instantiating a plugin
 * (`Op::PluginAdd`) or binding a track resolves to `silent` with a reason,
 * never to a target that would commit an op. Design §8.2, ruling R-3.
 */

import type { PluginInstanceInfo, TrackState } from "../types/ipc";

/** C3 — the pitch every pitched audition uses (design §8.2). */
export const AUDITION_KEY = 60;

export type AuditionTarget =
  /** An audio file, played through the preview stream (`library_audition`). */
  | { kind: "sample"; path: string }
  /** A loaded sampler instrument (`sampler_preview_note`). */
  | { kind: "instrument"; instrumentId: string; key: number }
  /** A live plugin instance on a midi track (`plugin_preview_note`). */
  | { kind: "pluginInstance"; instanceId: string; key: number }
  /** Nothing to play, and a sentence saying why. Never an error. */
  | { kind: "silent"; reason: string };

/** The `instrumentId` a midi track carries when a plugin is its instrument. */
function pluginInstrumentId(instanceId: string): string {
  return `plugin:${instanceId}`;
}

function isBoundToMidiTrack(instanceId: string, tracks: readonly TrackState[]): boolean {
  const want = pluginInstrumentId(instanceId);
  return tracks.some((t) => t.kind === "midi" && t.instrumentId === want);
}

/**
 * A specific live instance. Mirrors the backend's own precondition
 * (`midi_track_bound_to_plugin` in `src-tauri/src/plugins/mod.rs`): an
 * insert-only or unplaced instance has no note input, so asking would just
 * earn an error string.
 */
export function resolvePluginInstanceTarget(
  instanceId: string,
  tracks: readonly TrackState[],
): AuditionTarget {
  if (!isBoundToMidiTrack(instanceId, tracks)) {
    return { kind: "silent", reason: "this instance is not on a midi track" };
  }
  return { kind: "pluginInstance", instanceId, key: AUDITION_KEY };
}

/**
 * A catalog row. Borrows an existing active instance of the same plugin if
 * one is already live and track-bound; otherwise silent, because the only
 * way to reach a bare descriptor is to instantiate it, and that is an edit.
 */
export function resolveDescriptorTarget(
  uid: string,
  instances: readonly PluginInstanceInfo[],
  tracks: readonly TrackState[],
): AuditionTarget {
  const live = instances.find(
    (i) => i.uid === uid && i.status === "active" && isBoundToMidiTrack(i.id, tracks),
  );
  if (!live) {
    return { kind: "silent", reason: "no live instance of this plugin to audition" };
  }
  return { kind: "pluginInstance", instanceId: live.id, key: AUDITION_KEY };
}
```

- [ ] **Step 4: Run the test**

```
timeout 300 npx vitest run src/lib/utils/audition-target.test.ts
```

Expected: PASS, 6 tests.

If `TrackState` or `PluginInstanceInfo` do not carry the fields used here
under those exact names, fix the *test fixture* to match `src/lib/types/ipc.ts`
— do not change the resolver's shape.

- [ ] **Step 5: Commit**

```bash
git add src/lib/utils/audition-target.ts src/lib/utils/audition-target.test.ts
git commit -m "feat(browser): pure audition target resolvers"
```

---

### Task 3: The audition store

**Files:**
- Create: `src/lib/state/audition.svelte.ts`
- Test: `src/lib/state/audition.test.ts`

**Interfaces:**
- Consumes: `AuditionTarget`, `AUDITION_KEY` from Task 2; `backend` from
  `../tauri`; `prefs` from `../prefs/prefs.svelte`.
- Produces, used verbatim by Tasks 4–6:
  - `audition.enabled: boolean` (get/set — the setter writes the pref)
  - `audition.sounding: string | null` — an opaque id for row highlighting:
    the sample path, the instrument id, or the instance id
  - `audition.lastSilentReason: string | null`
  - `audition.play(target: AuditionTarget): Promise<void>`
  - `audition.stop(): Promise<void>`
  - `AUDITION_DECAY_MS = 1200`

- [ ] **Step 1: Write the failing test**

Create `src/lib/state/audition.test.ts`:

```ts
/**
 * The audition store: one gesture, four row kinds, one gate.
 *
 * Every assertion here is about the two rules the store exists to hold:
 * the preference gates it, and no path it takes is ever a project edit.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

const invokes = {
  samplerPreviewNote: vi.fn(async (_id: string, _key: number, _vel: number) => {}),
  pluginPreviewNote: vi.fn(async (_id: string, _key: number, _vel: number) => {}),
  libraryAudition: vi.fn(async (_p: string, _s?: number | null) => {}),
  libraryAuditionStop: vi.fn(async () => {}),
};

vi.mock("../tauri", () => ({ backend: invokes }));

const { audition } = await import("./audition.svelte");
const { prefs } = await import("../prefs/prefs.svelte");

beforeEach(() => {
  for (const fn of Object.values(invokes)) fn.mockClear();
  audition.enabled = true;
  audition.sounding = null;
  audition.lastSilentReason = null;
});

describe("audition gate", () => {
  it("plays nothing at all while the preference is off", async () => {
    audition.enabled = false;
    await audition.play({ kind: "sample", path: "/lib/kick.wav" });
    expect(invokes.libraryAudition).not.toHaveBeenCalled();
    expect(audition.sounding).toBeNull();
  });

  it("the enabled setter writes the preference, so the chip and the dialog agree", () => {
    audition.enabled = false;
    expect(prefs.values.browserAudition).toBe(false);
    audition.enabled = true;
    expect(prefs.values.browserAudition).toBe(true);
  });
});

describe("audition play", () => {
  it("a sample goes through library_audition and marks its path sounding", async () => {
    await audition.play({ kind: "sample", path: "/lib/kick.wav" });
    expect(invokes.libraryAudition).toHaveBeenCalledWith("/lib/kick.wav");
    expect(audition.sounding).toBe("/lib/kick.wav");
  });

  it("an instrument goes through sampler_preview_note at the given key", async () => {
    await audition.play({ kind: "instrument", instrumentId: "i1", key: 60 });
    expect(invokes.samplerPreviewNote).toHaveBeenCalledWith("i1", 60, 100);
    expect(audition.sounding).toBe("i1");
  });

  it("a plugin instance goes through plugin_preview_note", async () => {
    await audition.play({ kind: "pluginInstance", instanceId: "p1", key: 60 });
    expect(invokes.pluginPreviewNote).toHaveBeenCalledWith("p1", 60, 100);
    expect(audition.sounding).toBe("p1");
  });

  it("a silent target records its reason and calls no backend at all", async () => {
    await audition.play({ kind: "silent", reason: "no live instance of this plugin to audition" });
    expect(audition.lastSilentReason).toMatch(/no live instance/);
    expect(invokes.libraryAudition).not.toHaveBeenCalled();
    expect(invokes.samplerPreviewNote).not.toHaveBeenCalled();
    expect(invokes.pluginPreviewNote).not.toHaveBeenCalled();
    expect(audition.sounding).toBeNull();
  });

  it("a new audition stops the previous sample before starting", async () => {
    await audition.play({ kind: "sample", path: "/lib/a.wav" });
    invokes.libraryAuditionStop.mockClear();
    await audition.play({ kind: "sample", path: "/lib/b.wav" });
    expect(invokes.libraryAuditionStop).toHaveBeenCalled();
    expect(audition.sounding).toBe("/lib/b.wav");
  });

  it("a backend failure clears sounding and surfaces the reason, never throws", async () => {
    invokes.libraryAudition.mockRejectedValueOnce(new Error("decode failed"));
    await expect(audition.play({ kind: "sample", path: "/lib/bad.wav" })).resolves.toBeUndefined();
    expect(audition.sounding).toBeNull();
    expect(audition.lastSilentReason).toMatch(/decode failed/);
  });
});

describe("audition decay", () => {
  it("clears the sounding highlight on a timer, and a newer audition wins the timer", async () => {
    vi.useFakeTimers();
    try {
      await audition.play({ kind: "instrument", instrumentId: "i1", key: 60 });
      expect(audition.sounding).toBe("i1");
      vi.advanceTimersByTime(1200);
      expect(audition.sounding).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("audition stop", () => {
  it("releases the sample stream and clears the highlight", async () => {
    await audition.play({ kind: "sample", path: "/lib/a.wav" });
    await audition.stop();
    expect(invokes.libraryAuditionStop).toHaveBeenCalled();
    expect(audition.sounding).toBeNull();
  });
});
```

- [ ] **Step 2: Run it and watch it fail**

```
timeout 300 npx vitest run src/lib/state/audition.test.ts
```

Expected: FAIL — cannot resolve `./audition.svelte`.

- [ ] **Step 3: Write the store**

Create `src/lib/state/audition.svelte.ts` (a `.svelte.ts` module — runes in
plain TypeScript, no component markup):

```ts
/**
 * Unified browser audition (design §8.2, Step 7).
 *
 * One gesture — double-click a row — reaches every browser, and one store
 * decides what that means. The store is deliberately thin: it owns the
 * gate, the "what is sounding right now" highlight, and the dispatch from
 * an `AuditionTarget` to whichever preview command already exists. It owns
 * no authoritative state (ADR 0006) and emits no ops.
 *
 * Two invariants, both tested:
 *
 * 1. **The preference gates everything.** `browserAudition` defaults off,
 *    because a browser that makes noise you did not ask for is the fastest
 *    way to lose someone. `enabled` reads and writes that pref directly
 *    rather than shadowing it in a second session-level mute — one truth,
 *    so the toolbar chip and the Preferences dialog can never disagree
 *    (ruling R-2).
 * 2. **An audition is never a project edit.** Every command below is a
 *    document-decoupled preview path: no op, no dirty flag, no undo entry.
 *    A target that could only be reached by editing resolves to `silent`
 *    upstream, in `utils/audition-target.ts`.
 */

import { backend } from "../tauri";
import { prefs } from "../prefs/prefs.svelte";
import type { AuditionTarget } from "../utils/audition-target";

/** How long a row stays lit after it was auditioned. Long enough to see
 * which row answered, short enough not to look like a selection. */
export const AUDITION_DECAY_MS = 1200;

const AUDITION_VELOCITY = 100;

class AuditionStore {
  /** Opaque id of what last sounded — sample path, instrument id, or
   * instance id. Rows compare their own id against it to light up. */
  sounding = $state<string | null>(null);
  /** Why the last attempt made no sound, when it made none. A UI hint, not
   * an error state: "no live instance of this plugin to audition" is a
   * perfectly ordinary answer. */
  lastSilentReason = $state<string | null>(null);

  /** True while a sample is on the preview stream, so `stop` knows whether
   * `library_audition_stop` has anything to release. */
  #sampleSounding = false;
  #decay: ReturnType<typeof setTimeout> | null = null;

  get enabled(): boolean {
    return prefs.values.browserAudition;
  }
  set enabled(on: boolean) {
    prefs.values.browserAudition = on;
  }

  async play(target: AuditionTarget): Promise<void> {
    if (!this.enabled) return;
    if (target.kind === "silent") {
      this.lastSilentReason = target.reason;
      return;
    }
    // Whatever was sounding gets cut first: two overlapping auditions tell
    // you nothing about either one.
    await this.stop();
    this.lastSilentReason = null;
    try {
      switch (target.kind) {
        case "sample":
          await backend.libraryAudition?.(target.path);
          this.#sampleSounding = true;
          this.#light(target.path);
          break;
        case "instrument":
          await backend.samplerPreviewNote(target.instrumentId, target.key, AUDITION_VELOCITY);
          this.#light(target.instrumentId);
          break;
        case "pluginInstance":
          await backend.pluginPreviewNote(target.instanceId, target.key, AUDITION_VELOCITY);
          this.#light(target.instanceId);
          break;
      }
    } catch (err) {
      // A preview that cannot sound is a hint, never a thrown error that
      // takes a click handler down with it.
      this.sounding = null;
      this.#sampleSounding = false;
      this.lastSilentReason = String(err);
    }
  }

  async stop(): Promise<void> {
    if (this.#decay) {
      clearTimeout(this.#decay);
      this.#decay = null;
    }
    this.sounding = null;
    if (!this.#sampleSounding) return;
    this.#sampleSounding = false;
    try {
      await backend.libraryAuditionStop?.();
    } catch {
      /* nothing was sounding — the preview stream is lazy */
    }
  }

  #light(id: string) {
    this.sounding = id;
    if (this.#decay) clearTimeout(this.#decay);
    this.#decay = setTimeout(() => {
      this.#decay = null;
      if (this.sounding === id) this.sounding = null;
    }, AUDITION_DECAY_MS);
  }
}

export const audition = new AuditionStore();
```

Note the `?.` on `libraryAudition` / `libraryAuditionStop`: they are optional
on the backend interface (desktop only) — match the existing call style in
`src/lib/state/library.svelte.ts` rather than inventing a new one. If the
mocked test object trips the optional call, keep the `?.` and adjust the mock.

- [ ] **Step 4: Run the test**

```
timeout 300 npx vitest run src/lib/state/audition.test.ts
```

Expected: PASS, 10 tests. If the `prefs` import pulls `localStorage` into the
node project and throws, check how `src/lib/prefs/prefs.test.ts` sets that up
and follow the same setup — do not stop importing `prefs`, since R-2 depends
on it.

- [ ] **Step 5: Commit**

```bash
git add src/lib/state/audition.svelte.ts src/lib/state/audition.test.ts
git commit -m "feat(browser): audition store — one gate, four row kinds"
```

---

### Task 4: The gesture and the chip

**Files:**
- Modify: `src/lib/components/browser/BrowserRow.svelte`
- Modify: `src/lib/components/browser/BrowserShell.svelte`
- Modify: `src/lib/components/browser/BrowserShellHarness.svelte`
- Create: `src/lib/components/browser/AuditionChip.svelte`
- Test: `src/lib/components/browser/browser-audition.dom.test.ts`

**Interfaces:**
- Consumes: `audition` from `../../state/audition.svelte` (Task 3).
- Produces:
  - `BrowserRow` prop `ondblclick?: () => void`
  - `BrowserShell` props `onAudition?: (row: BrowserRowRef) => void` and
    `audition?: boolean` (render the chip in the toolbar)
  - `AuditionChip.svelte`, no props

- [ ] **Step 1: Write the failing test**

Read `docs/superpowers/plans/2026-08-18-dom-test-environment.md` first.

Create `src/lib/components/browser/browser-audition.dom.test.ts`:

```ts
/**
 * The unified gesture: double-click a row to hear it, Shift+Enter for the
 * keyboard, and the toolbar chip that turns the whole thing on and off.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import BrowserRow from "./BrowserRow.svelte";
import BrowserShellHarness from "./BrowserShellHarness.svelte";
import { audition } from "../../state/audition.svelte";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("BrowserRow audition gesture", () => {
  it("double-click calls ondblclick and single click does not", async () => {
    const onclick = vi.fn();
    const ondblclick = vi.fn();
    render(BrowserRow, { props: { id: "r1", label: "Kick", onclick, ondblclick } });
    const row = screen.getByText("Kick").closest(".row") as HTMLElement;

    await fireEvent.click(row);
    expect(ondblclick).not.toHaveBeenCalled();
    expect(onclick).toHaveBeenCalledTimes(1);

    await fireEvent.dblClick(row);
    expect(ondblclick).toHaveBeenCalledTimes(1);
  });

  it("a row with no ondblclick is inert on double-click", async () => {
    render(BrowserRow, { props: { id: "r1", label: "Kick" } });
    const row = screen.getByText("Kick").closest(".row") as HTMLElement;
    await expect(fireEvent.dblClick(row)).resolves.not.toThrow();
  });
});

describe("BrowserShell audition", () => {
  it("Shift+Enter auditions the active row without activating it", async () => {
    const onActivate = vi.fn();
    const onAudition = vi.fn();
    render(BrowserShellHarness, { props: { onActivate, onAudition } });
    const list = screen.getByRole("tree");
    list.focus();
    await fireEvent.keyDown(list, { key: "ArrowDown" });

    await fireEvent.keyDown(list, { key: "Enter", shiftKey: true });
    expect(onAudition).toHaveBeenCalledTimes(1);
    expect(onActivate).not.toHaveBeenCalled();

    await fireEvent.keyDown(list, { key: "Enter" });
    expect(onActivate).toHaveBeenCalledTimes(1);
    expect(onAudition).toHaveBeenCalledTimes(1);
  });

  it("the toolbar chip reflects and flips the audition preference", async () => {
    audition.enabled = false;
    render(BrowserShellHarness, { props: { audition: true } });
    const chip = screen.getByRole("button", { name: /audition/i });
    expect(chip.getAttribute("aria-pressed")).toBe("false");

    await fireEvent.click(chip);
    expect(audition.enabled).toBe(true);
    expect(chip.getAttribute("aria-pressed")).toBe("true");

    await fireEvent.click(chip);
    expect(audition.enabled).toBe(false);
  });
});
```

- [ ] **Step 2: Run it and watch it fail**

```
timeout 300 npx vitest run --project dom src/lib/components/browser/browser-audition.dom.test.ts
```

Expected: FAIL — `ondblclick` is not a prop, `onAudition` is not a prop, no
chip in the DOM.

- [ ] **Step 3: Add the `ondblclick` prop to `BrowserRow`**

In `src/lib/components/browser/BrowserRow.svelte`, add to the destructured
props (next to `onclick`):

```ts
    ondblclick,
```

to the type block (next to `onclick?: () => void;`):

```ts
    /** Audition: hear this row. Double-click, never single — single click
     * keeps whatever meaning it already had in each browser (design §8.2). */
    ondblclick?: () => void;
```

and to the `<div class="row">` element, next to `onclick`:

```svelte
  ondblclick={() => ondblclick?.()}
```

- [ ] **Step 4: Write the chip**

Create `src/lib/components/browser/AuditionChip.svelte`:

```svelte
<script lang="ts">
  /**
   * The audition toggle, in the toolbar rather than only in Preferences: a
   * dialog is the wrong distance away for a setting whose right answer
   * changes every few minutes (design §8.2).
   *
   * It writes the preference itself — there is no second session-level
   * mute that could disagree with it (ruling R-2).
   */
  import { audition } from "../../state/audition.svelte";

  const on = $derived(audition.enabled);
</script>

<button
  type="button"
  class="chip mono"
  class:on
  aria-pressed={on}
  aria-label={on ? "Audition on double-click: on" : "Audition on double-click: off"}
  title={on
    ? "Double-click a row to hear it — click to mute"
    : "Auditioning is off — click to hear rows on double-click"}
  onclick={() => (audition.enabled = !on)}
>
  <span aria-hidden="true">{on ? "♪" : "♪̸"}</span>
</button>

<style>
  .chip {
    flex: none;
    height: 24px;
    min-width: 24px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 0 6px;
    font-size: 11px;
    line-height: 1;
    border-radius: 5px;
    border: var(--border-width) solid var(--glass-border);
    background: rgb(var(--bg-0-rgb) / 0.7);
    color: var(--text-faint);
    cursor: pointer;
  }
  .chip:hover {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }
  .chip.on {
    color: var(--cyan);
    border-color: var(--cyan-dim);
    background: rgb(var(--cyan-rgb) / 0.08);
  }
</style>
```

If `♪̸` renders badly in the app, fall back to `♪` with the `on` class doing
all the work — check it during the ear-check, not now.

- [ ] **Step 5: Wire the shell**

In `src/lib/components/browser/BrowserShell.svelte`:

Import at the top of the `<script>`:

```ts
  import AuditionChip from "./AuditionChip.svelte";
```

Add to the destructured props and the type block:

```ts
    /** Shift+Enter on the active row. Omit and the shell offers no keyboard
     * audition — a browser whose rows cannot sound should not pretend. */
    onAudition?: (row: BrowserRowRef) => void;
    /** Render the audition toggle in the toolbar. */
    audition?: boolean;
```

with `audition = false` in the destructuring defaults.

In `onListKeydown`, replace the `case "Enter":` block with:

```ts
      case "Enter":
        if (activeRow) {
          e.preventDefault();
          // Shift+Enter is the keyboard's double-click: a browser row that
          // can only be heard with a mouse is not reachable at all.
          if (e.shiftKey) onAudition?.(activeRow);
          else onActivate?.(activeRow);
        }
        break;
```

In the toolbar markup, immediately before the `{#if filters}` block:

```svelte
    {#if audition}
      <AuditionChip />
    {/if}
```

- [ ] **Step 6: Teach the harness the new props**

`BrowserShellHarness.svelte` exists so the shell can be mounted in jsdom.
Give it pass-through props so the test above can drive them — read the file
first and follow its existing prop style exactly:

```ts
  let {
    onActivate,
    onAudition,
    audition = false,
  }: {
    onActivate?: (row: BrowserRowRef) => void;
    onAudition?: (row: BrowserRowRef) => void;
    audition?: boolean;
  } = $props();
```

and forward all three to `<BrowserShell ... {onActivate} {onAudition}
{audition} />`. Keep whatever props the harness already declares.

- [ ] **Step 7: Run the DOM tests**

```
timeout 300 npx vitest run --project dom src/lib/components/browser
```

Expected: PASS — the new file plus the pre-existing `browser-shell.dom.test.ts`
and `browser-folds.dom.test.ts`, which must not regress.

- [ ] **Step 8: Commit**

```bash
git add src/lib/components/browser/
git commit -m "feat(browser): double-click and Shift+Enter audition, toolbar chip"
```

---

### Task 5: Wire the library and instrument browsers

**Files:**
- Modify: `src/lib/components/library/SamplesRoot.svelte`
- Modify: `src/lib/components/library/PresetsRoot.svelte`
- Modify: `src/lib/components/instruments/InstrumentBrowser.svelte`

**Interfaces:**
- Consumes: `audition` (Task 3), `AUDITION_KEY` (Task 2), `BrowserRow`'s
  `ondblclick` and `BrowserShell`'s `onAudition` / `audition` (Task 4).
- Produces: nothing new.

Per ruling R-1, **do not touch any existing single-click or hold-to-play
handler in these files.** You are adding a gesture, not moving one.

- [ ] **Step 1: `SamplesRoot` — audio rows**

Import:

```ts
  import { audition } from "../../state/audition.svelte";
```

On the `BrowserRow` inside the `{#each g.items as e, i}` loop, add:

```svelte
                ondblclick={() =>
                  e.kind === "audio" && void audition.play({ kind: "sample", path: e.path })}
```

On its `<BrowserShell>`, add `audition` and the keyboard route:

```svelte
      audition
      onAudition={(row) => {
        if (row.kind !== "item") return;
        const e = groups.find((g) => g.key === row.groupKey)?.items[row.itemIndex];
        if (e?.kind === "audio") void audition.play({ kind: "sample", path: e.path });
      }}
```

- [ ] **Step 2: `PresetsRoot` — instruments and patches**

Import:

```ts
  import { audition } from "../../state/audition.svelte";
  import { AUDITION_KEY } from "../../utils/audition-target";
  import { plugins } from "../../state/plugins.svelte";
  import { project } from "../../state/project.svelte";
  import { resolvePluginInstanceTarget } from "../../utils/audition-target";
```

(check `plugins` / `project` are not already imported in this file before
adding a duplicate import).

On the instrument `BrowserRow`:

```svelte
              ondblclick={() =>
                void audition.play({ kind: "instrument", instrumentId: i.id, key: AUDITION_KEY })}
```

On the patch `BrowserRow` — a Zyn patch can only be heard through a live Zyn
instance, and *loading* one is an edit, so the honest answer here is a silent
target rather than a load:

```svelte
              ondblclick={() => {
                const open = zyn.openInstance;
                void audition.play(
                  open
                    ? resolvePluginInstanceTarget(open.id, project.tracks)
                    : { kind: "silent", reason: "open a Zyn instance to audition its patches" },
                );
              }}
```

Add `audition` to the `<BrowserShell>`. Wire `onAudition` to the same two
branches, reading the item out of `instrumentGroups` / `patchGroups` by
`row.groupKey` and `row.itemIndex` exactly as Step 1 does for `groups`.

- [ ] **Step 3: `InstrumentBrowser` — instrument rows**

Import `audition` and `AUDITION_KEY` as above, then on the `BrowserRow`:

```svelte
                ondblclick={() =>
                  void audition.play({ kind: "instrument", instrumentId: inst.id, key: AUDITION_KEY })}
```

The mini keyboard in the `actions` snippet stays exactly as it is — it answers
a different question ("what does *this* pitch sound like"), which §8.2 says
explicitly.

Add `audition` to its `<BrowserShell>` and route `onAudition` to the same
call, resolving the instrument from `groups[...]` by the row ref.

- [ ] **Step 4: Verify nothing regressed**

```
timeout 300 npx vitest run
```

Expected: PASS. Then read your own diff and confirm no existing `onclick`,
`onpointerdown`, `onpointerup` or `onpointercancel` handler was modified or
removed:

```bash
git diff -U2 | grep -E '^[-+].*on(click|pointerdown|pointerup|pointercancel)='
```

Every line that grep prints must be a `+` on a line you added, never a `-`.

- [ ] **Step 5: Commit**

```bash
git add src/lib/components/library src/lib/components/instruments
git commit -m "feat(browser): audition samples, instruments and zyn patches"
```

---

### Task 6: Wire the plugin browsers

**Files:**
- Modify: `src/lib/components/plugins/PluginManager.svelte`
- Modify: `src/lib/components/plugins/ZynPatchBrowser.svelte`

**Interfaces:**
- Consumes: `audition`, `resolveDescriptorTarget`,
  `resolvePluginInstanceTarget`, `AuditionChip`.
- Produces: nothing new.

- [ ] **Step 1: Rack rows in `PluginManager`**

Import:

```ts
  import { audition } from "../../state/audition.svelte";
  import AuditionChip from "../browser/AuditionChip.svelte";
  import { resolveDescriptorTarget, resolvePluginInstanceTarget } from "../../utils/audition-target";
```

On the rack `BrowserRow` (the one in the `rackGroups` loop, keyed by
`entry.instance.id`), add:

```svelte
                  ondblclick={() =>
                    void audition.play(resolvePluginInstanceTarget(inst.id, project.tracks))}
```

- [ ] **Step 2: Browse rows in `PluginManager`**

On the browse `BrowserRow` (the one in the `s.items` loop, keyed by `d.uid`):

```svelte
                  ondblclick={() =>
                    void audition.play(
                      resolveDescriptorTarget(d.uid, plugins.instances, project.tracks),
                    )}
```

R-3 in force: a descriptor with nothing live stays silent. Do not add an
`plugin_instantiate` fallback here — that is the whole point of the ruling.

- [ ] **Step 3: The chip in the manager's own toolbar**

`PluginManager` passes `chrome={false}` to `BrowserShell`, so the shell draws
no toolbar and the chip has to go in the manager's own chip row. Add it as the
last chip in the row that holds `ALL` / `INST` / `FX` / `CLAP` / `LV2` / `★` /
`⏱`:

```svelte
        <AuditionChip />
```

- [ ] **Step 4: Surface the silent reason**

A double-click that makes no sound with no explanation reads as a bug. Below
the chip row (next to wherever the manager already renders its status/error
line — read the file and follow it), add:

```svelte
    {#if audition.lastSilentReason}
      <div class="note silk" role="status">{audition.lastSilentReason}</div>
    {/if}
```

and a `.note` rule in `<style>` using existing tokens only — copy the shape of
`SamplesRoot.svelte`'s `.note` rule rather than inventing colours.

- [ ] **Step 5: `ZynPatchBrowser`**

Its rows are hand-rolled (windowed list — §10.12), so it gets the gesture
directly on its row element. Find the patch row markup and add an
`ondblclick` that goes through the store, keeping the existing hold-to-play
pointer handlers exactly as they are:

```svelte
  ondblclick={() => {
    if (!inst || inst.status !== "active") {
      void audition.play({ kind: "silent", reason: "no live Zyn instance to audition through" });
      return;
    }
    void zyn.audition(inst.id, patch);
    setTimeout(() => void zyn.previewUp(), 700);
  }}
```

with `import { audition } from "../../state/audition.svelte";` at the top, and
guard the whole handler on `audition.enabled` — `zyn.audition` loads the patch,
which *is* an edit, so it must not fire when the gate is off:

```svelte
    if (!audition.enabled) return;
```

as the handler's first line. Put that first line above the `inst` check.

- [ ] **Step 6: Run the suites**

```
timeout 300 npx vitest run
```

Expected: PASS, including the pre-existing
`src/lib/components/plugins/zyn-patch-browser.dom.test.ts`, which must not
regress.

Then the type check and lint the project already uses — read `package.json`'s
`scripts` and run the check/lint script it defines (do not guess the name):

```bash
timeout 300 npm run check
```

- [ ] **Step 7: Commit**

```bash
git add src/lib/components/plugins
git commit -m "feat(plugins): audition rack and catalog rows through the shared store"
```

---

### Task 7: Documentation, counts, and releasing the claim

**Files:**
- Modify: `README.md`, `CONTRIBUTING.md`
- Modify: `docs/backlog/plugin-manager.md`
- Modify: `docs/LANDED.md`
- Modify: `next-prompt.md`

- [ ] **Step 1: Measure the real test count**

```
timeout 900 cargo test --manifest-path src-tauri/Cargo.toml -- --test-threads=1
timeout 300 npx vitest run
```

Both must be green. Write down the exact frontend numbers vitest prints —
total, and the `unit` / `dom` split. Do not copy a number from this plan or
from any briefing; the convention exists because copied numbers rot.

- [ ] **Step 2: Update the dated counts**

In `README.md` (the `npm test` line, currently "1215 frontend tests (1105 unit
+ 110 DOM-mounted; counted 2026-08-21 …)") and the matching line in
`CONTRIBUTING.md`: put the measured numbers in, change the date to
2026-08-23, and change the trailing "after …" clause to
`after Step 7 (unified browser audition) landed`. Append the new suites to the
end of the feature list: `+ audition targets + the audition store + the
browser audition gesture`. The Rust count is unchanged by this branch — leave
it alone unless your run disagrees with it, in which case update it and say so
in the commit message.

- [ ] **Step 3: Update the track backlog**

In `docs/backlog/plugin-manager.md`:

Replace the `## Next — Step 7, unified audition` section with a
`## What shipped, in detail` row for Step 7 (follow the table style already in
that file):

```markdown
**Step 7 (PR #106):**

| Piece | Detail |
|---|---|
| The gesture | `BrowserRow`'s `ondblclick`, plus Shift+Enter on `BrowserShell` for keyboard parity. Single click is untouched everywhere (ruling R-1). |
| The gate | `browserAudition` pref, default off, plus `AuditionChip.svelte` in every browser toolbar. The chip writes the pref — there is no second session mute (ruling R-2). |
| The dispatch | `utils/audition-target.ts` resolves a row to an `AuditionTarget`; `state/audition.svelte.ts` plays it through `sampler_preview_note` / `plugin_preview_note` / `library_audition`. No new IPC, no ops. |
| Catalog plugins | A descriptor borrows a live, track-bound instance of the same plugin if one exists; otherwise silent with a reason. Instantiating to preview would commit `Op::PluginAdd` (ruling R-3). |
```

Then add to the **Leftovers** section:

```markdown
- **The document-free plugin preview slot.** §8.2 asks for a lazily
  instantiated, reusable preview slot so a catalog plugin can be heard
  without being added to the project. Step 7 deliberately did not build it:
  `plugin_preview_note` routes through the instance's bound midi track, so
  reaching a bare descriptor means `plugin_instantiate` — an `Op::PluginAdd`
  with an undo entry, which an audition must never be. Doing it properly is
  backend work: host an instance outside the document and render it into the
  preview stream that `audio/sampler_preview.rs` already owns, with a single
  reusable slot and teardown on a timer. Additive IPC. Owner scoped it out of
  Step 7 on 2026-08-23.
```

And to the **Owner ear-checks still owed** section:

```markdown
- **From Step 7:** does double-click-to-audition feel right at C3, and is
  1200 ms the right decay for the row highlight? And the R-1 question the
  plan did not answer: `SamplesRoot` still auditions on *single* click and
  `InstrumentBrowser` still sounds on hold, both regardless of the new
  preference, because gating them would delete shipped behaviour. Should the
  preference own those too?
```

- [ ] **Step 4: Record it in `docs/LANDED.md`**

Add a row in that file's existing style: Step 7 / unified browser audition /
PR #106, pointing at `docs/backlog/plugin-manager.md`.

- [ ] **Step 5: Release the claim**

In `next-prompt.md`: delete the `feat/browser-audition` row from *Active
claims*, restoring the `| _(none)_ | | | |` row if the table is now empty.
Remove Step 7 from *Next up — unclaimed* and renumber what remains. In the
Backlog table, change "Plugin manager (Step 7 next)" to
"Plugin manager (preview slot open)".

- [ ] **Step 6: Commit**

```bash
git add README.md CONTRIBUTING.md docs next-prompt.md
git commit -m "docs: Step 7 landed — unified browser audition; release the claim"
```

- [ ] **Step 7: Final gate before review**

```
timeout 300 npx vitest run
timeout 900 cargo test --manifest-path src-tauri/Cargo.toml -- --test-threads=1
timeout 300 npm run check
```

All three green, in the foreground, before the PR leaves draft. Never claim a
gate passed without pasting what it printed.

---

## Self-review

**Spec coverage.** §8.2's table of four inconsistent surfaces: `SamplesRoot`
(Task 5.1), `PresetsRoot` (Task 5.2), `InstrumentBrowser` (Task 5.3),
`PluginBrowser` (Task 6.2), `ZynPatchBrowser` (Task 6.5) — all five wired.
"Double-click a row" — Task 4. "C3 for anything pitched, the sample itself
for audio" — `AUDITION_KEY = 60` in Task 2, the `sample` arm in Task 3.
"Single click keeps its current meaning" — ruling R-1, enforced by Task 5's
Step 4 grep. "The mini keyboard stays" — Task 5.3. "Never let a preview
become a project edit" — the `silent` arm, rulings R-1/R-3, and the Zyn
`enabled` guard in Task 6.5. "New `browserAudition` pref … default off" —
Task 1. "Surface a mute toggle in the shell's chip row" — Task 4's chip,
placed in Task 4.5 and Task 6.3.

**Not covered, deliberately, with the owner's agreement:** "instantiate
lazily into a single reusable preview slot, tear it down on a timer". That
sentence is the backend preview slot; ruling R-3 explains why it is not in
this step, and Task 7.3 files it as its own job. §8.1 (collapse all / expand
all) was folded into Step 5 and already shipped.

**Placeholders.** None: every code step carries the code, every test step
carries the assertions, and the two places the plan says "read the file first"
(`BrowserShellHarness`'s prop style, `PluginManager`'s status line) name
exactly what to look for rather than deferring a decision.

**Type consistency.** `AuditionTarget`'s four arms are spelled `sample`,
`instrument`, `pluginInstance`, `silent` in Tasks 2, 3, 5 and 6.
`AUDITION_KEY` (not `PREVIEW_KEY`) throughout. `audition.play` /
`audition.stop` / `audition.enabled` / `audition.sounding` /
`audition.lastSilentReason` are the only store members any task calls, and all
five are declared in Task 3. `resolvePluginInstanceTarget(instanceId, tracks)`
and `resolveDescriptorTarget(uid, instances, tracks)` keep their argument
order in Tasks 5 and 6.
