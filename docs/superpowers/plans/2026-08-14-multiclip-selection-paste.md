# Multi-Clip Selection, Group Drag & Cross-Instance Paste Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> `superpowers:subagent-driven-development` (recommended) or
> `superpowers:executing-plans` to implement this plan task-by-task. Steps
> use checkbox (`- [ ]`) syntax for tracking. Execution mode for this run is
> decided by the owner at execution time and recorded in the execution note
> at the bottom of this file.

**Goal:** Give the arrangement timeline a real multi-clip selection (audio
and MIDI together), let a selection be dragged as ONE undoable gesture with
every clip's relative offset preserved, and let a selection be copied and
pasted at the playhead — onto the same tracks or onto freshly created ones —
including across two running AURA instances via the OS clipboard.

**Architecture:** Selection is **viewer state** — a `Set<string>` of
`"<kind>:<id>"` keys in a new frontend store that never enters the document,
never reaches a snapshot, and never becomes an op. Every mutation goes
through the landed Plan E channel: group drag emits ONE additive
`move_clips` command whose `ControlPlane` method mirrors `set_track_mix`
exactly (gesture-aware transient fold when a gesture is open, one plain
commit otherwise), and paste emits ONE additive `clips_paste` command whose
single `commit` closure composes the EXISTING `TrackAdd` / `ClipAdd` /
`MidiClipAdd` ops — so undo sees one entry and the op vocabulary does not
grow. The clipboard payload is built backend-side by an additive
`clips_copy` READ command, so the in-app and cross-instance paths carry
byte-identical JSON, and the OS clipboard is reached through two thin
`arboard`-backed commands rather than webview clipboard permissions.

**Tech Stack:** Rust (`src-tauri/`: serde, serde_json, parking_lot, arboard),
TypeScript / Svelte 5 (`src/lib/`), vitest.

**Spec:** `docs/backlog/multi-clip-selection-and-paste.md` (the owner's
requirements, verbatim intent, plus the design notes mapping them onto the
post-Plan-E architecture) + `next-prompt.md` §3's **Track C** entry (the
binding scope definition) + §2 (standing constraints). Supporting: ADR 0006
(thin renderer), ADR 0004 / round-2 §5 (content vs placement), round-2 §2.1
(copy mints fresh), round-2 §4.4 (gestures, coalescing, value-replacement
wrappers), `docs/PHASE4-PLAN.md`'s **"Plan E handoff"** section (the
primitives this plan stands on).

---

## Global Constraints

Every task's requirements implicitly include this section.

- **The op log is ON, and this plan adds NO new op kind, NO new `PropPath`
  variant, and NO new `ObjectRef` variant.** `OP_FORMAT_VERSION` stays **2**
  (`src-tauri/src/control/op.rs:32`). If a task finds itself reaching for a
  new op shape, that is a scope violation — stop and re-read scope ruling
  A. A version bump would additionally require a dual-shape reader, which
  is Track A's surface, not this one's.
- **Thin renderer (ADR 0006).** No new authoritative state and no new time
  math frontend-side. The ONE sanctioned client-side bijection is
  `src/lib/sectionTable.ts` reached through `midi.ticksToSamples` /
  `midi.samplesToTicks` (`src/lib/state/midi.svelte.ts:65-71`). Paste
  placement math (anchor resolution, tick↔sample conversion of the paste
  point, per-clip offsets) happens **backend-side**, inside
  `clips_copy` / `clips_paste`. The frontend only supplies a playhead
  position in samples and a boolean.
- **Selection is viewer state.** It lives in one frontend store, is never
  serialized into `project.json`, never appears in `ProjectSnapshot`, never
  becomes an op, and is never read by the backend. `clips_copy` /
  `clips_paste` take explicit clip-id lists as arguments — the backend never
  learns what is "selected".
- **Frozen command/event names stay frozen; new commands are additive.**
  New additive commands in this plan: `move_clips`, `clips_copy`,
  `clips_paste`, `os_clipboard_write_text`, `os_clipboard_read_text`. No
  existing command name, signature, or event name changes.
- **`transact` closures must not panic** (no panic rollback until Plan F).
  Validate everything — clip existence, track existence, track kind, asset
  resolution — BEFORE the first `tx.apply`, exactly as every landed op arm
  does. No `unwrap`, no `expect`, no indexing that can go out of bounds
  inside a commit closure.
- **Prepare-outside / commit-inside** (round-2 §4.4). Asset resolution and
  any file copy in `clips_paste` happen BEFORE the transaction opens. The
  transaction is a short apply. No file I/O inside a commit closure.
- **The M-3 redo invariant.** The only transient batches this plan produces
  are `move_clips`'s mid-gesture folds, which are sanctioned by
  `IN_GESTURE_FOLD` (`src-tauri/src/control/mod.rs`,
  `debug_assert_transient_invariant`). No task may mark any other commit
  `.transient()`. If a `debug_assert!` about the transient invariant fires
  in the test suite, that is this rule catching a real bug — do not silence
  it.
- **Gesture-before-session lock order** (Task 14's fix, binding). A group
  drag IS a gesture: `move_clips` must reach its commit through
  `GestureState::commit_transient_and_fold` (which holds the gesture mutex
  across the nested session-lock acquisition), never take the gesture lock
  while holding the session lock, and never re-acquire the gesture lock
  after committing. Copy the `set_track_mix` shape verbatim
  (`src-tauri/src/control/mod.rs:1504-1565`) rather than re-deriving it.
  **Frontend half of the same rule:** `await` `gestureBegin` before the
  first mutating invoke of the drag, and `await` the `moveClips` invoke
  before calling `gestureEnd` — an un-awaited mutation racing `gestureEnd`
  is exactly the TOCTOU Task 14 closed.
- **Foreground, `timeout`-guarded test runs only. Never background a test
  run.**
  ```
  timeout 900 cargo test --manifest-path src-tauri/Cargo.toml
  timeout 300 npx vitest run
  ```
- **Baseline, verified in this worktree at branch base `3340aa8`
  (2026-08-14): 527 backend (499 lib + 3 + 2 + 2 + 13 + 2 + 2 + 4 across the
  integration binaries) + 206 frontend, all green.** Note that
  `README.md:389` and `CONTRIBUTING.md:62` still say **506** — they predate
  PR #17's merge and are stale by 21. The first task to change the backend
  count corrects them to the true post-task number, dated. **Observed
  flake:** one `cargo test` run in this worktree failed in the `--lib`
  target with only LV2/Zyn stderr noise (`Sending key 'state' to UI failed,
  out of space`) visible; an immediate re-run was fully green at 527. If a
  run fails, re-run once and read the actual failing test name before
  concluding anything — but never "just update the number" if the count
  moves without your changes explaining it.
- **Dated test-count convention.** Any task that changes a test count
  updates `README.md:389` (backend) / `README.md:390` (frontend) and
  `CONTRIBUTING.md:62` (backend) in the SAME commit, with the date
  `2026-08-14` (or the actual date of execution if later).
- **Evidence policy (ADR 0007):** corrections are marked, never silent. If
  the backlog doc contradicts something here, this plan's scope ruling wins
  and the deviation is already recorded below.

---

## Scope rulings (decided now, so no task stalls mid-flight)

**A. Paste composes existing ops in ONE transaction; no new op, no batch
`clips.paste` op.** `ControlPlane::clips_paste` opens exactly one
`self.commit(TxMeta::user("paste clips"), |tx| …)` and inside it applies, in
order: zero or more `Op::TrackAdd` (via the landed `ops::add_track_tx`,
paste-to-new-tracks only), then one `Op::ClipAdd` per audio clip and one
`Op::MidiClipAdd` per MIDI clip. `Session::transact` folds and returns ONE
`Committed`; `Committer::commit_with_rebuild` hands that to
`HistoryLog::record_commit`, which builds ONE `HistoryEntry` — and because
the batch contains structural ops, `batch_coalesce_key` returns `None`
(`src-tauri/src/control/history.rs:137-145`), so the 350 ms fallback can
never merge two pastes into one step either. Undo therefore removes every
pasted clip AND every track the paste created, in one Ctrl+Z. The exact
precedent is `control/hum.rs`'s `commit_hum_clip` (optional `add_track_tx` +
`Op::MidiClipAdd` in one tx) — mirror it. **This is why
`OP_FORMAT_VERSION` stays 2 and no journal migration is needed.**

**B. Payload contents: MIDI by value, audio by reference — and a missing
asset skips that clip, never the whole paste.** The
`application/x-aura-clips` payload carries, per clip:
- **MIDI:** the complete note list by value (`tick`, `lengthTicks`, `key`,
  `velocity`, `channel`) with `noteId` **stripped to the mint sentinel
  `NoteId(0)`** (round-2 §2.1: copy mints fresh; ADR 0001: ids are never
  reused), plus `lengthTicks`, `contentLengthTicks`, `name`, the source
  track's id and name, and the clip's tick offset from the anchor.
- **Audio:** a **reference**, never bytes — `sourcePath` (project-relative,
  POSIX), `sourceAbsPath` (the absolute path resolved at copy time, `null`
  when no project is open), `sourceChannels`, `sourceSampleRate`,
  `sourceLengthSamples`, `offsetSamples` (into the source), `lengthSamples`,
  `gainDb`, both fades, the source track's id and name, and the clip's
  sample offset from the anchor. A wav is megabytes; the clipboard is not an
  asset store.

  On paste, `clips_paste` resolves each audio clip's asset BEFORE opening
  the transaction, in this order: (1) `sourcePath` resolves inside THIS
  session's project dir → reference it in place, reusing the path (no copy,
  no new `SourceId` needed beyond the fresh clip identity); (2) else
  `sourceAbsPath` is `Some`, exists, and this session HAS a project dir →
  copy the file into `<project>/audio/` under a fresh name and reference the
  new relative path; (3) else the clip is **skipped** and reported in
  `PasteResult.skipped` as `{ name, reason }` with reason
  `"missing audio source: <path>"` or `"no project open"`. Every MIDI clip
  in the same payload still lands, in the same single transaction. A paste
  where EVERY clip skipped commits nothing (the closure returns before any
  `tx.apply`) and returns an all-skipped result — not an error, because the
  user asked for a paste and got a precise report.

**C. SMF is EXPORT-ONLY, to a FILE, and needs no new backend surface.** The
OS clipboard slot AURA can actually reach in a Tauri v2 / WebKitGTK app is
plain text — arbitrary MIME flavors are not available, so a binary SMF blob
cannot ride on the clipboard at all. The `application/x-aura-clips` MIME
name therefore lives INSIDE the JSON envelope (a `mime` field plus a magic
first line), not as an OS clipboard flavor. The SMF half is delivered
instead as **"Export selection as MIDI…"**, which reuses the FROZEN,
already-shipped `midi_export_file(path, clipIds)` command
(`src-tauri/src/midi/mod.rs:666`) — it already takes a clip-id filter.
**Paste does NOT parse SMF**; nothing can put SMF on the clipboard for it to
find. Recorded as a deliberate narrowing of the backlog's "SMF fallback on
paste".

**D. Group-drag snapping snaps the ANCHOR ONLY, and the resulting delta is
applied to every clip.** Snapping each clip independently would destroy the
relative offsets requirement 1 explicitly demands. The anchor is the clip
under the pointer. The drag reuses the EXISTING `view.snapSamples`
(`src/lib/state/view.svelte.ts:53`) on the anchor's target position —
identical to what `ClipView`/`MidiClipView` already do for a single clip, so
a one-clip selection behaves bit-for-bit as it does today, and `Alt` still
bypasses snapping. From the anchor the drag derives ONE pair
`(deltaSamples, deltaTicks)`, where
`deltaTicks = midi.samplesToTicks(anchorOrigSamples + deltaSamples) -
midi.samplesToTicks(anchorOrigSamples)` — one conversion per pointermove,
through the shipped section table, exactly as `MidiClipView` converts today.
Audio clips move by `deltaSamples`, MIDI clips by `deltaTicks`. The delta —
not the individual clip — is clamped so the leftmost selected clip lands at
0 and no further, mirroring `nudgeSelection`'s "blocking preserves the
rhythm's internal offsets" precedent (`src/lib/utils/note-ops.ts:92`).

**E. Selection keys are `"<kind>:<id>"`, not bare `ClipId`.** Audio clips
and MIDI clips are minted by two independent stores, so a bare id set cannot
tell `project.clips` from `midi.clips`. Track C's brief says
`Set<ClipId>`; this is a marked, mechanical widening of that — the SET is
still one flat set of clip identities, it just carries the store tag needed
to resolve each one. `refKey`/`parseKey` are the only places the encoding is
known.

**F. The legacy single-selection fields stay, as the "focused clip".**
`project.selectedClipId` and `midi.selectedClipId` keep their current
meaning (the ONE clip the Dock, the "SPLIT STEMS" button, AI-Studio
targeting and the piano roll act on) and are set to the last-clicked clip
alongside the new multi-selection. Nothing that reads them changes.
`midi.copySelected` / `pasteAtPlayhead` / `duplicateSelected` and their
tests (`src/lib/state/midi-stamp.test.ts`) are **kept untouched**; only the
`App.svelte` keyboard bindings for Ctrl+C / Ctrl+V move to the new
multi-clip path. **Ctrl+D stays wired to `midi.duplicateSelected`** —
multi-clip duplicate is not in this plan's scope, recorded rather than
silently dropped.

**G. A group RESIZE (loop-length adjust) applies to MIDI clips only.**
Audio clips in the selection are left untouched by an edge drag.
`PropPath::LengthSamples` exists, but resizing an audio clip is not a
shipped gesture at HEAD (`ClipView` has no edge-drag zone at all), and
inventing one here would be scope creep with real semantics to settle
(source-length clamping, fades). Recorded; the `move_clips` command's
payload leaves room for it (an audio entry simply never carries a length).

**H. `contentLengthTicks` can be SET but not CLEARED through `move_clips`.**
Clearing back to "same as `lengthTicks`" keeps going through the existing
`midi_set_clip_bounds(…, null)` path. `Option<u64>` on the wire means
"unchanged" for the batch command, not "clear" — the ambiguity of a
nested `Option<Option<u64>>` is not worth a batch API. Recorded.

**I. The OS clipboard is reached through two additive Rust commands backed
by `arboard`, not through the webview.** `navigator.clipboard.readText()`
requires a `clipboard-read` permission WebKitGTK does not grant, which is
precisely the cross-instance paste case this plan exists to serve. Adding
`tauri-plugin-clipboard-manager` would require editing
`src-tauri/capabilities/default.json`, which is a **FROZEN FILE** (its own
header says so). Two `#[tauri::command]` functions over `arboard` need
neither. They are not unit-tested (a headless test environment has no
clipboard); the **codec** on both sides is, purely.

---

## Non-goals (so the diff stays reviewable)

- **No content instancing.** A paste mints a fresh `ContentId` per clip
  (plain copy, matching the piano-roll paste precedent). Shared-`ContentId`
  true instancing is round-2 §5 / ADR 0004's own later feature — the backlog
  doc already says "decide at implementation time: plain copy first"; this
  is that decision, recorded.
- **No `engine.rs` changes.** Zero. If a task touches
  `src-tauri/src/audio/engine.rs`, it has left this track.
- **No SMF import/paste** (scope ruling C).
- **No multi-clip DELETE, split, or duplicate** — selection-aware delete is
  an obvious follow-on but is not in Track C's brief.
- **No MCP tools** for any of this (the roster is frozen, ARCHITECTURE
  §12.2).
- **No fix for the hardcoded MCP port 41717**, which real two-instance
  workflows also need — tracked in the roadmap's Tier 2, called out by the
  backlog doc, explicitly out of scope here.
- **No demo-backend (`src/lib/demo.ts`) implementations.** Every new
  command is declared OPTIONAL on the `Backend` interface (`moveClips?`,
  `clipsCopy?`, …), the same convention `moveClip?` / `gestureBegin?` /
  `undo?` already use. In a plain browser the new gestures degrade to
  local-preview-only, exactly as `moveClip?` does today.

---

## File structure

**New, frontend:**
| File | Responsibility |
|---|---|
| `src/lib/utils/clip-selection.ts` | Pure selection algebra: `ClipRef`, key encoding, `applySelection` modes, `marqueeClipHits`. No Svelte, no stores. |
| `src/lib/utils/clip-selection.test.ts` | Its tests. |
| `src/lib/utils/aura-clips.ts` | Pure envelope codec: `encodeAuraClips`, `parseAuraClips` (validating), the magic line and schema version. |
| `src/lib/utils/aura-clips.test.ts` | Its tests. |
| `src/lib/state/clip-selection.svelte.ts` | The viewer-state store: the `Set`, the anchor, live filtering against `project.clips`/`midi.clips`. |
| `src/lib/state/clip-selection.test.ts` | Its tests. |
| `src/lib/state/clip-drag.svelte.ts` | The group drag/resize controller: origins snapshot, delta math, gesture + `move_clips` emission. |
| `src/lib/state/clip-drag.test.ts` | Its tests. |
| `src/lib/state/clip-clipboard.svelte.ts` | Copy/paste orchestration: `clips_copy` → OS clipboard, OS clipboard → `clips_paste`, store application, skip toasts. |
| `src/lib/state/clip-clipboard.test.ts` | Its tests. |

**New, backend:**
| File | Responsibility |
|---|---|
| `src-tauri/src/control/clipboard.rs` | `AuraClipsPayload` + `ClipboardClip` wire types, `ControlPlane::clips_copy`, `ControlPlane::clips_paste`, the two Tauri command wrappers, and their tests. |
| `src-tauri/src/osclipboard.rs` | `os_clipboard_write_text` / `os_clipboard_read_text` over `arboard`. |

**Modified:**
`src/lib/components/ClipView.svelte`, `src/lib/components/MidiClipView.svelte`,
`src/lib/components/Timeline.svelte`, `src/App.svelte`,
`src/lib/tauri.ts`, `src/lib/types/ipc.ts`,
`src-tauri/src/control/mod.rs` (the `move_clips` method + command, and
`mod clipboard;`), `src-tauri/src/lib.rs` (command registration),
`src-tauri/Cargo.toml` (`arboard`), `README.md`, `CONTRIBUTING.md`,
`docs/backlog/multi-clip-selection-and-paste.md`, `next-prompt.md`.

**Dependency order:** 1 → 2 → 3 → 4 (selection), 5 → 6 → 7 (group drag),
8 → 9 → 10 → 11 (clipboard), 12 (SMF), 13 (close-out). Tasks 5 and 8 have no
frontend prerequisite and may run in parallel with 1-4 in separate
worktrees if the executor wants; everything else is sequential.

---

### Task 1: Pure clip-selection algebra

The clip-level mirror of `src/lib/utils/note-ops.ts` (PR #11's note-level
selection model). Everything here is pure so it unit-tests without a canvas,
a store, or a backend — exactly the reason `note-ops.ts` is pure.

**Files:**
- Create: `src/lib/utils/clip-selection.ts`
- Test: `src/lib/utils/clip-selection.test.ts`

**Interfaces:**
- Consumes: `MarqueeMode`-style modes only in spirit; imports nothing from
  the codebase except types.
- Produces (later tasks import exactly these names):
  - `type ClipKind = "audio" | "midi"`
  - `interface ClipRef { kind: ClipKind; id: string }`
  - `type SelectionMode = "replace" | "add" | "toggle" | "subtract"`
  - `refKey(ref: ClipRef): string`
  - `parseKey(key: string): ClipRef`
  - `applySelection(current: Set<string>, hits: ClipRef[], mode: SelectionMode): Set<string>`
  - `interface LaneBox { ref: ClipRef; laneIndex: number; startSamples: number; endSamples: number }`
  - `marqueeClipHits(boxes: LaneBox[], s0: number, s1: number, laneLo: number, laneHi: number): ClipRef[]`

- [ ] **Step 1: Write the failing test**

Create `src/lib/utils/clip-selection.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import {
  applySelection,
  marqueeClipHits,
  parseKey,
  refKey,
  type ClipRef,
  type LaneBox,
} from "./clip-selection";

const A = (id: string): ClipRef => ({ kind: "audio", id });
const M = (id: string): ClipRef => ({ kind: "midi", id });

describe("refKey / parseKey", () => {
  it("round-trips both kinds", () => {
    expect(refKey(A("c-1"))).toBe("audio:c-1");
    expect(refKey(M("c-1"))).toBe("midi:c-1");
    expect(parseKey("midi:c-1")).toEqual(M("c-1"));
  });

  it("keeps an id containing a colon intact", () => {
    // ids are uuids today, but the encoding must not be the weak link.
    expect(parseKey(refKey(A("a:b:c")))).toEqual(A("a:b:c"));
  });

  it("distinguishes an audio and a midi clip that share an id", () => {
    expect(refKey(A("x"))).not.toBe(refKey(M("x")));
  });
});

describe("applySelection", () => {
  const cur = new Set(["audio:c-1", "midi:c-2"]);

  it("replace keeps only the hits", () => {
    expect(applySelection(cur, [M("c-3")], "replace")).toEqual(new Set(["midi:c-3"]));
  });

  it("add unions without dropping the existing selection", () => {
    expect(applySelection(cur, [M("c-3")], "add")).toEqual(
      new Set(["audio:c-1", "midi:c-2", "midi:c-3"]),
    );
  });

  it("subtract removes hits and ignores misses", () => {
    expect(applySelection(cur, [A("c-1"), A("nope")], "subtract")).toEqual(
      new Set(["midi:c-2"]),
    );
  });

  it("toggle flips each hit independently", () => {
    expect(applySelection(cur, [A("c-1"), M("c-3")], "toggle")).toEqual(
      new Set(["midi:c-2", "midi:c-3"]),
    );
  });

  it("never mutates the input set", () => {
    const before = new Set(cur);
    applySelection(cur, [M("c-9")], "add");
    expect(cur).toEqual(before);
  });
});

describe("marqueeClipHits", () => {
  const boxes: LaneBox[] = [
    { ref: A("a1"), laneIndex: 0, startSamples: 0, endSamples: 1000 },
    { ref: M("m1"), laneIndex: 1, startSamples: 500, endSamples: 1500 },
    { ref: A("a2"), laneIndex: 2, startSamples: 4000, endSamples: 5000 },
  ];

  it("hits clips whose span overlaps the band and whose lane is in range", () => {
    expect(marqueeClipHits(boxes, 400, 600, 0, 1)).toEqual([A("a1"), M("m1")]);
  });

  it("excludes clips outside the lane range even when the time overlaps", () => {
    expect(marqueeClipHits(boxes, 400, 600, 1, 1)).toEqual([M("m1")]);
  });

  it("uses half-open overlap: touching edges do not count", () => {
    // a1 spans [0,1000); a band starting exactly at 1000 must miss it.
    expect(marqueeClipHits(boxes, 1000, 1200, 0, 2)).toEqual([M("m1")]);
  });

  it("returns an empty array when nothing overlaps", () => {
    expect(marqueeClipHits(boxes, 2000, 3000, 0, 2)).toEqual([]);
  });
});
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `timeout 300 npx vitest run src/lib/utils/clip-selection.test.ts`
Expected: FAIL — `Failed to resolve import "./clip-selection"`.

- [ ] **Step 3: Write the implementation**

Create `src/lib/utils/clip-selection.ts`:

```ts
/**
 * Pure clip-selection operations for the arrangement timeline — the
 * clip-level mirror of note-ops.ts's note-level selection (PR #11).
 *
 * Selection is VIEWER state: it never enters the document, never becomes an
 * op, and the backend never learns what is selected. It is keyed by
 * `"<kind>:<id>"` rather than a bare clip id because audio clips
 * (project.clips) and MIDI clips (midi.clips) are minted by two independent
 * stores, so an id alone cannot say which store to resolve it against.
 * `refKey`/`parseKey` are the ONLY places that encoding is known.
 */

export type ClipKind = "audio" | "midi";

export interface ClipRef {
  kind: ClipKind;
  id: string;
}

export type SelectionMode = "replace" | "add" | "toggle" | "subtract";

/** The set's element type: `"audio:<id>"` / `"midi:<id>"`. */
export function refKey(ref: ClipRef): string {
  return `${ref.kind}:${ref.id}`;
}

/** Inverse of refKey. Splits on the FIRST colon only, so an id that itself
 * contains a colon survives the round trip. */
export function parseKey(key: string): ClipRef {
  const cut = key.indexOf(":");
  return { kind: key.slice(0, cut) as ClipKind, id: key.slice(cut + 1) };
}

/** Combine `hits` into `current` under `mode`. Never mutates `current`. */
export function applySelection(
  current: Set<string>,
  hits: ClipRef[],
  mode: SelectionMode,
): Set<string> {
  if (mode === "replace") return new Set(hits.map(refKey));
  const next = new Set(current);
  for (const ref of hits) {
    const k = refKey(ref);
    if (mode === "add") next.add(k);
    else if (mode === "subtract") next.delete(k);
    else if (next.has(k)) next.delete(k);
    else next.add(k);
  }
  return next;
}

/** One clip's screen-independent extent, for marquee hit-testing: its lane
 * (track) index and its timeline span in SAMPLES — MIDI clips are converted
 * by the caller through the shipped section table, never re-derived here. */
export interface LaneBox {
  ref: ClipRef;
  laneIndex: number;
  startSamples: number;
  endSamples: number;
}

/** Clips whose half-open span [start, end) overlaps [s0, s1) and whose lane
 * index is within [laneLo, laneHi]. Half-open on purpose: a marquee that
 * starts exactly where a clip ends must not select it, matching
 * `marqueeHits`'s note-level rule in note-ops.ts. */
export function marqueeClipHits(
  boxes: LaneBox[],
  s0: number,
  s1: number,
  laneLo: number,
  laneHi: number,
): ClipRef[] {
  return boxes
    .filter(
      (b) =>
        b.laneIndex >= laneLo &&
        b.laneIndex <= laneHi &&
        b.endSamples > s0 &&
        b.startSamples < s1,
    )
    .map((b) => b.ref);
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `timeout 300 npx vitest run src/lib/utils/clip-selection.test.ts`
Expected: PASS, 11 tests.

- [ ] **Step 5: Run the whole frontend suite and update the dated count**

Run: `timeout 300 npx vitest run`
Expected: 217 passed (206 + 11). Update `README.md:390` to
`npm test                      # 217 frontend unit tests (counted 2026-08-14; vitest): stores + timeline math + section-table bijection`
(keep the rest of the line as it is).

- [ ] **Step 6: Commit**

```bash
git add src/lib/utils/clip-selection.ts src/lib/utils/clip-selection.test.ts README.md
git commit -m "feat(timeline): pure clip-selection algebra (keys, modes, marquee hits)"
```

---

### Task 2: The clip-selection viewer store

The `Set` itself, plus live filtering: a clip removed by an undo, a project
open, or a track delete must fall out of the selection without any explicit
invalidation call, or a later group drag will send `move_clips` entries for
clips the backend no longer has. The store filters against
`project.clips`/`midi.clips` on every read — the same "one authority, read
through it" discipline `view.svelte.ts` uses when it imports `project` for
`samplesPerBeat`.

**Files:**
- Create: `src/lib/state/clip-selection.svelte.ts`
- Test: `src/lib/state/clip-selection.test.ts`

**Interfaces:**
- Consumes: `ClipRef`, `SelectionMode`, `applySelection`, `refKey`,
  `parseKey` from Task 1; `project` (`src/lib/state/project.svelte.ts`) and
  `midi` (`src/lib/state/midi.svelte.ts`).
- Produces: `export const clipSelection` — an instance of
  `ClipSelectionStore` with:
  - `keys: Set<string>` (`$state`; raw, may contain stale keys)
  - `anchor: ClipRef | null` (`$state`; the last-clicked clip — the drag
    anchor and the "focused" clip)
  - `refs(): ClipRef[]` — live, stale-filtered, in `[…audio, …midi]` order
  - `count(): number`
  - `has(ref: ClipRef): boolean` — live-filtered
  - `apply(hits: ClipRef[], mode: SelectionMode): void`
  - `selectOnly(ref: ClipRef): void` — replace + set anchor
  - `clear(): void` — empties the set and nulls the anchor
  - `audioIds(): string[]` / `midiIds(): string[]` — live-filtered id lists

- [ ] **Step 1: Write the failing test**

Create `src/lib/state/clip-selection.test.ts`:

```ts
/**
 * Clip selection is VIEWER state: it is never persisted, never sent to the
 * backend as "the selection", and it must silently drop clips the document
 * no longer has (undo, project open, track delete).
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Clip, MidiClip } from "../types/ipc";

vi.mock("../tauri", () => ({
  backend: {
    on: () => () => {},
    getProjectState: () =>
      Promise.resolve({ ppq: 960, tempoEvents: [{ tick: 0, bpm: 120 }], midiClips: [] }),
  },
}));

const { project } = await import("./project.svelte");
const { midi } = await import("./midi.svelte");
const { clipSelection } = await import("./clip-selection.svelte");

beforeEach(() => {
  project.clips = [
    { id: "a1", trackId: "t1" } as Clip,
    { id: "a2", trackId: "t1" } as Clip,
  ];
  midi.clips = [{ id: "m1", trackId: "t2" } as MidiClip];
  clipSelection.clear();
});

describe("clipSelection", () => {
  it("starts empty", () => {
    expect(clipSelection.count()).toBe(0);
    expect(clipSelection.anchor).toBeNull();
  });

  it("selectOnly replaces the selection and sets the anchor", () => {
    clipSelection.apply([{ kind: "audio", id: "a1" }], "add");
    clipSelection.selectOnly({ kind: "midi", id: "m1" });
    expect(clipSelection.refs()).toEqual([{ kind: "midi", id: "m1" }]);
    expect(clipSelection.anchor).toEqual({ kind: "midi", id: "m1" });
  });

  it("apply('add') accumulates across both stores", () => {
    clipSelection.apply([{ kind: "audio", id: "a1" }], "replace");
    clipSelection.apply([{ kind: "midi", id: "m1" }], "add");
    expect(clipSelection.count()).toBe(2);
    expect(clipSelection.audioIds()).toEqual(["a1"]);
    expect(clipSelection.midiIds()).toEqual(["m1"]);
  });

  it("drops clips the document no longer has, without an explicit prune", () => {
    clipSelection.apply(
      [
        { kind: "audio", id: "a1" },
        { kind: "audio", id: "a2" },
        { kind: "midi", id: "m1" },
      ],
      "replace",
    );
    expect(clipSelection.count()).toBe(3);
    // an undo removes a2 and m1 from the document
    project.clips = [{ id: "a1", trackId: "t1" } as Clip];
    midi.clips = [];
    expect(clipSelection.count()).toBe(1);
    expect(clipSelection.refs()).toEqual([{ kind: "audio", id: "a1" }]);
    expect(clipSelection.has({ kind: "audio", id: "a2" })).toBe(false);
  });

  it("clears the anchor too when the anchored clip disappears", () => {
    clipSelection.selectOnly({ kind: "midi", id: "m1" });
    midi.clips = [];
    expect(clipSelection.anchorLive()).toBeNull();
  });

  it("clear() empties both the set and the anchor", () => {
    clipSelection.selectOnly({ kind: "audio", id: "a1" });
    clipSelection.clear();
    expect(clipSelection.count()).toBe(0);
    expect(clipSelection.anchor).toBeNull();
  });

  it("refs() returns audio clips before midi clips, in document order", () => {
    clipSelection.apply(
      [
        { kind: "midi", id: "m1" },
        { kind: "audio", id: "a2" },
        { kind: "audio", id: "a1" },
      ],
      "replace",
    );
    expect(clipSelection.refs()).toEqual([
      { kind: "audio", id: "a1" },
      { kind: "audio", id: "a2" },
      { kind: "midi", id: "m1" },
    ]);
  });
});
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `timeout 300 npx vitest run src/lib/state/clip-selection.test.ts`
Expected: FAIL — cannot resolve `./clip-selection.svelte`.

- [ ] **Step 3: Write the implementation**

Create `src/lib/state/clip-selection.svelte.ts`:

```ts
/**
 * Timeline multi-selection — VIEWER state, mirroring PR #11's note-level
 * selection model at the clip level. It never lands in the document: no op,
 * no snapshot field, no project.json key, and the backend is never told
 * "what is selected" (clips_copy/clips_paste take explicit id lists).
 *
 * Reads are LIVE-FILTERED against project.clips/midi.clips rather than
 * pruned on an event: an undo, a project open or a track delete can remove a
 * selected clip at any time, and a stale key would otherwise reach
 * `move_clips` as an unknown id and fail the whole batch.
 */

import {
  applySelection,
  refKey,
  type ClipRef,
  type SelectionMode,
} from "../utils/clip-selection";
import { project } from "./project.svelte";
import { midi } from "./midi.svelte";

class ClipSelectionStore {
  /** Raw key set — may contain keys for clips the document has since lost;
   * every read below filters. */
  keys = $state<Set<string>>(new Set());
  /** Last-clicked clip: the group-drag anchor and the "focused" clip. May
   * name a clip that no longer exists — use `anchorLive()` to read it. */
  anchor = $state<ClipRef | null>(null);

  private liveAudio(): Set<string> {
    return new Set(project.clips.map((c) => c.id));
  }
  private liveMidi(): Set<string> {
    return new Set(midi.clips.map((c) => c.id));
  }

  /** The selection, filtered to clips the document still has, audio first
   * then midi, each in document order. */
  refs(): ClipRef[] {
    const out: ClipRef[] = [];
    for (const c of project.clips) {
      if (this.keys.has(refKey({ kind: "audio", id: c.id })))
        out.push({ kind: "audio", id: c.id });
    }
    for (const c of midi.clips) {
      if (this.keys.has(refKey({ kind: "midi", id: c.id })))
        out.push({ kind: "midi", id: c.id });
    }
    return out;
  }

  count(): number {
    return this.refs().length;
  }

  has(ref: ClipRef): boolean {
    if (!this.keys.has(refKey(ref))) return false;
    return ref.kind === "audio" ? this.liveAudio().has(ref.id) : this.liveMidi().has(ref.id);
  }

  audioIds(): string[] {
    return this.refs().filter((r) => r.kind === "audio").map((r) => r.id);
  }
  midiIds(): string[] {
    return this.refs().filter((r) => r.kind === "midi").map((r) => r.id);
  }

  /** The anchor, or null when the anchored clip is gone. */
  anchorLive(): ClipRef | null {
    const a = this.anchor;
    if (!a) return null;
    return this.has(a) ? a : null;
  }

  apply(hits: ClipRef[], mode: SelectionMode) {
    this.keys = applySelection(this.keys, hits, mode);
    if (hits.length > 0 && mode !== "subtract") this.anchor = hits[hits.length - 1];
  }

  selectOnly(ref: ClipRef) {
    this.keys = applySelection(this.keys, [ref], "replace");
    this.anchor = ref;
  }

  clear() {
    this.keys = new Set();
    this.anchor = null;
  }
}

export const clipSelection = new ClipSelectionStore();
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `timeout 300 npx vitest run src/lib/state/clip-selection.test.ts`
Expected: PASS, 7 tests.

- [ ] **Step 5: Run the whole frontend suite and update the dated count**

Run: `timeout 300 npx vitest run`
Expected: 224 passed. Update the count on `README.md:390` to 224.

- [ ] **Step 6: Commit**

```bash
git add src/lib/state/clip-selection.svelte.ts src/lib/state/clip-selection.test.ts README.md
git commit -m "feat(timeline): clip-selection viewer store with live stale-key filtering"
```

---

### Task 3: Click / shift-click / ctrl-click selection on clips

Wire the two clip components to the new store. The visual `selected` class
now derives from the multi-selection; the legacy single-selection fields are
kept in step as the "focused clip" (scope ruling F), so SPLIT STEMS, the
Dock and AI-Studio targeting keep working unchanged.

**Files:**
- Modify: `src/lib/components/ClipView.svelte` (:27 `selected`, :90-103
  `onPointerDown`)
- Modify: `src/lib/components/MidiClipView.svelte` (:73 `selected`, :143-161
  `onPointerDown`)
- Create: `src/lib/utils/selection-modifiers.ts`
- Test: `src/lib/utils/selection-modifiers.test.ts`

**Interfaces:**
- Consumes: `clipSelection` (Task 2), `ClipRef`/`SelectionMode` (Task 1).
- Produces:
  `selectionModeFor(e: { shiftKey: boolean; ctrlKey: boolean; metaKey: boolean }): SelectionMode`
  — the ONE place the modifier convention is written down, so the two clip
  components and the marquee (Task 4) cannot drift.

The convention (Ableton/Reaper-compatible, and the same one the piano roll
already implies): **plain = replace, Shift = add, Ctrl/Cmd = toggle, Shift +
Ctrl/Cmd = subtract.**

- [ ] **Step 1: Write the failing test**

Create `src/lib/utils/selection-modifiers.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { selectionModeFor } from "./selection-modifiers";

const ev = (shiftKey = false, ctrlKey = false, metaKey = false) => ({
  shiftKey,
  ctrlKey,
  metaKey,
});

describe("selectionModeFor", () => {
  it("plain click replaces", () => {
    expect(selectionModeFor(ev())).toBe("replace");
  });
  it("shift adds", () => {
    expect(selectionModeFor(ev(true))).toBe("add");
  });
  it("ctrl toggles", () => {
    expect(selectionModeFor(ev(false, true))).toBe("toggle");
  });
  it("meta toggles too (macOS)", () => {
    expect(selectionModeFor(ev(false, false, true))).toBe("toggle");
  });
  it("shift+ctrl subtracts", () => {
    expect(selectionModeFor(ev(true, true))).toBe("subtract");
  });
  it("shift+meta subtracts too", () => {
    expect(selectionModeFor(ev(true, false, true))).toBe("subtract");
  });
});
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `timeout 300 npx vitest run src/lib/utils/selection-modifiers.test.ts`
Expected: FAIL — cannot resolve `./selection-modifiers`.

- [ ] **Step 3: Write the implementation**

Create `src/lib/utils/selection-modifiers.ts`:

```ts
/**
 * The ONE place the timeline's selection-modifier convention is written
 * down, so the clip components and the lane marquee cannot drift apart:
 * plain = replace, Shift = add, Ctrl/Cmd = toggle, Shift+Ctrl/Cmd = subtract.
 */
import type { SelectionMode } from "./clip-selection";

export function selectionModeFor(e: {
  shiftKey: boolean;
  ctrlKey: boolean;
  metaKey: boolean;
}): SelectionMode {
  const mod = e.ctrlKey || e.metaKey;
  if (e.shiftKey && mod) return "subtract";
  if (mod) return "toggle";
  if (e.shiftKey) return "add";
  return "replace";
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `timeout 300 npx vitest run src/lib/utils/selection-modifiers.test.ts`
Expected: PASS, 6 tests.

- [ ] **Step 5: Wire `ClipView.svelte`**

Add to the import block (after the `ui` import):

```ts
  import { clipSelection } from "../state/clip-selection.svelte";
  import { selectionModeFor } from "../utils/selection-modifiers";
```

Replace the `selected` derivation (currently line 27):

```ts
  const selected = $derived(clipSelection.has({ kind: "audio", id: clip.id }));
```

Replace the body of `onPointerDown` up to and including `project.select(...)`
(currently lines 90-93) with:

```ts
  function onPointerDown(e: PointerEvent) {
    if (e.button !== 0) return;
    if ((e.target as HTMLElement).closest("button")) return;
    const ref = { kind: "audio", id: clip.id } as const;
    const mode = selectionModeFor(e);
    // A plain click on an ALREADY-selected clip must not collapse the
    // selection — that is what makes dragging a group possible at all.
    if (!(mode === "replace" && clipSelection.has(ref))) {
      clipSelection.apply([ref], mode);
    } else {
      clipSelection.anchor = ref;
    }
    // Focused clip (scope ruling F): SPLIT STEMS, the Dock and AI-Studio
    // targeting still act on exactly one clip.
    project.select(clip.id);
```

(the rest of `onPointerDown` — `dragging = true` onwards — is unchanged for
now; Task 6 replaces it.)

- [ ] **Step 6: Wire `MidiClipView.svelte`**

Add to the import block:

```ts
  import { clipSelection } from "../state/clip-selection.svelte";
  import { selectionModeFor } from "../utils/selection-modifiers";
```

Replace the `selected` derivation (currently line 73):

```ts
  const selected = $derived(clipSelection.has({ kind: "midi", id: clip.id }));
```

Replace lines 145-146 (`midi.select(clip.id); project.select(null);`) with:

```ts
    const ref = { kind: "midi", id: clip.id } as const;
    const mode = selectionModeFor(e);
    if (!(mode === "replace" && clipSelection.has(ref))) {
      clipSelection.apply([ref], mode);
    } else {
      clipSelection.anchor = ref;
    }
    midi.select(clip.id);
    project.select(null);
```

- [ ] **Step 7: Type-check and run the whole frontend suite**

Run: `npx svelte-check --threshold error`
Expected: no errors introduced by these files.

Run: `timeout 300 npx vitest run`
Expected: 230 passed (224 + 6). Update `README.md:390` to 230.

- [ ] **Step 8: Commit**

```bash
git add src/lib/utils/selection-modifiers.ts src/lib/utils/selection-modifiers.test.ts \
        src/lib/components/ClipView.svelte src/lib/components/MidiClipView.svelte README.md
git commit -m "feat(timeline): shift/ctrl-click multi-select on audio and MIDI clips"
```

---

### Task 4: Marquee selection on the lanes

Rubber-band selection over the lane area. Two things this task must get
right, both of which have bitten this repo before:

1. **Pointer maths go through `canvasPos`** (`src/lib/utils/canvas-pos.ts`)
   — under interface zoom (PR #10's `aura.prefs` `uiZoom`), raw
   `clientX - rect.left` drifts. The memory note "canvasPos/zoom-pekerbug"
   records that `Timeline`'s own pointer handling still has this bug; this
   task fixes it for the code it adds and for `loopSampleAt`/`rulerSeek`
   while it is in the file.
2. **The marquee must not fight the clip drag.** It starts only on a
   `pointerdown` whose target is the lane background (`.lane`, `.lanes`, or
   the grid canvas) — never a `.clip`/`.mclip`.

**Files:**
- Modify: `src/lib/components/Timeline.svelte` (the `.lanes` div, ~:455-511;
  `loopSampleAt` ~:270; `rulerSeek` ~:190)
- Create: `src/lib/utils/lane-geometry.ts`
- Test: `src/lib/utils/lane-geometry.test.ts`

**Interfaces:**
- Consumes: `LaneBox`, `marqueeClipHits`, `applySelection` (Task 1),
  `selectionModeFor` (Task 3), `clipSelection` (Task 2), `canvasPos`.
- Produces:
  - `TRACK_HEIGHT_PX = 88` — the single source for the lane height the grid
    canvas and the marquee both need (today `88` is a magic number at
    `Timeline.svelte:113` while the CSS uses `var(--track-height)`).
  - `laneIndexAt(y: number, trackCount: number): number` — clamped.
  - `buildLaneBoxes(args): LaneBox[]` — one box per clip, MIDI ticks already
    converted to samples by the caller-supplied `ticksToSamples`.

- [ ] **Step 1: Write the failing test**

Create `src/lib/utils/lane-geometry.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { buildLaneBoxes, laneIndexAt, TRACK_HEIGHT_PX } from "./lane-geometry";

describe("laneIndexAt", () => {
  it("maps y to the lane it falls in", () => {
    expect(laneIndexAt(0, 3)).toBe(0);
    expect(laneIndexAt(TRACK_HEIGHT_PX - 1, 3)).toBe(0);
    expect(laneIndexAt(TRACK_HEIGHT_PX, 3)).toBe(1);
    expect(laneIndexAt(TRACK_HEIGHT_PX * 2 + 4, 3)).toBe(2);
  });

  it("clamps below zero and above the last track", () => {
    expect(laneIndexAt(-500, 3)).toBe(0);
    expect(laneIndexAt(TRACK_HEIGHT_PX * 99, 3)).toBe(2);
  });

  it("returns 0 for an empty arrangement rather than -1", () => {
    expect(laneIndexAt(0, 0)).toBe(0);
  });
});

describe("buildLaneBoxes", () => {
  const trackIds = ["t1", "t2"];
  const audioClips = [
    { id: "a1", trackId: "t1", timelineStartSamples: 100, lengthSamples: 400 },
    { id: "a2", trackId: "zz", timelineStartSamples: 0, lengthSamples: 10 },
  ];
  const midiClips = [{ id: "m1", trackId: "t2", timelineStartTicks: 960, lengthTicks: 1920 }];
  // a flat 1 tick = 10 samples map, enough to prove the conversion is used
  const ticksToSamples = (t: number) => t * 10;

  it("places each clip on its track's lane index with a sample span", () => {
    const boxes = buildLaneBoxes({ trackIds, audioClips, midiClips, ticksToSamples });
    expect(boxes).toEqual([
      { ref: { kind: "audio", id: "a1" }, laneIndex: 0, startSamples: 100, endSamples: 500 },
      { ref: { kind: "midi", id: "m1" }, laneIndex: 1, startSamples: 9600, endSamples: 28800 },
    ]);
  });

  it("drops clips whose track is not on the timeline", () => {
    const boxes = buildLaneBoxes({ trackIds, audioClips, midiClips, ticksToSamples });
    expect(boxes.some((b) => b.ref.id === "a2")).toBe(false);
  });

  it("converts a MIDI clip's END through the map, not by adding a converted length", () => {
    // a non-linear map: doubling after tick 1000
    const nonLinear = (t: number) => (t <= 1000 ? t : 1000 + (t - 1000) * 2);
    const boxes = buildLaneBoxes({
      trackIds,
      audioClips: [],
      midiClips: [{ id: "m1", trackId: "t2", timelineStartTicks: 900, lengthTicks: 200 }],
      ticksToSamples: nonLinear,
    });
    expect(boxes[0].startSamples).toBe(900);
    expect(boxes[0].endSamples).toBe(1200); // f(1100), not 900 + f(200)
  });
});
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `timeout 300 npx vitest run src/lib/utils/lane-geometry.test.ts`
Expected: FAIL — cannot resolve `./lane-geometry`.

- [ ] **Step 3: Write the implementation**

Create `src/lib/utils/lane-geometry.ts`:

```ts
/**
 * Lane geometry for the arrangement timeline: the lane height, y -> lane
 * index, and the per-clip sample-space boxes the marquee hit-tests against.
 * Pure, so the marquee's maths test without a DOM.
 */
import type { LaneBox } from "./clip-selection";

/** One lane's height in LAYOUT px. Mirrors `--track-height` in the app's
 * CSS and the grid canvas's own `trackH`; kept here so the marquee and the
 * grid cannot disagree. */
export const TRACK_HEIGHT_PX = 88;

/** Which lane a lane-area y coordinate falls in, clamped to the tracks that
 * exist (0 when there are none). */
export function laneIndexAt(y: number, trackCount: number): number {
  const raw = Math.floor(y / TRACK_HEIGHT_PX);
  return Math.max(0, Math.min(raw, Math.max(0, trackCount - 1)));
}

export interface BuildLaneBoxesArgs {
  /** Track ids in timeline order — the index into this array IS the lane. */
  trackIds: string[];
  audioClips: { id: string; trackId: string; timelineStartSamples: number; lengthSamples: number }[];
  midiClips: { id: string; trackId: string; timelineStartTicks: number; lengthTicks: number }[];
  /** The shipped section-table bijection (midi.ticksToSamples). */
  ticksToSamples: (ticks: number) => number;
}

/** One box per clip whose track is on the timeline. A MIDI clip's END is the
 * conversion of `start + length` ticks, NOT the start plus a converted
 * length — the latter drifts under a non-constant tempo map (the same bug
 * App.svelte's edge-jump already fixed). */
export function buildLaneBoxes(args: BuildLaneBoxesArgs): LaneBox[] {
  const lane = new Map(args.trackIds.map((id, i) => [id, i]));
  const boxes: LaneBox[] = [];
  for (const c of args.audioClips) {
    const i = lane.get(c.trackId);
    if (i === undefined) continue;
    boxes.push({
      ref: { kind: "audio", id: c.id },
      laneIndex: i,
      startSamples: c.timelineStartSamples,
      endSamples: c.timelineStartSamples + c.lengthSamples,
    });
  }
  for (const c of args.midiClips) {
    const i = lane.get(c.trackId);
    if (i === undefined) continue;
    boxes.push({
      ref: { kind: "midi", id: c.id },
      laneIndex: i,
      startSamples: args.ticksToSamples(c.timelineStartTicks),
      endSamples: args.ticksToSamples(c.timelineStartTicks + c.lengthTicks),
    });
  }
  return boxes;
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `timeout 300 npx vitest run src/lib/utils/lane-geometry.test.ts`
Expected: PASS, 6 tests.

- [ ] **Step 5: Wire the marquee into `Timeline.svelte`**

Add to the import block:

```ts
  import { clipSelection } from "../state/clip-selection.svelte";
  import { canvasPos } from "../utils/canvas-pos";
  import { applySelection, marqueeClipHits } from "../utils/clip-selection";
  import { selectionModeFor } from "../utils/selection-modifiers";
  import { buildLaneBoxes, laneIndexAt } from "../utils/lane-geometry";
```

Add this block after the `onLaneDblClick` function:

```ts
  // ── marquee selection over the lane area ──
  // Starts only on the lane BACKGROUND, so it never competes with a clip
  // drag. Pointer maths go through canvasPos: under interface zoom
  // (prefs.uiZoom) raw clientX/rect maths drift down-right.

  let marquee: {
    x0: number; y0: number; x1: number; y1: number;
    baseKeys: Set<string>;
    mode: ReturnType<typeof selectionModeFor>;
  } | null = $state(null);

  const marqueeRect = $derived(
    marquee
      ? {
          left: Math.min(marquee.x0, marquee.x1),
          top: Math.min(marquee.y0, marquee.y1),
          width: Math.abs(marquee.x1 - marquee.x0),
          height: Math.abs(marquee.y1 - marquee.y0),
        }
      : null,
  );

  function onLanesPointerDown(e: PointerEvent) {
    if (e.button !== 0) return;
    if ((e.target as HTMLElement).closest(".clip, .mclip, button")) return;
    if (!lanesEl) return;
    const p = canvasPos(lanesEl, e.clientX, e.clientY);
    const mode = selectionModeFor(e);
    marquee = { x0: p.x, y0: p.y, x1: p.x, y1: p.y, baseKeys: new Set(clipSelection.keys), mode };
    if (mode === "replace") clipSelection.clear();
    try {
      (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    } catch {
      /* synthetic pointer */
    }
  }

  function onLanesPointerMove(e: PointerEvent) {
    if (!marquee || !lanesEl) return;
    const p = canvasPos(lanesEl, e.clientX, e.clientY);
    marquee = { ...marquee, x1: p.x, y1: p.y };
    const s0 = view.samplesAt(Math.min(marquee.x0, marquee.x1));
    const s1 = view.samplesAt(Math.max(marquee.x0, marquee.x1));
    const n = project.tracks.length;
    const laneLo = laneIndexAt(Math.min(marquee.y0, marquee.y1), n);
    const laneHi = laneIndexAt(Math.max(marquee.y0, marquee.y1), n);
    const boxes = buildLaneBoxes({
      trackIds: project.tracks.map((t) => t.id),
      audioClips: project.clips,
      midiClips: midi.clips,
      ticksToSamples: (t) => midi.ticksToSamples(t),
    });
    const hits = marqueeClipHits(boxes, s0, s1, laneLo, laneHi);
    // Recomputed from the drag's STARTING selection every move, so dragging
    // the band back over a clip un-hits it instead of latching.
    clipSelection.keys = applySelection(marquee.baseKeys, hits, marquee.mode);
  }

  function onLanesPointerUp(e: PointerEvent) {
    marquee = null;
    try {
      (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    } catch {
      /* not captured */
    }
  }
```

On the `.lanes` div (currently `<div class="lanes" id="timeline-lanes" bind:this={lanesEl}>`)
add:

```
      role="presentation"
      onpointerdown={onLanesPointerDown}
      onpointermove={onLanesPointerMove}
      onpointerup={onLanesPointerUp}
      onpointercancel={onLanesPointerUp}
```

and render the band just before `<div bind:this={playheadEl} class="playhead"></div>`:

```svelte
      {#if marqueeRect && marqueeRect.width > 2}
        <div
          class="marquee"
          style:left="{marqueeRect.left}px"
          style:top="{marqueeRect.top}px"
          style:width="{marqueeRect.width}px"
          style:height="{marqueeRect.height}px"
        ></div>
      {/if}
```

with this CSS added to the component's `<style>` block:

```css
  .marquee {
    position: absolute;
    background: rgba(82, 229, 255, 0.08);
    border: 1px solid var(--cyan-dim);
    pointer-events: none;
    z-index: 3;
  }
```

- [ ] **Step 6: Fix the two pre-existing zoom-pointer bugs in the same file**

`rulerSeek` (~:190) and `loopSampleAt` (~:270) both do raw
`e.clientX - rect.left`. Replace with `canvasPos`:

```ts
  function rulerSeek(e: PointerEvent) {
    const el = e.currentTarget as HTMLElement;
    let samples = view.samplesAt(canvasPos(el, e.clientX, e.clientY).x);
    samples = view.snapSamples(Math.max(0, samples));
    void transport.seek(samples);
  }
```

```ts
  function loopSampleAt(e: PointerEvent): number {
    const el = rulerEl;
    if (!el) return 0;
    return Math.max(0, view.snapSamples(view.samplesAt(canvasPos(el, e.clientX, e.clientY).x)));
  }
```

Also `onRulerDown`'s strip test (`e.clientY - rect.top < rect.height * 0.5`)
becomes `canvasPos(el, e.clientX, e.clientY).y < el.clientHeight * 0.5`, and
`onWheel`'s `e.clientX - rect.left` becomes
`canvasPos(lanesEl ?? (e.currentTarget as HTMLElement), e.clientX, e.clientY).x`.
Record in the commit body that this closes the timeline half of the
canvasPos/zoom pointer bug PR #11 fixed for the piano roll.

- [ ] **Step 7: Type-check and run the whole frontend suite**

Run: `npx svelte-check --threshold error`
Expected: no new errors.

Run: `timeout 300 npx vitest run`
Expected: 236 passed. Update `README.md:390` to 236.

- [ ] **Step 8: Commit**

```bash
git add src/lib/utils/lane-geometry.ts src/lib/utils/lane-geometry.test.ts \
        src/lib/components/Timeline.svelte README.md
git commit -m "feat(timeline): marquee multi-select over lanes; route timeline pointer maths through canvasPos"
```

---

### Task 5: Backend `move_clips` — one batch, one undo entry, gesture-aware

The additive batch counterpart of `move_clip`. Audio and MIDI clips mix
freely in ONE transaction because the channel is cross-store atomic (both
`store.clips` and `midi.clips` live under the one session lock — round-2
§4.1), which is exactly the property `Op::TempoSet` was built on.

**This method's shape is `set_track_mix`'s, verbatim**
(`src-tauri/src/control/mod.rs:1504-1565`): validate every id first, then
try `GestureState::commit_transient_and_fold`, falling back to a plain
`commit` when no matching gesture is open. Do not re-derive the lock order —
copy the shape. `CoalesceKey::for_op` already keys `Op::Set` by
`(object, path)`, so a `Set{Clip, TimelineStartSamples}` and a
`Set{MidiClip, TimelineStartTicks}` fold into a gesture under distinct keys
with no change to the folding code at all.

**Files:**
- Modify: `src-tauri/src/control/mod.rs` — add `ClipPlacement` next to
  `TrackMixChange`, `ControlPlane::move_clips` next to
  `ControlPlane::move_clip` (~:1573), and the `#[tauri::command] move_clips`
  next to `move_clip` (~:2822)
- Modify: `src-tauri/src/lib.rs` — register `control::move_clips` in
  `generate_handler!`, immediately after `control::move_clip`
- Test: `src-tauri/src/control/mod.rs`'s `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: `ControlPlane::commit` / `commit_with`, `GestureState::
  commit_transient_and_fold`, `op::Op::Set`, `op::PropPath::{TimelineStartSamples,
  TimelineStartTicks, LengthTicks, ContentLengthTicks}`, `op::ObjectRef::{Clip,
  MidiClip}`.
- Produces (Task 6 and the TS adapter consume exactly this wire shape):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ClipPlacement {
    Audio { clip_id: String, timeline_start_samples: u64 },
    Midi {
        clip_id: String,
        timeline_start_ticks: u64,
        #[serde(default)] length_ticks: Option<u64>,
        #[serde(default)] content_length_ticks: Option<u64>,
    },
}
```
  and `pub fn move_clips(&self, placements: Vec<ClipPlacement>, meta: op::TxMeta) -> Result<(), String>`
  plus `#[tauri::command] pub fn move_clips(placements: Vec<ClipPlacement>, control: State<'_, Arc<ControlPlane>>) -> Result<(), String>`.

  Wire form (what the frontend sends):
  `{"kind":"audio","clipId":"…","timelineStartSamples":48000}` /
  `{"kind":"midi","clipId":"…","timelineStartTicks":960,"lengthTicks":1920}`.

- [ ] **Step 1: Write the failing tests**

Add to `src-tauri/src/control/mod.rs`'s test module. (`test_plane_with_tracks`,
`dummy_midi_clip` and `test_clip` are the existing helpers in that module;
reuse them, do not invent new ones.)

```rust
    /// The batch's whole reason to exist: audio and MIDI clips move in ONE
    /// transaction (cross-store atomicity, round-2 §4.1) and produce exactly
    /// ONE undo entry — not one per clip.
    #[test]
    fn move_clips_moves_audio_and_midi_in_one_transaction_and_one_history_entry() {
        let (cp, _rx, _events) = test_plane_with_tracks(&["t-1"]);
        cp.commit(TxMeta::user("seed"), |tx| {
            tx.apply(Op::ClipAdd { clip: test_clip("a-1", "t-1"), index: 0 })?;
            tx.apply(Op::ClipAdd { clip: test_clip("a-2", "t-1"), index: 1 })?;
            tx.apply(Op::MidiClipAdd { clip: dummy_midi_clip("t-1"), index: 0 })
        })
        .unwrap();
        let midi_id = cp.session().lock().midi.clips[0].id.to_string();
        let (undo_before, _) = cp.history_depths();

        cp.move_clips(
            vec![
                ClipPlacement::Audio { clip_id: "a-1".into(), timeline_start_samples: 1_000 },
                ClipPlacement::Audio { clip_id: "a-2".into(), timeline_start_samples: 2_000 },
                ClipPlacement::Midi {
                    clip_id: midi_id.clone(),
                    timeline_start_ticks: 480,
                    length_ticks: None,
                    content_length_ticks: None,
                },
            ],
            TxMeta::user("move clips"),
        )
        .unwrap();

        {
            let s = cp.session().lock();
            assert_eq!(s.store.clips[0].timeline_start_samples, 1_000);
            assert_eq!(s.store.clips[1].timeline_start_samples, 2_000);
            assert_eq!(s.midi.clips[0].timeline_start_ticks, 480);
        }
        let (undo_after, _) = cp.history_depths();
        assert_eq!(undo_after, undo_before + 1, "a group move is ONE undo step");
    }

    /// One Ctrl+Z puts every clip in the batch back.
    #[test]
    fn move_clips_undo_restores_every_clip_in_the_batch() {
        let (cp, _rx, _events) = test_plane_with_tracks(&["t-1"]);
        cp.commit(TxMeta::user("seed"), |tx| {
            tx.apply(Op::ClipAdd { clip: test_clip("a-1", "t-1"), index: 0 })?;
            tx.apply(Op::ClipAdd { clip: test_clip("a-2", "t-1"), index: 1 })
        })
        .unwrap();
        cp.move_clips(
            vec![
                ClipPlacement::Audio { clip_id: "a-1".into(), timeline_start_samples: 7_000 },
                ClipPlacement::Audio { clip_id: "a-2".into(), timeline_start_samples: 9_000 },
            ],
            TxMeta::user("move clips"),
        )
        .unwrap();
        cp.undo().unwrap();
        let s = cp.session().lock();
        assert_eq!(s.store.clips[0].timeline_start_samples, 0);
        assert_eq!(s.store.clips[1].timeline_start_samples, 0);
    }

    /// Validate-before-mutate (the `transact`-must-not-panic rule's sibling):
    /// one bad id fails the WHOLE batch, and nothing moved.
    #[test]
    fn move_clips_rejects_an_unknown_id_without_moving_anything() {
        let (cp, _rx, _events) = test_plane_with_tracks(&["t-1"]);
        cp.commit(TxMeta::user("seed"), |tx| {
            tx.apply(Op::ClipAdd { clip: test_clip("a-1", "t-1"), index: 0 })
        })
        .unwrap();
        let (undo_before, _) = cp.history_depths();

        let err = cp
            .move_clips(
                vec![
                    ClipPlacement::Audio { clip_id: "a-1".into(), timeline_start_samples: 5_000 },
                    ClipPlacement::Audio { clip_id: "nope".into(), timeline_start_samples: 5_000 },
                ],
                TxMeta::user("move clips"),
            )
            .unwrap_err();
        assert!(err.contains("nope"), "the error names the offending id: {err}");
        assert_eq!(cp.session().lock().store.clips[0].timeline_start_samples, 0);
        assert_eq!(cp.history_depths().0, undo_before, "a rejected batch is not a history step");
    }

    /// Inside an open gesture the batch runs TRANSIENT and folds; the ONE
    /// history entry is synthesized by `gesture_end`, and it carries the LAST
    /// position per clip, not the first.
    #[test]
    fn move_clips_folds_into_an_open_gesture_as_one_entry_with_the_last_position() {
        let (cp, _rx, _events) = test_plane_with_tracks(&["t-1"]);
        cp.commit(TxMeta::user("seed"), |tx| {
            tx.apply(Op::ClipAdd { clip: test_clip("a-1", "t-1"), index: 0 })
        })
        .unwrap();
        let (undo_before, _) = cp.history_depths();

        cp.gesture_begin("move clips".into()).unwrap();
        cp.move_clips(
            vec![ClipPlacement::Audio { clip_id: "a-1".into(), timeline_start_samples: 100 }],
            TxMeta::user("move clips"),
        )
        .unwrap();
        cp.move_clips(
            vec![ClipPlacement::Audio { clip_id: "a-1".into(), timeline_start_samples: 900 }],
            TxMeta::user("move clips"),
        )
        .unwrap();
        assert_eq!(cp.history_depths().0, undo_before, "mid-gesture commits are transient");
        cp.gesture_end().unwrap();

        assert_eq!(cp.history_depths().0, undo_before + 1, "the whole drag is ONE step");
        let batch = cp.take_last_gesture_batch().expect("gesture synthesized a batch");
        assert_eq!(batch.ops.len(), 1, "folded to one net Set: {:?}", batch.ops);
        assert!(
            matches!(&batch.ops[0], Op::Set { to, .. } if to == &serde_json::json!(900u64)),
            "the gesture batch carries the LAST position: {:?}",
            batch.ops[0]
        );
        assert_eq!(cp.session().lock().store.clips[0].timeline_start_samples, 900);
    }

    /// A MIDI entry may carry bounds too (the group loop-length adjust,
    /// Task 7) — and `contentLengthTicks: None` means "unchanged", never
    /// "clear" (scope ruling H).
    #[test]
    fn move_clips_midi_entry_sets_bounds_and_leaves_content_length_alone_when_absent() {
        let (cp, _rx, _events) = test_plane_with_tracks(&["t-1"]);
        cp.commit(TxMeta::user("seed"), |tx| {
            let mut c = dummy_midi_clip("t-1");
            c.content_length_ticks = Some(1_920);
            tx.apply(Op::MidiClipAdd { clip: c, index: 0 })
        })
        .unwrap();
        let id = cp.session().lock().midi.clips[0].id.to_string();

        cp.move_clips(
            vec![ClipPlacement::Midi {
                clip_id: id,
                timeline_start_ticks: 960,
                length_ticks: Some(7_680),
                content_length_ticks: None,
            }],
            TxMeta::user("resize clips"),
        )
        .unwrap();

        let s = cp.session().lock();
        assert_eq!(s.midi.clips[0].timeline_start_ticks, 960);
        assert_eq!(s.midi.clips[0].length_ticks, 7_680);
        assert_eq!(
            s.midi.clips[0].content_length_ticks,
            Some(1_920),
            "an absent contentLengthTicks means UNCHANGED, not cleared"
        );
    }

    /// An empty batch is a no-op, not an error and not a history step — the
    /// same "nothing to do is not a failure" rule set_mute/set_solo follow.
    #[test]
    fn move_clips_with_no_placements_is_a_no_op() {
        let (cp, _rx, _events) = test_plane_with_tracks(&["t-1"]);
        let (undo_before, _) = cp.history_depths();
        cp.move_clips(vec![], TxMeta::user("move clips")).unwrap();
        assert_eq!(cp.history_depths().0, undo_before);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml move_clips`
Expected: FAIL — `cannot find type ClipPlacement`, `no method named move_clips`.

- [ ] **Step 3: Add the `ClipPlacement` type**

In `src-tauri/src/control/mod.rs`, next to `ImportClipRequest` (~:75):

```rust
/// One clip's new placement in a `move_clips` batch. Externally tagged by
/// `kind` because audio clips are placed in SAMPLES and MIDI clips in TICKS
/// — two different units that must not be confusable on the wire, and two
/// different stores (`store.clips` / `midi.clips`) resolved by two different
/// lookups. A batch mixes both freely; the channel is cross-store atomic.
///
/// `length_ticks` / `content_length_ticks` are additive `#[serde(default)]`
/// fields carrying the group loop-length adjust. `None` means UNCHANGED, not
/// "clear" — clearing `content_length_ticks` back to "same as length" keeps
/// going through the existing `midi_set_clip_bounds(…, null)` command
/// (plan scope ruling H).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ClipPlacement {
    Audio { clip_id: String, timeline_start_samples: u64 },
    Midi {
        clip_id: String,
        timeline_start_ticks: u64,
        #[serde(default)]
        length_ticks: Option<u64>,
        #[serde(default)]
        content_length_ticks: Option<u64>,
    },
}

impl ClipPlacement {
    fn clip_id(&self) -> &str {
        match self {
            Self::Audio { clip_id, .. } | Self::Midi { clip_id, .. } => clip_id,
        }
    }
}
```

- [ ] **Step 4: Add `ControlPlane::move_clips`**

Directly after `ControlPlane::move_clip` (~:1591):

```rust
    /// Move (and, for MIDI, optionally resize) a BATCH of clips in ONE
    /// transaction — the group-drag counterpart of [`Self::move_clip`].
    /// Audio and MIDI clips mix freely: both stores live under the one
    /// session lock (round-2 §4.1's cross-store atomicity), so a mixed
    /// selection is one atomic batch and ONE undo entry.
    ///
    /// GESTURE-AWARE, and deliberately the same shape as
    /// [`Self::set_track_mix`]: when a gesture matching this actor is open,
    /// the batch commits TRANSIENT and folds into the gesture's accumulator
    /// through `GestureState::commit_transient_and_fold` — which holds the
    /// gesture mutex across the nested session-lock acquisition, the ONE
    /// safe nesting direction (Task 14, fix round 1 Finding 2). Do not
    /// reorder those locks. With no gesture open it falls back to a plain
    /// `commit`, which is still exactly one history entry.
    ///
    /// Every id is validated BEFORE any op is applied: a batch with one bad
    /// id fails whole, having moved nothing (`Session::transact` would roll
    /// back the applied ops, but pre-checking makes the failure atomic at
    /// the batch level and keeps the error message about the id the caller
    /// got wrong).
    pub fn move_clips(
        &self,
        placements: Vec<ClipPlacement>,
        meta: op::TxMeta,
    ) -> Result<(), String> {
        if placements.is_empty() {
            return Ok(());
        }
        {
            let session = self.session.lock();
            for p in &placements {
                let known = match p {
                    ClipPlacement::Audio { clip_id, .. } => {
                        session.store.clips.iter().any(|c| c.id == *clip_id)
                    }
                    ClipPlacement::Midi { clip_id, .. } => {
                        session.midi.clips.iter().any(|c| c.id == *clip_id)
                    }
                };
                if !known {
                    return Err(format!("unknown clip: {}", p.clip_id()));
                }
            }
        }
        let gesture_meta = meta.clone();
        let placements_for_gesture = placements.clone();
        let gesture_result = self.gesture.commit_transient_and_fold(&meta.actor, || {
            self.commit_with(
                gesture_meta.transient(),
                |tx| apply_clip_placements(&placements_for_gesture, tx),
                false,
            )
        });
        match gesture_result {
            Some(result) => {
                result?;
            }
            None => {
                self.commit(meta, |tx| apply_clip_placements(&placements, tx))?;
            }
        }
        Ok(())
    }
```

and the free helper next to `apply_mix_changes` (same module):

```rust
/// Apply one `move_clips` batch inside an open transaction. Pure op
/// emission — every id was validated by the caller before the lock, so this
/// never needs to reject anything and never panics.
fn apply_clip_placements(
    placements: &[ClipPlacement],
    tx: &mut session::Tx<'_>,
) -> Result<(), String> {
    for p in placements {
        match p {
            ClipPlacement::Audio { clip_id, timeline_start_samples } => {
                tx.apply(op::Op::Set {
                    object: op::ObjectRef::Clip(clip_id.as_str().into()),
                    path: op::PropPath::TimelineStartSamples,
                    from: serde_json::Value::Null,
                    to: serde_json::json!(timeline_start_samples),
                })?;
            }
            ClipPlacement::Midi {
                clip_id,
                timeline_start_ticks,
                length_ticks,
                content_length_ticks,
            } => {
                tx.apply(op::Op::Set {
                    object: op::ObjectRef::MidiClip(clip_id.as_str().into()),
                    path: op::PropPath::TimelineStartTicks,
                    from: serde_json::Value::Null,
                    to: serde_json::json!(timeline_start_ticks),
                })?;
                if let Some(len) = length_ticks {
                    tx.apply(op::Op::Set {
                        object: op::ObjectRef::MidiClip(clip_id.as_str().into()),
                        path: op::PropPath::LengthTicks,
                        from: serde_json::Value::Null,
                        to: serde_json::json!(len),
                    })?;
                }
                if let Some(cl) = content_length_ticks {
                    tx.apply(op::Op::Set {
                        object: op::ObjectRef::MidiClip(clip_id.as_str().into()),
                        path: op::PropPath::ContentLengthTicks,
                        from: serde_json::Value::Null,
                        to: serde_json::json!(cl),
                    })?;
                }
            }
        }
    }
    Ok(())
}
```

- [ ] **Step 5: Add the Tauri command and register it**

In `src-tauri/src/control/mod.rs`, directly after `#[tauri::command] pub fn move_clip`
(~:2833):

```rust
/// Move (and optionally resize) a BATCH of clips in one transaction — thin
/// delegate over [`ControlPlane::move_clips`]. The frontend previews a group
/// drag locally and calls this ONCE at gesture end, inside a
/// `gesture_begin`/`gesture_end` boundary, so the whole drag is one undo
/// step. Additive command.
#[tauri::command]
pub fn move_clips(
    placements: Vec<ClipPlacement>,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<(), String> {
    control.move_clips(placements, op::TxMeta::user("move clips"))
}
```

In `src-tauri/src/lib.rs`'s `generate_handler!`, add `control::move_clips,`
on the line after `control::move_clip,`.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml move_clips`
Expected: PASS, 6 tests.

- [ ] **Step 7: Run the whole backend suite and update the dated counts**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 533 total (527 + 6), all green. Update BOTH stale lines — they say
506 and predate PR #17 (see Global Constraints):
- `README.md:389` → `cd src-tauri && cargo test    # 533 tests (counted 2026-08-14): …`
- `CONTRIBUTING.md:62` → `cd src-tauri && cargo test   # 533 tests (counted 2026-08-14)`

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/control/mod.rs src-tauri/src/lib.rs README.md CONTRIBUTING.md
git commit -m "feat(control): additive move_clips batch command — one gesture-folded transaction for a group drag"
```

---

### Task 6: Frontend group drag

One controller shared by both clip components, so the group maths exist
once. It snapshots every selected clip's origin at `pointerdown`, previews
locally per `pointermove` (D-03: one invoke per gesture, never per event),
and emits ONE `move_clips` at `pointerup` inside the gesture boundary.

**Scope ruling D, restated as the algorithm** — snap the ANCHOR, apply its
delta to everyone:
```
dxSamples   = (clientX - startClientX) * view.spp
targetAnchor= altKey ? anchorOrigSamples + dxSamples
                     : view.snapSamples(anchorOrigSamples + dxSamples)
delta       = round(targetAnchor - anchorOrigSamples)
delta       = max(delta, -minOrigStartSamples)        // clamp the DELTA, not each clip
deltaTicks  = midi.samplesToTicks(anchorOrigSamples + delta)
            - midi.samplesToTicks(anchorOrigSamples)  // one section-table lookup pair
deltaTicks  = max(deltaTicks, -minOrigStartTicks)
```

**Files:**
- Create: `src/lib/state/clip-drag.svelte.ts`
- Test: `src/lib/state/clip-drag.test.ts`
- Modify: `src/lib/tauri.ts` (`Backend` interface + `TauriBackend`)
- Modify: `src/lib/types/ipc.ts` (`ClipPlacement`)
- Modify: `src/lib/components/ClipView.svelte` (drag handlers, :85-134)
- Modify: `src/lib/components/MidiClipView.svelte` (drag handlers, :126-201)

**Interfaces:**
- Consumes: `clipSelection` (Task 2), `project`, `midi`, `view`, `backend`.
- Produces:
  - In `src/lib/types/ipc.ts`:
    ```ts
    export type ClipPlacement =
      | { kind: "audio"; clipId: string; timelineStartSamples: number }
      | {
          kind: "midi";
          clipId: string;
          timelineStartTicks: number;
          lengthTicks?: number;
          contentLengthTicks?: number;
        };
    ```
  - In `src/lib/tauri.ts`'s `Backend` interface (optional, real-engine only —
    same convention as `moveClip?`):
    `moveClips?(placements: ClipPlacement[]): Promise<void>;`
    implemented as `invoke<void>("move_clips", { placements })`.
  - `export const clipDrag` with:
    - `active: boolean` (`$state`)
    - `begin(anchor: ClipRef, clientX: number): void`
    - `move(clientX: number, altKey: boolean): void`
    - `end(): Promise<void>`
    - `cancel(): void`
    - `computeDelta(clientX: number, altKey: boolean): { deltaSamples: number; deltaTicks: number }`
      (exported for the unit test; pure given the snapshot)

- [ ] **Step 1: Write the failing test**

Create `src/lib/state/clip-drag.test.ts`:

```ts
/**
 * Group drag: the delta comes from the ANCHOR (snapped once), every clip
 * moves by it, and the whole drag is ONE gesture-wrapped move_clips call.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Clip, MidiClip } from "../types/ipc";

const calls: string[] = [];
const gestureBegin = vi.fn(async () => {
  calls.push("gestureBegin");
});
const gestureEnd = vi.fn(async () => {
  calls.push("gestureEnd");
});
const moveClips = vi.fn(async () => {
  calls.push("moveClips");
});

vi.mock("../tauri", () => ({
  backend: {
    on: () => () => {},
    gestureBegin: (...a: unknown[]) => gestureBegin(...(a as [])),
    gestureEnd: () => gestureEnd(),
    moveClips: (p: unknown) => moveClips(p as never),
    getProjectState: () =>
      Promise.resolve({ ppq: 960, tempoEvents: [{ tick: 0, bpm: 120 }], midiClips: [] }),
  },
}));

const { project } = await import("./project.svelte");
const { midi } = await import("./midi.svelte");
const { view } = await import("./view.svelte");
const { clipSelection } = await import("./clip-selection.svelte");
const { clipDrag } = await import("./clip-drag.svelte");

beforeEach(() => {
  calls.length = 0;
  vi.clearAllMocks();
  project.sampleRate = 48000;
  project.tempoBpm = 120;
  project.timeSignature = [4, 4];
  // 1 sample per pixel keeps the pointer maths readable in the assertions
  view.spp = 1;
  view.snap = false;
  project.clips = [
    { id: "a1", trackId: "t1", timelineStartSamples: 1000, lengthSamples: 100 } as Clip,
    { id: "a2", trackId: "t1", timelineStartSamples: 3000, lengthSamples: 100 } as Clip,
  ];
  // a flat 120bpm/960ppq map: 1 beat = 24000 samples = 960 ticks -> 25 samples/tick
  midi.ppq = 960;
  midi.sectionTable = [];
  midi.clips = [{ id: "m1", trackId: "t2", timelineStartTicks: 400, lengthTicks: 960 } as MidiClip];
  clipSelection.clear();
});

describe("clipDrag group move", () => {
  it("moves every selected clip by the anchor's delta, preserving offsets", () => {
    clipSelection.apply(
      [
        { kind: "audio", id: "a1" },
        { kind: "audio", id: "a2" },
      ],
      "replace",
    );
    clipDrag.begin({ kind: "audio", id: "a1" }, 0);
    clipDrag.move(500, true); // alt = no snapping
    expect(project.clips[0].timelineStartSamples).toBe(1500);
    expect(project.clips[1].timelineStartSamples).toBe(3500);
    // the gap between them is untouched — the offsets requirement
    expect(project.clips[1].timelineStartSamples - project.clips[0].timelineStartSamples).toBe(2000);
  });

  it("clamps the DELTA at zero, so the group keeps its shape at the left edge", () => {
    clipSelection.apply(
      [
        { kind: "audio", id: "a1" },
        { kind: "audio", id: "a2" },
      ],
      "replace",
    );
    clipDrag.begin({ kind: "audio", id: "a2" }, 0);
    clipDrag.move(-9999, true);
    expect(project.clips[0].timelineStartSamples).toBe(0);
    expect(project.clips[1].timelineStartSamples).toBe(2000, "offsets survive the clamp");
  });

  it("snaps the anchor only — the other clips keep their off-grid offsets", () => {
    view.snap = true; // beat grid = 24000 samples at 120bpm/48k
    clipSelection.apply(
      [
        { kind: "audio", id: "a1" },
        { kind: "audio", id: "a2" },
      ],
      "replace",
    );
    clipDrag.begin({ kind: "audio", id: "a1" }, 0);
    clipDrag.move(23000, false);
    // the anchor lands on the grid; a2 keeps its exact 2000-sample offset
    expect(project.clips[0].timelineStartSamples % 24000).toBe(0);
    expect(project.clips[1].timelineStartSamples - project.clips[0].timelineStartSamples).toBe(2000);
  });

  it("moves a mixed audio+MIDI selection by the same wall-clock delta", () => {
    clipSelection.apply(
      [
        { kind: "audio", id: "a1" },
        { kind: "midi", id: "m1" },
      ],
      "replace",
    );
    clipDrag.begin({ kind: "audio", id: "a1" }, 0);
    clipDrag.move(2500, true); // +2500 samples = +100 ticks at 25 samples/tick
    expect(project.clips[0].timelineStartSamples).toBe(3500);
    expect(midi.clips[0].timelineStartTicks).toBe(500);
  });

  it("emits gestureBegin, then exactly one moveClips, then gestureEnd — in that order", async () => {
    clipSelection.apply(
      [
        { kind: "audio", id: "a1" },
        { kind: "midi", id: "m1" },
      ],
      "replace",
    );
    clipDrag.begin({ kind: "audio", id: "a1" }, 0);
    clipDrag.move(100, true);
    clipDrag.move(200, true);
    clipDrag.move(300, true);
    await clipDrag.end();
    expect(calls).toEqual(["gestureBegin", "moveClips", "gestureEnd"]);
    expect(moveClips).toHaveBeenCalledTimes(1);
    expect(moveClips).toHaveBeenCalledWith([
      { kind: "audio", clipId: "a1", timelineStartSamples: 1300 },
      { kind: "midi", clipId: "m1", timelineStartTicks: 412 },
    ]);
  });

  it("a drag that never moved sends no moveClips but still closes the gesture", async () => {
    clipSelection.apply([{ kind: "audio", id: "a1" }], "replace");
    clipDrag.begin({ kind: "audio", id: "a1" }, 0);
    await clipDrag.end();
    expect(moveClips).not.toHaveBeenCalled();
    expect(calls).toEqual(["gestureBegin", "gestureEnd"]);
  });

  it("dragging an unselected clip drags only that clip", () => {
    clipSelection.apply([{ kind: "audio", id: "a2" }], "replace");
    clipDrag.begin({ kind: "audio", id: "a1" }, 0);
    clipDrag.move(500, true);
    expect(project.clips[0].timelineStartSamples).toBe(1500);
    expect(project.clips[1].timelineStartSamples).toBe(3000, "a2 was not part of this drag");
  });
});
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `timeout 300 npx vitest run src/lib/state/clip-drag.test.ts`
Expected: FAIL — cannot resolve `./clip-drag.svelte`.

- [ ] **Step 3: Add the IPC type and the adapter method**

In `src/lib/types/ipc.ts`, after the `MidiClip` interface:

```ts
/** One clip's new placement in a `move_clips` batch (Track C). Audio is
 * placed in SAMPLES, MIDI in TICKS — two units that must never be
 * confusable, hence the `kind` tag. `lengthTicks`/`contentLengthTicks` are
 * additive and OPTIONAL: absent means "unchanged", never "clear". */
export type ClipPlacement =
  | { kind: "audio"; clipId: string; timelineStartSamples: number }
  | {
      kind: "midi";
      clipId: string;
      timelineStartTicks: number;
      lengthTicks?: number;
      contentLengthTicks?: number;
    };
```

In `src/lib/tauri.ts`, add to the `Backend` interface right after
`moveClip?`:

```ts
  /** Batch placement move — the group-drag counterpart of `moveClip`. Sent
   * ONCE at gesture end, inside a `gestureBegin`/`gestureEnd` boundary, so
   * the whole drag is one undo step. Optional — real-engine only, same
   * convention as `moveClip?`. */
  moveClips?(placements: ClipPlacement[]): Promise<void>;
```

and to `TauriBackend`, after `moveClip`:

```ts
  moveClips(placements: ClipPlacement[]) {
    return invoke<void>("move_clips", { placements });
  }
```

(add `ClipPlacement` to the existing `import type { … } from "./types/ipc"`
lists in both files).

- [ ] **Step 4: Write the controller**

Create `src/lib/state/clip-drag.svelte.ts`:

```ts
/**
 * Group drag/resize for the arrangement timeline.
 *
 * ONE controller shared by ClipView and MidiClipView so the group maths
 * exist once. The preview/commit split is the established one (Plan E Task
 * 4, D-03): pointermove mutates the stores locally ONLY; a single
 * `move_clips` invoke lands at pointerup, inside the gesture boundary, so a
 * whole drag is one undo entry.
 *
 * SNAPPING (plan scope ruling D): the ANCHOR — the clip under the pointer —
 * is snapped through the existing `view.snapSamples`, exactly as a
 * single-clip drag already is, and the resulting delta is applied to every
 * selected clip. Snapping each clip on its own would destroy the relative
 * offsets the whole feature exists to preserve. The delta, not the
 * individual clip, is clamped at the timeline origin — the same rule
 * note-ops' `nudgeSelection` follows for notes.
 *
 * ORDERING (the frontend half of the gesture-before-session lock rule):
 * `gestureBegin` is issued before any mutation, and the `move_clips` invoke
 * is AWAITED before `gestureEnd`. An un-awaited mutation racing `gestureEnd`
 * is the exact TOCTOU Plan E Task 14 closed backend-side.
 */

import { backend } from "../tauri";
import { project } from "./project.svelte";
import { midi } from "./midi.svelte";
import { view } from "./view.svelte";
import { clipSelection } from "./clip-selection.svelte";
import type { ClipRef } from "../utils/clip-selection";
import type { ClipPlacement } from "../types/ipc";

interface AudioOrigin {
  kind: "audio";
  id: string;
  startSamples: number;
}
interface MidiOrigin {
  kind: "midi";
  id: string;
  startTicks: number;
  lengthTicks: number;
  contentLengthTicks: number;
}
type Origin = AudioOrigin | MidiOrigin;

class ClipDragController {
  active = $state(false);
  /** True once the pointer has travelled far enough to count as a drag. */
  moved = $state(false);
  mode = $state<"move" | "resize">("move");

  private origins: Origin[] = [];
  private anchorOrigSamples = 0;
  private minStartSamples = 0;
  private minStartTicks = 0;
  private startClientX = 0;

  /** Snapshot the drag: every selected clip's origin, plus the anchor's.
   * When the anchor is NOT part of the selection, the drag is that clip
   * alone (clicking an unselected clip and dragging in one motion). */
  begin(anchor: ClipRef, clientX: number, mode: "move" | "resize" = "move") {
    const refs = clipSelection.has(anchor) ? clipSelection.refs() : [anchor];
    this.origins = [];
    for (const r of refs) {
      if (r.kind === "audio") {
        const c = project.clips.find((x) => x.id === r.id);
        if (c) this.origins.push({ kind: "audio", id: c.id, startSamples: c.timelineStartSamples });
      } else {
        const c = midi.clipById(r.id);
        if (c)
          this.origins.push({
            kind: "midi",
            id: c.id,
            startTicks: c.timelineStartTicks,
            lengthTicks: c.lengthTicks,
            // Pinned once, at drag start — the content length a first
            // right-edge drag ESTABLISHES (MidiClipView's existing rule).
            contentLengthTicks: midi.effectiveContentLengthTicks(c),
          });
      }
    }
    this.anchorOrigSamples =
      anchor.kind === "audio"
        ? (project.clips.find((c) => c.id === anchor.id)?.timelineStartSamples ?? 0)
        : midi.ticksToSamples(midi.clipById(anchor.id)?.timelineStartTicks ?? 0);
    this.minStartSamples = Math.min(
      ...this.origins.map((o) =>
        o.kind === "audio" ? o.startSamples : midi.ticksToSamples(o.startTicks),
      ),
      this.anchorOrigSamples,
    );
    this.minStartTicks = Math.min(
      ...this.origins.filter((o): o is MidiOrigin => o.kind === "midi").map((o) => o.startTicks),
      Number.MAX_SAFE_INTEGER,
    );
    this.startClientX = clientX;
    this.mode = mode;
    this.moved = false;
    this.active = true;
    void backend.gestureBegin?.(mode === "resize" ? "resize clips" : "move clips");
  }

  /** The one delta pair the whole group moves by. Pure given the snapshot. */
  computeDelta(clientX: number, altKey: boolean): { deltaSamples: number; deltaTicks: number } {
    const dx = (clientX - this.startClientX) * view.spp;
    let target = this.anchorOrigSamples + dx;
    if (!altKey) target = view.snapSamples(target);
    let deltaSamples = Math.round(target - this.anchorOrigSamples);
    if (this.minStartSamples + deltaSamples < 0) deltaSamples = -this.minStartSamples;
    let deltaTicks =
      midi.samplesToTicks(this.anchorOrigSamples + deltaSamples) -
      midi.samplesToTicks(this.anchorOrigSamples);
    if (this.minStartTicks !== Number.MAX_SAFE_INTEGER && this.minStartTicks + deltaTicks < 0) {
      deltaTicks = -this.minStartTicks;
    }
    return { deltaSamples, deltaTicks: Math.round(deltaTicks) };
  }

  /** Live preview — store-local only, no invoke (D-03). */
  move(clientX: number, altKey: boolean) {
    if (!this.active) return;
    if (Math.abs(clientX - this.startClientX) > 2) this.moved = true;
    if (!this.moved) return;
    const { deltaSamples, deltaTicks } = this.computeDelta(clientX, altKey);
    if (this.mode === "resize") {
      this.previewResize(deltaTicks);
      return;
    }
    for (const o of this.origins) {
      if (o.kind === "audio") project.moveClip(o.id, o.startSamples + deltaSamples);
      else midi.moveClip(o.id, o.startTicks + deltaTicks);
    }
  }

  /** Group loop-length adjust (Task 7): MIDI clips only — audio clips have
   * no shipped resize gesture (plan scope ruling G). */
  private previewResize(deltaTicks: number) {
    const byId = new Map(
      this.origins.filter((o): o is MidiOrigin => o.kind === "midi").map((o) => [o.id, o]),
    );
    if (byId.size === 0) return;
    midi.clips = midi.clips.map((c) => {
      const o = byId.get(c.id);
      if (!o) return c;
      return {
        ...c,
        lengthTicks: Math.max(1, o.lengthTicks + deltaTicks),
        contentLengthTicks: o.contentLengthTicks,
      };
    });
  }

  /** Commit: ONE move_clips from the stores' CURRENT values (same rule
   * `commitClipMove` follows), awaited, then close the gesture. */
  async end(): Promise<void> {
    if (!this.active) return;
    const moved = this.moved;
    const origins = this.origins;
    const mode = this.mode;
    this.active = false;
    this.moved = false;
    this.origins = [];
    if (moved && origins.length > 0) {
      const placements: ClipPlacement[] = [];
      for (const o of origins) {
        if (o.kind === "audio") {
          if (mode === "resize") continue; // scope ruling G
          const c = project.clips.find((x) => x.id === o.id);
          if (c) placements.push({ kind: "audio", clipId: c.id, timelineStartSamples: c.timelineStartSamples });
        } else {
          const c = midi.clipById(o.id);
          if (!c) continue;
          placements.push(
            mode === "resize"
              ? {
                  kind: "midi",
                  clipId: c.id,
                  timelineStartTicks: c.timelineStartTicks,
                  lengthTicks: c.lengthTicks,
                  contentLengthTicks: c.contentLengthTicks ?? o.contentLengthTicks,
                }
              : { kind: "midi", clipId: c.id, timelineStartTicks: c.timelineStartTicks },
          );
        }
      }
      if (placements.length > 0) {
        try {
          await backend.moveClips?.(placements);
        } catch (err) {
          console.error("[aura] move_clips failed:", err);
        }
      }
    }
    await backend.gestureEnd?.();
  }

  /** pointercancel / Escape: close the gesture without sending anything. The
   * local preview stays where it is; the next commit or a project reload
   * reconciles it — the same forgiveness `gestureEnd` itself has. */
  cancel() {
    this.active = false;
    this.moved = false;
    this.origins = [];
    void backend.gestureEnd?.();
  }
}

export const clipDrag = new ClipDragController();
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `timeout 300 npx vitest run src/lib/state/clip-drag.test.ts`
Expected: PASS, 7 tests.

- [ ] **Step 6: Wire `ClipView.svelte` to the controller**

Add `import { clipDrag } from "../state/clip-drag.svelte";` and replace the
drag block (the `dragging`/`dragStartX`/`dragOrigSamples`/`dragMoved` locals
and the three handlers, :85-124) with:

```ts
  const dragging = $derived(clipDrag.active && selected);

  function onPointerDown(e: PointerEvent) {
    if (e.button !== 0) return;
    if ((e.target as HTMLElement).closest("button")) return;
    const ref = { kind: "audio", id: clip.id } as const;
    const mode = selectionModeFor(e);
    if (!(mode === "replace" && clipSelection.has(ref))) clipSelection.apply([ref], mode);
    else clipSelection.anchor = ref;
    project.select(clip.id);
    clipDrag.begin(ref, e.clientX);
    try {
      (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    } catch {
      /* synthetic pointers have no active id */
    }
  }

  function onPointerMove(e: PointerEvent) {
    clipDrag.move(e.clientX, e.altKey);
  }

  function onPointerUp(e: PointerEvent) {
    try {
      (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    } catch {
      /* not captured */
    }
    void clipDrag.end();
  }
```

and add `onpointercancel={() => clipDrag.cancel()}` to the clip div. Keep
`onKeydown` exactly as it is (arrow-key nudge stays single-clip — recorded
as out of scope, like Ctrl+D).

- [ ] **Step 7: Wire `MidiClipView.svelte` to the controller**

Same shape. Its `onPointerDown` keeps the edge test and passes the mode
through:

```ts
  function onPointerDown(e: PointerEvent) {
    if (e.button !== 0) return;
    const ref = { kind: "midi", id: clip.id } as const;
    const mode = selectionModeFor(e);
    if (!(mode === "replace" && clipSelection.has(ref))) clipSelection.apply([ref], mode);
    else clipSelection.anchor = ref;
    midi.select(clip.id);
    project.select(null);
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const nearRightEdge = rect.right - e.clientX <= EDGE_PX;
    dragMode = nearRightEdge ? "resize" : "move";
    clipDrag.begin(ref, e.clientX, dragMode);
    (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
  }

  function onPointerMove(e: PointerEvent) {
    updateHoverEdge(e);
    clipDrag.move(e.clientX, e.altKey);
  }

  function onPointerUp(e: PointerEvent) {
    (e.currentTarget as HTMLElement).releasePointerCapture?.(e.pointerId);
    void clipDrag.end();
  }
```

with `let dragging = $derived(clipDrag.active && selected);` and
`let dragMode: "move" | "resize" = $state("move");` retained (the `.edge`
class still reads it). Delete the now-unused `dragStartX`, `dragOrigTicks`,
`dragOrigLengthTicks`, `dragOrigContentTicks`, `dragMoved` locals — the
controller owns that snapshot now. Add
`onpointercancel={() => clipDrag.cancel()}`.

- [ ] **Step 8: Type-check and run the whole frontend suite**

Run: `npx svelte-check --threshold error`
Expected: no new errors (in particular, no "declared but never read" for the
deleted locals).

Run: `timeout 300 npx vitest run`
Expected: 243 passed. Update `README.md:390` to 243.

- [ ] **Step 9: Manual verification (the one thing tests cannot cover)**

Build and run the app (`.claude` skill `run-aura`, or
`npm run tauri dev`). Select two clips on different tracks — one audio, one
MIDI — drag them, release, press Ctrl+Z ONCE. Both must return together.
Record the result in the task's review notes.

- [ ] **Step 10: Commit**

```bash
git add src/lib/state/clip-drag.svelte.ts src/lib/state/clip-drag.test.ts \
        src/lib/tauri.ts src/lib/types/ipc.ts \
        src/lib/components/ClipView.svelte src/lib/components/MidiClipView.svelte README.md
git commit -m "feat(timeline): group drag as one gesture-wrapped move_clips transaction"
```

---

### Task 7: Group loop-length adjust

The controller already carries `mode: "resize"` from Task 6. This task
proves it end-to-end and adds the multi-clip visual affordance: when more
than one MIDI clip is selected, EVERY selected MIDI clip shows the right-edge
resize cursor, so it is discoverable that the drag will move them all.

**Files:**
- Modify: `src/lib/components/MidiClipView.svelte` (the `.edge` class
  condition)
- Test: `src/lib/state/clip-drag.test.ts` (append a `describe` block)

**Interfaces:**
- Consumes: `clipDrag.begin(ref, clientX, "resize")`, `ClipPlacement`'s
  `lengthTicks`/`contentLengthTicks`.
- Produces: nothing new.

- [ ] **Step 1: Write the failing test**

Append to `src/lib/state/clip-drag.test.ts`:

```ts
describe("clipDrag group resize (loop length)", () => {
  beforeEach(() => {
    midi.clips = [
      { id: "m1", trackId: "t2", timelineStartTicks: 0, lengthTicks: 960 } as MidiClip,
      { id: "m2", trackId: "t2", timelineStartTicks: 4800, lengthTicks: 1920 } as MidiClip,
    ];
  });

  it("adds the same tick delta to every selected MIDI clip's length", () => {
    clipSelection.apply(
      [
        { kind: "midi", id: "m1" },
        { kind: "midi", id: "m2" },
      ],
      "replace",
    );
    clipDrag.begin({ kind: "midi", id: "m1" }, 0, "resize");
    clipDrag.move(2500, true); // +2500 samples = +100 ticks
    expect(midi.clips[0].lengthTicks).toBe(1060);
    expect(midi.clips[1].lengthTicks).toBe(2020);
  });

  it("never shrinks a clip below one tick", () => {
    clipSelection.apply([{ kind: "midi", id: "m1" }], "replace");
    clipDrag.begin({ kind: "midi", id: "m1" }, 0, "resize");
    clipDrag.move(-999999, true);
    expect(midi.clips[0].lengthTicks).toBe(1);
  });

  it("pins each clip's content length at drag start, so a first resize establishes it", () => {
    clipSelection.apply([{ kind: "midi", id: "m1" }], "replace");
    clipDrag.begin({ kind: "midi", id: "m1" }, 0, "resize");
    clipDrag.move(2500, true);
    // m1 had no explicit content length; its start-of-gesture placement
    // length (960) becomes the content length, as MidiClipView's rule says
    expect(midi.clips[0].contentLengthTicks).toBe(960);
  });

  it("sends one move_clips carrying bounds, and leaves audio clips out", async () => {
    project.clips = [
      { id: "a1", trackId: "t1", timelineStartSamples: 0, lengthSamples: 100 } as Clip,
    ];
    clipSelection.apply(
      [
        { kind: "midi", id: "m1" },
        { kind: "audio", id: "a1" },
      ],
      "replace",
    );
    clipDrag.begin({ kind: "midi", id: "m1" }, 0, "resize");
    clipDrag.move(2500, true);
    await clipDrag.end();
    expect(moveClips).toHaveBeenCalledWith([
      {
        kind: "midi",
        clipId: "m1",
        timelineStartTicks: 0,
        lengthTicks: 1060,
        contentLengthTicks: 960,
      },
    ]);
  });
});
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `timeout 300 npx vitest run src/lib/state/clip-drag.test.ts`
Expected: FAIL — the resize path exists from Task 6 but the audio-exclusion
and content-length pinning assertions pin down behaviour Task 6's tests did
not cover; if all four pass first try, that is a legitimate green (the
controller was written to this spec) — record it and go to Step 4.

- [ ] **Step 3: Fix whatever fails**

The two likely gaps in Task 6's controller: `previewResize` must apply
`Math.max(1, …)` (it does) and `end()` must `continue` past audio origins in
resize mode (it does). If a test fails, fix `clip-drag.svelte.ts`, not the
test — the test is the spec.

- [ ] **Step 4: Add the multi-selection resize affordance**

In `MidiClipView.svelte`, replace the `.edge` class condition on the clip
div so a hovered edge highlights every selected MIDI clip:

```svelte
    class:edge={hoverEdge ||
      (dragging && clipDrag.mode === "resize") ||
      (selected && groupEdgeHover)}
```

with, in the script block:

```ts
  import { clipDrag } from "../state/clip-drag.svelte";
  /** True while the pointer hovers ANY selected MIDI clip's right edge —
   * so a group resize announces itself on every clip it will affect, not
   * just the one under the pointer. */
  const groupEdgeHover = $derived(clipDrag.edgeHoverActive && clipSelection.count() > 1);
```

and in `clip-drag.svelte.ts` add:

```ts
  /** Set by MidiClipView while a right-edge hover is live, so sibling
   * selected clips can show the same affordance. Pure chrome — no document
   * meaning. */
  edgeHoverActive = $state(false);
```

with `MidiClipView`'s `updateHoverEdge` setting
`clipDrag.edgeHoverActive = hoverEdge;` after computing `hoverEdge`, and the
clip div's `onpointerleave={() => { hoverEdge = false; clipDrag.edgeHoverActive = false; }}`.

- [ ] **Step 5: Type-check and run the whole frontend suite**

Run: `npx svelte-check --threshold error`
Run: `timeout 300 npx vitest run`
Expected: 247 passed. Update `README.md:390` to 247.

- [ ] **Step 6: Commit**

```bash
git add src/lib/state/clip-drag.svelte.ts src/lib/state/clip-drag.test.ts \
        src/lib/components/MidiClipView.svelte README.md
git commit -m "feat(timeline): group loop-length adjust — one delta across every selected MIDI clip"
```

---

### Task 8: Backend `clips_copy` — the `application/x-aura-clips` payload

A pure READ (it must stay one, or it becomes a §7-test-4 / Figma-invariant
violation): it takes explicit clip-id lists, reads the session, and returns
the payload. Building it backend-side is what keeps the in-app and
cross-instance paths byte-identical, and it is where the anchor's
tick↔sample resolution belongs (ADR 0006 — the frontend does no time math).

**Files:**
- Create: `src-tauri/src/control/clipboard.rs`
- Modify: `src-tauri/src/control/mod.rs` — add `pub mod clipboard;` next to
  the other `control` submodules
- Modify: `src-tauri/src/lib.rs` — register
  `control::clipboard::clips_copy` in `generate_handler!`
- Test: `src-tauri/src/control/clipboard.rs`'s own `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: `ControlPlane::session()` (add a non-`#[cfg(test)]` read path if
  needed — prefer a new private read inside `ControlPlane` over widening the
  test accessor), `crate::midi::tempo`'s tick↔sample conversion as used by
  `ProjectSnapshot` (the same section table the frontend consumes).
- Produces (Task 9, Task 10 and `aura-clips.ts` consume exactly these):

```rust
pub const AURA_CLIPS_MIME: &str = "application/x-aura-clips";
pub const AURA_CLIPS_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuraClipsPayload {
    /// Always `AURA_CLIPS_MIME`. The MIME name rides INSIDE the envelope
    /// because the OS clipboard slot reachable from a Tauri v2/WebKitGTK
    /// app is plain text — arbitrary flavors are not available (scope
    /// ruling C).
    pub mime: String,
    pub schema_version: u32,
    /// The anchor: the leftmost selected clip's start, in BOTH units, so a
    /// paste can offset audio in samples and MIDI in ticks without
    /// re-deriving either.
    pub anchor_samples: u64,
    pub anchor_ticks: u64,
    pub ppq: u32,
    /// Absolute path of the source project's .aura dir, or null when the
    /// source session had none. Diagnostic + the base for resolving
    /// `source_path` on the other side.
    pub source_project_dir: Option<String>,
    pub clips: Vec<ClipboardClip>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ClipboardClip {
    Audio {
        name: String,
        source_track_id: String,
        source_track_name: String,
        /// Signed: a clip left of the anchor is impossible (the anchor IS
        /// the leftmost) but the type stays signed so a future anchor rule
        /// does not need a format change.
        offset_from_anchor_samples: i64,
        length_samples: u64,
        /// Project-relative, POSIX separators — resolved against the target
        /// project first.
        source_path: String,
        /// Absolute path at copy time; null when no project was open.
        source_abs_path: Option<String>,
        source_channels: u16,
        source_sample_rate: u32,
        source_length_samples: u64,
        offset_samples: u64,
        gain_db: f64,
        fade_in_samples: u64,
        fade_out_samples: u64,
    },
    Midi {
        name: String,
        source_track_id: String,
        source_track_name: String,
        offset_from_anchor_ticks: i64,
        length_ticks: u64,
        content_length_ticks: Option<u64>,
        /// By VALUE, with every `note_id` zeroed to the mint sentinel —
        /// a copy mints fresh ids (round-2 §2.1, ADR 0001).
        notes: Vec<crate::midi::types::MidiNote>,
    },
}
```
  plus `pub fn clips_copy(&self, audio_clip_ids: Vec<String>, midi_clip_ids: Vec<String>) -> Result<AuraClipsPayload, String>`
  on `ControlPlane` (implemented in this module via `impl ControlPlane`) and
  `#[tauri::command] pub fn clips_copy(audio_clip_ids: Vec<String>, midi_clip_ids: Vec<String>, control: State<'_, Arc<ControlPlane>>) -> Result<AuraClipsPayload, String>`.

- [ ] **Step 1: Write the failing tests**

Create `src-tauri/src/control/clipboard.rs` with only the test module first
(so Step 2's failure is honest), or write the whole file and run the tests —
either order is fine, but the tests must exist before the implementation is
considered done. The tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::op::{Op, TxMeta};

    /// Reuses control/mod.rs's harness — a ControlPlane over a test engine
    /// double with the named tracks already slotted.
    fn plane() -> crate::control::ControlPlane {
        let (cp, _rx, _events) = crate::control::tests::test_plane_with_tracks(&["t-1", "t-2"]);
        cp
    }

    #[test]
    fn copy_offsets_are_relative_to_the_leftmost_clip() {
        let cp = plane();
        cp.commit(TxMeta::user("seed"), |tx| {
            let mut a = crate::audio::types::testutil::test_clip("a-1", "t-1");
            a.timeline_start_samples = 96_000;
            let mut b = crate::audio::types::testutil::test_clip("a-2", "t-2");
            b.timeline_start_samples = 48_000;
            tx.apply(Op::ClipAdd { clip: a, index: 0 })?;
            tx.apply(Op::ClipAdd { clip: b, index: 1 })
        })
        .unwrap();

        let p = cp.clips_copy(vec!["a-1".into(), "a-2".into()], vec![]).unwrap();
        assert_eq!(p.mime, AURA_CLIPS_MIME);
        assert_eq!(p.schema_version, AURA_CLIPS_SCHEMA_VERSION);
        assert_eq!(p.anchor_samples, 48_000, "the anchor is the leftmost clip");
        let offsets: Vec<i64> = p
            .clips
            .iter()
            .map(|c| match c {
                ClipboardClip::Audio { offset_from_anchor_samples, .. } => *offset_from_anchor_samples,
                _ => panic!("expected audio"),
            })
            .collect();
        assert_eq!(offsets, vec![48_000, 0], "offsets preserve the arrangement's shape");
    }

    #[test]
    fn copy_strips_note_ids_to_the_mint_sentinel() {
        let cp = plane();
        cp.commit(TxMeta::user("seed"), |tx| {
            let mut c = crate::control::tests::dummy_midi_clip("t-1");
            c.notes = vec![crate::midi::types::MidiNote {
                tick: 0,
                length_ticks: 480,
                key: 60,
                velocity: 100,
                channel: 0,
                note_id: crate::ids::NoteId(7),
            }];
            c.next_note_id = 8;
            tx.apply(Op::MidiClipAdd { clip: c, index: 0 })
        })
        .unwrap();
        let id = cp.session().lock().midi.clips[0].id.to_string();

        let p = cp.clips_copy(vec![], vec![id]).unwrap();
        match &p.clips[0] {
            ClipboardClip::Midi { notes, .. } => {
                assert_eq!(notes.len(), 1);
                assert_eq!(
                    notes[0].note_id.0, 0,
                    "a copy carries the mint sentinel — ids are never reused (ADR 0001)"
                );
                assert_eq!(notes[0].key, 60, "the musical content travels by value");
            }
            _ => panic!("expected midi"),
        }
    }

    #[test]
    fn copy_of_a_mixed_selection_anchors_on_the_earliest_clip_in_either_store() {
        let cp = plane();
        cp.commit(TxMeta::user("seed"), |tx| {
            let mut a = crate::audio::types::testutil::test_clip("a-1", "t-1");
            a.timeline_start_samples = 96_000;
            let mut m = crate::control::tests::dummy_midi_clip("t-2");
            m.timeline_start_ticks = 0; // tick 0 = sample 0, earlier than the audio clip
            tx.apply(Op::ClipAdd { clip: a, index: 0 })?;
            tx.apply(Op::MidiClipAdd { clip: m, index: 0 })
        })
        .unwrap();
        let mid = cp.session().lock().midi.clips[0].id.to_string();

        let p = cp.clips_copy(vec!["a-1".into()], vec![mid]).unwrap();
        assert_eq!(p.anchor_samples, 0);
        assert_eq!(p.anchor_ticks, 0);
        let audio_off = p.clips.iter().find_map(|c| match c {
            ClipboardClip::Audio { offset_from_anchor_samples, .. } => Some(*offset_from_anchor_samples),
            _ => None,
        });
        assert_eq!(audio_off, Some(96_000));
    }

    #[test]
    fn copy_rejects_an_unknown_clip_id() {
        let cp = plane();
        let err = cp.clips_copy(vec!["nope".into()], vec![]).unwrap_err();
        assert!(err.contains("nope"), "{err}");
    }

    #[test]
    fn copy_of_nothing_is_an_empty_payload_not_an_error() {
        let cp = plane();
        let p = cp.clips_copy(vec![], vec![]).unwrap();
        assert!(p.clips.is_empty());
        assert_eq!(p.anchor_samples, 0);
    }

    #[test]
    fn copy_mutates_nothing_and_creates_no_history_step() {
        let cp = plane();
        cp.commit(TxMeta::user("seed"), |tx| {
            tx.apply(Op::ClipAdd {
                clip: crate::audio::types::testutil::test_clip("a-1", "t-1"),
                index: 0,
            })
        })
        .unwrap();
        let (undo_before, redo_before) = cp.history_depths();
        let rev_before = cp.session().lock().rev;

        let _ = cp.clips_copy(vec!["a-1".into()], vec![]).unwrap();

        assert_eq!(cp.history_depths(), (undo_before, redo_before), "copy is a READ");
        assert_eq!(cp.session().lock().rev, rev_before, "copy bumps no revision");
    }

    #[test]
    fn payload_round_trips_through_json_in_camel_case() {
        let cp = plane();
        cp.commit(TxMeta::user("seed"), |tx| {
            tx.apply(Op::ClipAdd {
                clip: crate::audio::types::testutil::test_clip("a-1", "t-1"),
                index: 0,
            })
        })
        .unwrap();
        let p = cp.clips_copy(vec!["a-1".into()], vec![]).unwrap();
        let s = serde_json::to_string(&p).unwrap();
        eprintln!("AuraClipsPayload wire form: {s}");
        assert!(s.contains("\"schemaVersion\":1"), "{s}");
        assert!(s.contains("\"kind\":\"audio\""), "{s}");
        assert!(s.contains("\"offsetFromAnchorSamples\""), "{s}");
        let back: AuraClipsPayload = serde_json::from_str(&s).unwrap();
        assert_eq!(back.clips.len(), 1);
    }
}
```

Note: `test_plane_with_tracks` and `dummy_midi_clip` currently live in
`control/mod.rs`'s private `mod tests`. Make them `pub(crate)` there (a
test-only visibility widening, no production impact) so this module's tests
can reuse them rather than cloning the harness.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml clipboard::`
Expected: FAIL — `no method named clips_copy`.

- [ ] **Step 3: Write the implementation**

Create the production half of `src-tauri/src/control/clipboard.rs` above the
test module. Structure:

```rust
//! The `application/x-aura-clips` clipboard payload: what a multi-clip copy
//! puts on the OS clipboard, and what a paste turns back into document rows.
//!
//! MIDI clips travel BY VALUE (their notes are the content, and they are
//! small); audio clips travel BY REFERENCE (a wav is megabytes — the
//! clipboard is not an asset store). See the plan's scope ruling B for the
//! three-step asset resolution a paste performs, and scope ruling C for why
//! the MIME name rides inside the envelope instead of being an OS clipboard
//! flavor.
```

`ControlPlane::clips_copy`:

1. Lock the session ONCE. Collect the requested audio clips (`store.clips`)
   and MIDI clips (`midi.clips`) by id, returning
   `Err(format!("unknown clip: {id}"))` on the first miss.
2. Compute each clip's start in samples — audio directly, MIDI via the same
   tick→sample function `ProjectSnapshot` uses. `anchor_samples` is the min
   (0 for an empty selection); `anchor_ticks` is the sample→tick conversion
   of `anchor_samples`.
3. Build `ClipboardClip::Audio` entries with
   `offset_from_anchor_samples = start as i64 - anchor_samples as i64`, and
   `source_abs_path = store.project_dir.map(|d| d.join(&source_path))`
   rendered as a string when the file is inside a project.
4. Build `ClipboardClip::Midi` entries with
   `offset_from_anchor_ticks = timeline_start_ticks as i64 - anchor_ticks as i64`
   and `notes` deep-copied with `note_id: crate::ids::NoteId(0)`.
5. Look up each clip's track NAME while the lock is held (it is what a
   paste-to-new-tracks names the created track).
6. Drop the lock and return. **No mutation of any kind** — no lazy resync,
   no `ensure_project`, nothing. This function must stay a pure read or it
   breaks the Figma invariant.

The Tauri wrapper:

```rust
/// Build the `application/x-aura-clips` payload for an explicit set of
/// clips — a pure READ. Selection lives in the frontend; the backend is
/// only ever told WHICH clips, never "what is selected". Additive command.
#[tauri::command]
pub fn clips_copy(
    audio_clip_ids: Vec<String>,
    midi_clip_ids: Vec<String>,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<AuraClipsPayload, String> {
    control.clips_copy(audio_clip_ids, midi_clip_ids)
}
```

Register `control::clipboard::clips_copy` in `src-tauri/src/lib.rs`'s
`generate_handler!`, grouped after `control::move_clips`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml clipboard::`
Expected: PASS, 7 tests.

- [ ] **Step 5: Run the whole backend suite and update the dated counts**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 540 total. Update `README.md:389` and `CONTRIBUTING.md:62` to 540.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/control/clipboard.rs src-tauri/src/control/mod.rs \
        src-tauri/src/lib.rs README.md CONTRIBUTING.md
git commit -m "feat(control): clips_copy — the application/x-aura-clips payload, built backend-side as a pure read"
```

---

### Task 9: Backend `clips_paste` — one transaction, one undo entry

The heart of scope ruling A. Asset resolution happens BEFORE the
transaction (prepare-outside, round-2 §4.4); the transaction is a short
apply of existing ops.

**Files:**
- Modify: `src-tauri/src/control/clipboard.rs` (add the paste half + tests)
- Modify: `src-tauri/src/lib.rs` (register `control::clipboard::clips_paste`)

**Interfaces:**
- Consumes: `AuraClipsPayload`/`ClipboardClip` (Task 8),
  `crate::control::ops::add_track_tx`, `Op::{TrackAdd, ClipAdd, MidiClipAdd}`,
  `crate::ids::{ClipId, ContentId, LaneId}`,
  `crate::midi::types::MidiClip::ensure_note_ids`.
- Produces:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasteRequest {
    pub payload: AuraClipsPayload,
    /// Where the anchor lands: the playhead, in samples.
    pub at_samples: u64,
    /// Paste-to-new-tracks: one fresh track per DISTINCT source track,
    /// named "<sourceTrackName> copy", clips retargeted to the new rows.
    #[serde(default)]
    pub to_new_tracks: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasteResult {
    pub audio_clips: Vec<crate::audio::types::Clip>,
    pub midi_clips: Vec<crate::midi::types::MidiClip>,
    pub created_tracks: Vec<crate::audio::types::TrackState>,
    pub skipped: Vec<SkippedClip>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedClip {
    pub name: String,
    pub reason: String,
}
```
  plus `ControlPlane::clips_paste(&self, req: PasteRequest) -> Result<PasteResult, String>`
  and `#[tauri::command] pub fn clips_paste(request: PasteRequest, control: State<'_, Arc<ControlPlane>>) -> Result<PasteResult, String>`.

- [ ] **Step 1: Write the failing tests**

Append to `src-tauri/src/control/clipboard.rs`'s test module:

```rust
    fn paste_req(payload: AuraClipsPayload, at_samples: u64, to_new_tracks: bool) -> PasteRequest {
        PasteRequest { payload, at_samples, to_new_tracks }
    }

    /// The plan's central claim: a mixed paste is ONE transaction and ONE
    /// undo entry, and Ctrl+Z takes every pasted clip back out together.
    #[test]
    fn paste_of_a_mixed_selection_is_one_history_entry_that_undoes_wholly() {
        let cp = plane();
        cp.commit(TxMeta::user("seed"), |tx| {
            let mut a = crate::audio::types::testutil::test_clip("a-1", "t-1");
            a.timeline_start_samples = 0;
            tx.apply(Op::ClipAdd { clip: a, index: 0 })?;
            tx.apply(Op::MidiClipAdd {
                clip: crate::control::tests::dummy_midi_clip("t-2"),
                index: 0,
            })
        })
        .unwrap();
        let mid = cp.session().lock().midi.clips[0].id.to_string();
        let payload = cp.clips_copy(vec!["a-1".into()], vec![mid]).unwrap();
        let (undo_before, _) = cp.history_depths();

        let res = cp.clips_paste(paste_req(payload, 96_000, false)).unwrap();
        assert_eq!(res.audio_clips.len(), 1);
        assert_eq!(res.midi_clips.len(), 1);
        assert!(res.skipped.is_empty(), "{:?}", res.skipped);
        assert_eq!(
            cp.history_depths().0,
            undo_before + 1,
            "a whole paste is exactly ONE undo step"
        );
        {
            let s = cp.session().lock();
            assert_eq!(s.store.clips.len(), 2);
            assert_eq!(s.midi.clips.len(), 2);
        }

        cp.undo().unwrap();
        let s = cp.session().lock();
        assert_eq!(s.store.clips.len(), 1, "one Ctrl+Z removed the whole paste");
        assert_eq!(s.midi.clips.len(), 1);
    }

    /// Requirement 2, verbatim: every clip lands at the playhead WITH its
    /// individual offset intact, each on the SAME track it came from.
    #[test]
    fn paste_preserves_per_clip_offsets_and_source_tracks() {
        let cp = plane();
        cp.commit(TxMeta::user("seed"), |tx| {
            let mut a = crate::audio::types::testutil::test_clip("a-1", "t-1");
            a.timeline_start_samples = 48_000;
            let mut b = crate::audio::types::testutil::test_clip("a-2", "t-2");
            b.timeline_start_samples = 72_000;
            tx.apply(Op::ClipAdd { clip: a, index: 0 })?;
            tx.apply(Op::ClipAdd { clip: b, index: 1 })
        })
        .unwrap();
        let payload = cp.clips_copy(vec!["a-1".into(), "a-2".into()], vec![]).unwrap();

        let res = cp.clips_paste(paste_req(payload, 480_000, false)).unwrap();
        let mut placed: Vec<(String, u64)> = res
            .audio_clips
            .iter()
            .map(|c| (c.track_id.to_string(), c.timeline_start_samples))
            .collect();
        placed.sort();
        assert_eq!(
            placed,
            vec![("t-1".to_string(), 480_000), ("t-2".to_string(), 504_000)],
            "the 24000-sample gap survives the paste, and each clip stayed on its track"
        );
    }

    /// Fresh identities: a paste is a COPY, never an instance (round-2
    /// §2.1's remint rule; instancing is this plan's explicit non-goal).
    #[test]
    fn paste_mints_fresh_clip_content_and_note_ids() {
        let cp = plane();
        cp.commit(TxMeta::user("seed"), |tx| {
            let mut c = crate::control::tests::dummy_midi_clip("t-2");
            c.notes = vec![crate::midi::types::MidiNote {
                tick: 0,
                length_ticks: 480,
                key: 64,
                velocity: 90,
                channel: 0,
                note_id: crate::ids::NoteId(3),
            }];
            c.next_note_id = 4;
            tx.apply(Op::MidiClipAdd { clip: c, index: 0 })
        })
        .unwrap();
        let (src_id, src_content) = {
            let s = cp.session().lock();
            (s.midi.clips[0].id.to_string(), s.midi.clips[0].content_id.clone())
        };
        let payload = cp.clips_copy(vec![], vec![src_id.clone()]).unwrap();

        let res = cp.clips_paste(paste_req(payload, 0, false)).unwrap();
        let pasted = &res.midi_clips[0];
        assert_ne!(pasted.id.to_string(), src_id, "fresh clip id");
        assert_ne!(pasted.content_id, src_content, "fresh ContentId — a copy, not an instance");
        assert_eq!(pasted.notes.len(), 1);
        assert!(pasted.notes[0].note_id.0 > 0, "the mint sentinel was resolved to a real id");
        assert!(
            pasted.next_note_id > pasted.notes[0].note_id.0,
            "the watermark is past every minted id"
        );
    }

    /// Requirement 3: paste onto NEW tracks — one fresh track per distinct
    /// source track, in the SAME transaction, so undo removes the tracks too.
    #[test]
    fn paste_to_new_tracks_creates_one_track_per_source_track_in_the_same_transaction() {
        let cp = plane();
        cp.commit(TxMeta::user("seed"), |tx| {
            tx.apply(Op::ClipAdd {
                clip: crate::audio::types::testutil::test_clip("a-1", "t-1"),
                index: 0,
            })?;
            tx.apply(Op::ClipAdd {
                clip: crate::audio::types::testutil::test_clip("a-2", "t-1"),
                index: 1,
            })?;
            tx.apply(Op::ClipAdd {
                clip: crate::audio::types::testutil::test_clip("a-3", "t-2"),
                index: 2,
            })
        })
        .unwrap();
        let payload = cp
            .clips_copy(vec!["a-1".into(), "a-2".into(), "a-3".into()], vec![])
            .unwrap();
        let tracks_before = cp.session().lock().store.tracks.len();
        let (undo_before, _) = cp.history_depths();

        let res = cp.clips_paste(paste_req(payload, 0, true)).unwrap();
        assert_eq!(res.created_tracks.len(), 2, "two distinct source tracks -> two new tracks");
        assert_eq!(cp.session().lock().store.tracks.len(), tracks_before + 2);
        // the two clips from t-1 share ONE new track
        let new_ids: Vec<String> = res.created_tracks.iter().map(|t| t.id.to_string()).collect();
        assert!(res.audio_clips.iter().all(|c| new_ids.contains(&c.track_id.to_string())));
        assert_eq!(cp.history_depths().0, undo_before + 1, "still ONE undo step");

        cp.undo().unwrap();
        assert_eq!(
            cp.session().lock().store.tracks.len(),
            tracks_before,
            "undo removes the tracks the paste created, not just its clips"
        );
    }

    /// Scope ruling B: a MIDI clip whose source track is gone is skipped and
    /// reported — it does not fail the paste, and it does not silently vanish.
    #[test]
    fn paste_skips_a_clip_whose_source_track_is_missing_and_reports_it() {
        let cp = plane();
        cp.commit(TxMeta::user("seed"), |tx| {
            tx.apply(Op::MidiClipAdd {
                clip: crate::control::tests::dummy_midi_clip("t-2"),
                index: 0,
            })
        })
        .unwrap();
        let mid = cp.session().lock().midi.clips[0].id.to_string();
        let mut payload = cp.clips_copy(vec![], vec![mid]).unwrap();
        // rewrite the source track to one this session does not have
        if let ClipboardClip::Midi { source_track_id, .. } = &mut payload.clips[0] {
            *source_track_id = "gone".into();
        }

        let res = cp.clips_paste(paste_req(payload, 0, false)).unwrap();
        assert!(res.midi_clips.is_empty());
        assert_eq!(res.skipped.len(), 1);
        assert!(res.skipped[0].reason.contains("track"), "{:?}", res.skipped[0]);
    }

    /// A MIDI clip may not land on an audio track — the same rule
    /// `midi_add_clip_core` enforces, applied per clip rather than fatally.
    #[test]
    fn paste_skips_a_midi_clip_targeting_an_audio_track() {
        let cp = plane(); // t-1 and t-2 are both kind "audio" in this harness
        cp.commit(TxMeta::user("seed"), |tx| {
            tx.apply(Op::MidiClipAdd {
                clip: crate::control::tests::dummy_midi_clip("t-1"),
                index: 0,
            })
        })
        .unwrap();
        let mid = cp.session().lock().midi.clips[0].id.to_string();
        let payload = cp.clips_copy(vec![], vec![mid]).unwrap();

        let res = cp.clips_paste(paste_req(payload, 0, false)).unwrap();
        assert!(res.midi_clips.is_empty());
        assert_eq!(res.skipped.len(), 1);
        assert!(res.skipped[0].reason.contains("midi"), "{:?}", res.skipped[0]);
    }

    /// A payload with an audio clip this machine cannot resolve is skipped;
    /// the MIDI clips in the same payload still land, in the same one
    /// transaction (scope ruling B).
    #[test]
    fn paste_skips_an_unresolvable_audio_clip_but_still_lands_the_midi_clips() {
        let cp = plane();
        cp.commit(TxMeta::user("seed"), |tx| {
            tx.apply(Op::ClipAdd {
                clip: crate::audio::types::testutil::test_clip("a-1", "t-1"),
                index: 0,
            })?;
            tx.apply(Op::MidiClipAdd {
                clip: crate::control::tests::dummy_midi_clip("t-2"),
                index: 0,
            })
        })
        .unwrap();
        let mid = cp.session().lock().midi.clips[0].id.to_string();
        let mut payload = cp.clips_copy(vec!["a-1".into()], vec![mid]).unwrap();
        // this session has no project dir, so `source_abs_path` cannot save it
        if let Some(ClipboardClip::Audio { source_abs_path, source_path, .. }) =
            payload.clips.iter_mut().find(|c| matches!(c, ClipboardClip::Audio { .. }))
        {
            *source_abs_path = Some("/definitely/not/here.wav".into());
            *source_path = "audio/definitely-not-here.wav".into();
        }

        let res = cp.clips_paste(paste_req(payload, 0, false)).unwrap();
        assert_eq!(res.audio_clips.len(), 0);
        assert_eq!(res.midi_clips.len(), 1, "the MIDI half still landed");
        assert_eq!(res.skipped.len(), 1);
        assert!(
            res.skipped[0].reason.contains("audio source") || res.skipped[0].reason.contains("no project"),
            "{:?}",
            res.skipped[0]
        );
    }

    /// A paste where everything skipped commits nothing — and is not an
    /// error, because the user asked for a paste and got a precise report.
    #[test]
    fn paste_where_everything_skips_commits_no_transaction() {
        let cp = plane();
        let (undo_before, _) = cp.history_depths();
        let rev_before = cp.session().lock().rev;
        let payload = AuraClipsPayload {
            mime: AURA_CLIPS_MIME.into(),
            schema_version: AURA_CLIPS_SCHEMA_VERSION,
            anchor_samples: 0,
            anchor_ticks: 0,
            ppq: 960,
            source_project_dir: None,
            clips: vec![ClipboardClip::Midi {
                name: "orphan".into(),
                source_track_id: "gone".into(),
                source_track_name: "Gone".into(),
                offset_from_anchor_ticks: 0,
                length_ticks: 960,
                content_length_ticks: None,
                notes: vec![],
            }],
        };

        let res = cp.clips_paste(paste_req(payload, 0, false)).unwrap();
        assert_eq!(res.skipped.len(), 1);
        assert_eq!(cp.history_depths().0, undo_before, "nothing committed");
        assert_eq!(cp.session().lock().rev, rev_before);
    }

    /// A payload from a future schema version is refused with a message a
    /// human can act on, rather than deserializing into nonsense.
    #[test]
    fn paste_rejects_a_newer_schema_version() {
        let cp = plane();
        let payload = AuraClipsPayload {
            mime: AURA_CLIPS_MIME.into(),
            schema_version: AURA_CLIPS_SCHEMA_VERSION + 1,
            anchor_samples: 0,
            anchor_ticks: 0,
            ppq: 960,
            source_project_dir: None,
            clips: vec![],
        };
        let err = cp.clips_paste(paste_req(payload, 0, false)).unwrap_err();
        assert!(err.contains("schema"), "{err}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml clipboard::`
Expected: FAIL — `cannot find type PasteRequest`, `no method named clips_paste`.

- [ ] **Step 3: Write the implementation**

`ControlPlane::clips_paste`, in phases — **the phase boundary is the point of
this task**:

**Phase 1 — validate the envelope (no lock).**
`payload.mime != AURA_CLIPS_MIME` → `Err("not an AURA clips payload")`;
`payload.schema_version > AURA_CLIPS_SCHEMA_VERSION` →
`Err(format!("clipboard payload schema {} is newer than this build understands ({})", …))`.

**Phase 2 — resolve assets (prepare-outside, no session lock held across
I/O).** Read `store.project_dir` under a short lock, drop it, then for each
`ClipboardClip::Audio` decide one of:
- `Resolved::InPlace(relative_path)` — `project_dir.join(&source_path)` exists;
- `Resolved::Copied { relative_path }` — `source_abs_path` is `Some`, exists,
  and `project_dir` is `Some`: copy into `<project_dir>/audio/` under
  `format!("{}-{}", uuid, original_file_name)` (never overwrite an existing
  file), then use the new project-relative POSIX path;
- `Resolved::Skip(reason)` — anything else: `"missing audio source: <path>"`
  when a path was tried, `"no project open — cannot import <name>"` when
  there is nowhere to copy to.

**Phase 3 — ONE transaction.**
```rust
let mut result = PasteResult { audio_clips: vec![], midi_clips: vec![], created_tracks: vec![], skipped };
if nothing_to_place { return Ok(result); }   // no commit at all
self.commit(op::TxMeta::user("paste clips"), |tx| {
    // 3a. paste-to-new-tracks: one add_track_tx per DISTINCT source track,
    //     in first-seen order, named "<sourceTrackName> copy" and kind-
    //     matched ("midi" when any clip from that source track is MIDI).
    // 3b. per clip, resolve the target track id from the map; validate it
    //     exists and (for MIDI) is kind "midi" — a clip that fails moves to
    //     `skipped` HERE ONLY IF to_new_tracks is false (a created track
    //     cannot fail). Validation before every tx.apply, no panics.
    // 3c. audio: build a Clip with a fresh ClipId::mint(), fresh
    //     ContentId::mint(), LaneId::default_for_track(target),
    //     SourceId::default() (or a fresh one when the file was copied),
    //     timeline_start_samples = clamp0(at_samples as i64 + offset),
    //     every other field copied from the payload; tx.apply(Op::ClipAdd
    //     { clip, index: tx.store().clips.len() }).
    // 3d. midi: build a MidiClip with a fresh ClipId::mint(), fresh
    //     ContentId::mint(), LaneId::default_for_track(target),
    //     next_note_id: 1, notes copied from the payload (note_id already
    //     the mint sentinel), timeline_start_ticks =
    //     clamp0(tick_at_sample(at_samples) as i64 + offset_ticks); then
    //     `clip.ensure_note_ids()?` BEFORE the apply — the exact
    //     hum.rs::commit_hum_clip sequence; tx.apply(Op::MidiClipAdd { … }).
    Ok(())
})?;
Ok(result)
```
Collect the created rows into `result` inside the closure (a `&mut` captured
by the closure, as `midi_add_clip_core` does with its `result` local).

Notes for the implementer:
- **`tx.store()` / `tx.midi()` give the in-transaction view** — read the
  insert index from there, not from a pre-lock snapshot, or two clips in the
  same batch get the same index.
- **`ensure_note_ids` before `apply`**, so the op carries final ids and the
  journal is self-describing.
- **`clamp0`** is `|v: i64| v.max(0) as u64`. A paste at the very start with
  a negative offset must not underflow.
- The `skipped` vec must be built in Phase 2 and appended to in Phase 3b;
  return it either way.

The Tauri wrapper and registration:

```rust
/// Paste an `application/x-aura-clips` payload at `atSamples` — ONE
/// transaction (one undo step) composing the existing TrackAdd/ClipAdd/
/// MidiClipAdd ops. Clips whose track or audio source cannot be resolved on
/// this machine are reported in `skipped`, never silently dropped and never
/// fatal to the rest of the paste. Additive command.
#[tauri::command]
pub fn clips_paste(
    request: PasteRequest,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<PasteResult, String> {
    control.clips_paste(request)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml clipboard::`
Expected: PASS, 16 tests (7 from Task 8 + 9 here).

- [ ] **Step 5: Run the whole backend suite and update the dated counts**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 549 total. Update `README.md:389` and `CONTRIBUTING.md:62` to 549.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/control/clipboard.rs src-tauri/src/lib.rs README.md CONTRIBUTING.md
git commit -m "feat(control): clips_paste — one transaction composing TrackAdd/ClipAdd/MidiClipAdd, per-clip skip reporting"
```

---

### Task 10: OS clipboard commands

Two thin additive commands over `arboard`. Scope ruling I explains why not
`navigator.clipboard` (WebKitGTK grants no `clipboard-read`, which is
exactly the cross-instance paste case) and why not
`tauri-plugin-clipboard-manager` (it would need an edit to
`src-tauri/capabilities/default.json`, a **FROZEN FILE** by its own header).

**Files:**
- Create: `src-tauri/src/osclipboard.rs`
- Modify: `src-tauri/Cargo.toml` (add `arboard = "3"` to `[dependencies]`)
- Modify: `src-tauri/src/lib.rs` (`mod osclipboard;` + two registrations)

**Interfaces:**
- Consumes: nothing from this plan.
- Produces:
  `#[tauri::command] pub fn os_clipboard_write_text(text: String) -> Result<(), String>`
  and `#[tauri::command] pub fn os_clipboard_read_text() -> Result<String, String>`.

- [ ] **Step 1: Add the dependency**

In `src-tauri/Cargo.toml`'s `[dependencies]`, after the `tauri-plugin-*`
lines:

```toml
# OS clipboard text slot for cross-instance clip copy/paste (Track C). Not
# the Tauri clipboard plugin: adding that would need an edit to
# capabilities/default.json, which is a frozen file.
arboard = "3"
```

Run: `timeout 900 cargo build --manifest-path src-tauri/Cargo.toml`
Expected: builds; note the resolved `arboard` version in the commit body.

- [ ] **Step 2: Write the module**

Create `src-tauri/src/osclipboard.rs`:

```rust
//! The OS clipboard's TEXT slot, for cross-instance clip copy/paste.
//!
//! WHY A COMMAND AND NOT THE WEBVIEW: `navigator.clipboard.readText()` needs
//! a `clipboard-read` permission WebKitGTK does not grant, and reading is
//! precisely the cross-instance case this exists for. WHY NOT
//! `tauri-plugin-clipboard-manager`: registering a plugin requires an edit
//! to `capabilities/default.json`, which is a frozen file.
//!
//! WHY TEXT ONLY: the slot a Tauri v2 desktop app can portably own is plain
//! text. The `application/x-aura-clips` MIME name therefore rides INSIDE the
//! JSON envelope (see `control::clipboard`), not as a clipboard flavor —
//! which is also why SMF interchange is an export-to-file action rather than
//! a second clipboard flavor.
//!
//! NOT UNIT-TESTED on purpose: a headless test environment has no clipboard,
//! so a test here would either be skipped or flaky. The CODEC on both sides
//! (`control::clipboard`'s payload tests, `src/lib/utils/aura-clips.test.ts`)
//! is what carries the coverage.

/// Put `text` on the OS clipboard.
#[tauri::command]
pub fn os_clipboard_write_text(text: String) -> Result<(), String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| format!("clipboard unavailable: {e}"))?;
    cb.set_text(text).map_err(|e| format!("clipboard write failed: {e}"))
}

/// Read the OS clipboard's text slot. An empty or non-text clipboard is an
/// empty string, not an error — "nothing to paste" is a normal state and the
/// caller falls back to its in-memory clipboard.
#[tauri::command]
pub fn os_clipboard_read_text() -> Result<String, String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| format!("clipboard unavailable: {e}"))?;
    Ok(cb.get_text().unwrap_or_default())
}
```

In `src-tauri/src/lib.rs`: add `mod osclipboard;` with the other module
declarations, and register both commands in `generate_handler!`.

- [ ] **Step 3: Verify nothing regressed**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 549, unchanged (this task adds no tests — recorded above, with the
reason). No README/CONTRIBUTING count change.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/osclipboard.rs src-tauri/src/lib.rs src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "feat(clipboard): additive os_clipboard_write_text/read_text commands over arboard"
```

---

### Task 11: Frontend clipboard — copy, paste, paste-to-new-tracks

Ties it together: Ctrl+C builds the payload backend-side and puts it on the
OS clipboard; Ctrl+V prefers a valid AURA envelope from the OS clipboard
over the in-memory one (so a copy in instance A pastes in instance B) and
falls back to in-memory when the clipboard holds something else;
Ctrl+Shift+V pastes onto new tracks.

**Files:**
- Create: `src/lib/utils/aura-clips.ts`
- Test: `src/lib/utils/aura-clips.test.ts`
- Create: `src/lib/state/clip-clipboard.svelte.ts`
- Test: `src/lib/state/clip-clipboard.test.ts`
- Modify: `src/lib/types/ipc.ts` (`AuraClipsPayload`, `ClipboardClip`,
  `PasteRequest`, `PasteResult`, `SkippedClip`)
- Modify: `src/lib/tauri.ts` (`clipsCopy?`, `clipsPaste?`,
  `osClipboardWriteText?`, `osClipboardReadText?`)
- Modify: `src/App.svelte` (Ctrl+C / Ctrl+V / Ctrl+Shift+V)

**Interfaces:**
- Consumes: `clipSelection`, `project`, `midi`, `toasts`, `backend`.
- Produces:
  - `encodeAuraClips(payload: AuraClipsPayload): string` — the magic first
    line `AURA-CLIPS/1` followed by a newline and pretty-free JSON.
  - `parseAuraClips(text: string): AuraClipsPayload | null` — null for
    anything that is not one of ours (never throws).
  - `export const clipClipboard` with `copy(): Promise<void>`,
    `paste(atSamples: number, toNewTracks: boolean): Promise<void>`,
    `payload: AuraClipsPayload | null` (`$state`, the in-memory fallback).

- [ ] **Step 1: Write the failing codec test**

Create `src/lib/utils/aura-clips.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { encodeAuraClips, parseAuraClips } from "./aura-clips";
import type { AuraClipsPayload } from "../types/ipc";

const payload: AuraClipsPayload = {
  mime: "application/x-aura-clips",
  schemaVersion: 1,
  anchorSamples: 48000,
  anchorTicks: 960,
  ppq: 960,
  sourceProjectDir: "/home/u/Songs/Demo.aura",
  clips: [
    {
      kind: "midi",
      name: "riff",
      sourceTrackId: "t1",
      sourceTrackName: "Lead",
      offsetFromAnchorTicks: 0,
      lengthTicks: 1920,
      contentLengthTicks: null,
      notes: [{ tick: 0, lengthTicks: 480, key: 60, velocity: 100, channel: 0, noteId: 0 }],
    },
  ],
};

describe("aura-clips envelope", () => {
  it("round-trips through encode/parse", () => {
    expect(parseAuraClips(encodeAuraClips(payload))).toEqual(payload);
  });

  it("starts with a magic line, so a human pasting it elsewhere sees what it is", () => {
    expect(encodeAuraClips(payload).startsWith("AURA-CLIPS/1\n")).toBe(true);
  });

  it("returns null for unrelated clipboard text instead of throwing", () => {
    expect(parseAuraClips("just some text a user copied")).toBeNull();
    expect(parseAuraClips("")).toBeNull();
    expect(parseAuraClips("AURA-CLIPS/1\n{not json")).toBeNull();
  });

  it("returns null for JSON that is not an AURA payload", () => {
    expect(parseAuraClips('AURA-CLIPS/1\n{"mime":"text/plain","clips":[]}')).toBeNull();
  });

  it("accepts a payload whose schemaVersion is older", () => {
    const older = { ...payload, schemaVersion: 1 };
    expect(parseAuraClips(encodeAuraClips(older))?.schemaVersion).toBe(1);
  });

  it("still parses a NEWER schema version — the backend, not the codec, decides", () => {
    // the backend returns a clear "schema is newer than this build" error;
    // the codec's job is only to recognise our own envelope.
    const newer = { ...payload, schemaVersion: 99 };
    expect(parseAuraClips(encodeAuraClips(newer))?.schemaVersion).toBe(99);
  });
});
```

- [ ] **Step 2: Run it to verify it fails**

Run: `timeout 300 npx vitest run src/lib/utils/aura-clips.test.ts`
Expected: FAIL — cannot resolve `./aura-clips`.

- [ ] **Step 3: Add the IPC types and the codec**

In `src/lib/types/ipc.ts`, mirroring the Rust wire form exactly:

```ts
// ── application/x-aura-clips (Track C) ──────────────────────────────────────

export interface SkippedClip {
  name: string;
  reason: string;
}

export type ClipboardClip =
  | {
      kind: "audio";
      name: string;
      sourceTrackId: string;
      sourceTrackName: string;
      offsetFromAnchorSamples: number;
      lengthSamples: number;
      sourcePath: string;
      sourceAbsPath: string | null;
      sourceChannels: number;
      sourceSampleRate: number;
      sourceLengthSamples: number;
      offsetSamples: number;
      gainDb: number;
      fadeInSamples: number;
      fadeOutSamples: number;
    }
  | {
      kind: "midi";
      name: string;
      sourceTrackId: string;
      sourceTrackName: string;
      offsetFromAnchorTicks: number;
      lengthTicks: number;
      contentLengthTicks: number | null;
      notes: MidiNote[];
    };

export interface AuraClipsPayload {
  mime: string;
  schemaVersion: number;
  anchorSamples: number;
  anchorTicks: number;
  ppq: number;
  sourceProjectDir: string | null;
  clips: ClipboardClip[];
}

export interface PasteRequest {
  payload: AuraClipsPayload;
  atSamples: number;
  toNewTracks: boolean;
}

export interface PasteResult {
  audioClips: Clip[];
  midiClips: MidiClip[];
  createdTracks: TrackState[];
  skipped: SkippedClip[];
}
```

Create `src/lib/utils/aura-clips.ts`:

```ts
/**
 * The `application/x-aura-clips` clipboard envelope.
 *
 * The MIME name lives INSIDE the payload, and the text begins with a magic
 * line, because the OS clipboard slot a Tauri v2 desktop app can portably
 * own is plain text — arbitrary flavors are not available (plan scope ruling
 * C). The magic line also means a human who pastes this into a text editor
 * immediately sees what it is.
 *
 * `parseAuraClips` NEVER throws: the clipboard holds whatever the user last
 * copied, and a paste must degrade to "not ours" rather than error.
 */
import type { AuraClipsPayload } from "../types/ipc";

export const AURA_CLIPS_MIME = "application/x-aura-clips";
const MAGIC = "AURA-CLIPS/1";

export function encodeAuraClips(payload: AuraClipsPayload): string {
  return `${MAGIC}\n${JSON.stringify(payload)}`;
}

export function parseAuraClips(text: string): AuraClipsPayload | null {
  if (!text.startsWith(`${MAGIC}\n`)) return null;
  try {
    const parsed = JSON.parse(text.slice(MAGIC.length + 1)) as unknown;
    if (
      typeof parsed !== "object" ||
      parsed === null ||
      (parsed as AuraClipsPayload).mime !== AURA_CLIPS_MIME ||
      !Array.isArray((parsed as AuraClipsPayload).clips)
    ) {
      return null;
    }
    return parsed as AuraClipsPayload;
  } catch {
    return null;
  }
}
```

- [ ] **Step 4: Run the codec test to verify it passes**

Run: `timeout 300 npx vitest run src/lib/utils/aura-clips.test.ts`
Expected: PASS, 6 tests.

- [ ] **Step 5: Write the failing store test**

Create `src/lib/state/clip-clipboard.test.ts`:

```ts
/**
 * Copy/paste orchestration. The payload is built and consumed BACKEND-side
 * (ADR 0006); this store only routes it, prefers the OS clipboard over its
 * own memory so a second instance can paste, and surfaces skipped clips.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { AuraClipsPayload, Clip, MidiClip } from "../types/ipc";

const payload: AuraClipsPayload = {
  mime: "application/x-aura-clips",
  schemaVersion: 1,
  anchorSamples: 0,
  anchorTicks: 0,
  ppq: 960,
  sourceProjectDir: null,
  clips: [],
};

const clipsCopy = vi.fn(async () => payload);
const clipsPaste = vi.fn(async () => ({
  audioClips: [] as Clip[],
  midiClips: [] as MidiClip[],
  createdTracks: [],
  skipped: [],
}));
let clipboardText = "";
const osClipboardWriteText = vi.fn(async (t: string) => {
  clipboardText = t;
});
const osClipboardReadText = vi.fn(async () => clipboardText);

vi.mock("../tauri", () => ({
  backend: {
    on: () => () => {},
    clipsCopy: (a: string[], m: string[]) => clipsCopy(a as never, m as never),
    clipsPaste: (r: unknown) => clipsPaste(r as never),
    osClipboardWriteText: (t: string) => osClipboardWriteText(t),
    osClipboardReadText: () => osClipboardReadText(),
    getProjectState: () =>
      Promise.resolve({ ppq: 960, tempoEvents: [{ tick: 0, bpm: 120 }], midiClips: [] }),
  },
}));

const { project } = await import("./project.svelte");
const { midi } = await import("./midi.svelte");
const { clipSelection } = await import("./clip-selection.svelte");
const { toasts } = await import("./toasts.svelte");
const { clipClipboard } = await import("./clip-clipboard.svelte");

beforeEach(() => {
  vi.clearAllMocks();
  clipboardText = "";
  project.clips = [{ id: "a1", trackId: "t1" } as Clip];
  midi.clips = [{ id: "m1", trackId: "t2" } as MidiClip];
  clipSelection.clear();
  clipClipboard.payload = null;
});

describe("clipClipboard.copy", () => {
  it("passes the selection's ids, split by store, and does nothing when empty", async () => {
    await clipClipboard.copy();
    expect(clipsCopy).not.toHaveBeenCalled();

    clipSelection.apply(
      [
        { kind: "audio", id: "a1" },
        { kind: "midi", id: "m1" },
      ],
      "replace",
    );
    await clipClipboard.copy();
    expect(clipsCopy).toHaveBeenCalledWith(["a1"], ["m1"]);
  });

  it("keeps the payload in memory AND writes the envelope to the OS clipboard", async () => {
    clipSelection.apply([{ kind: "audio", id: "a1" }], "replace");
    await clipClipboard.copy();
    expect(clipClipboard.payload).toEqual(payload);
    expect(clipboardText.startsWith("AURA-CLIPS/1\n")).toBe(true);
  });

  it("still keeps the in-memory payload when the OS clipboard write fails", async () => {
    osClipboardWriteText.mockRejectedValueOnce(new Error("no display"));
    clipSelection.apply([{ kind: "audio", id: "a1" }], "replace");
    await clipClipboard.copy();
    expect(clipClipboard.payload).toEqual(payload);
  });
});

describe("clipClipboard.paste", () => {
  it("prefers a valid AURA envelope on the OS clipboard over its own memory", async () => {
    const foreign = { ...payload, anchorSamples: 12345 };
    clipboardText = `AURA-CLIPS/1\n${JSON.stringify(foreign)}`;
    clipClipboard.payload = payload;
    await clipClipboard.paste(96000, false);
    expect(clipsPaste).toHaveBeenCalledWith({
      payload: foreign,
      atSamples: 96000,
      toNewTracks: false,
    });
  });

  it("falls back to the in-memory payload when the clipboard holds something else", async () => {
    clipboardText = "some unrelated text";
    clipClipboard.payload = payload;
    await clipClipboard.paste(0, false);
    expect(clipsPaste).toHaveBeenCalledWith({ payload, atSamples: 0, toNewTracks: false });
  });

  it("does nothing at all when there is no payload anywhere", async () => {
    await clipClipboard.paste(0, false);
    expect(clipsPaste).not.toHaveBeenCalled();
  });

  it("passes toNewTracks through", async () => {
    clipClipboard.payload = payload;
    await clipClipboard.paste(0, true);
    expect(clipsPaste).toHaveBeenCalledWith({ payload, atSamples: 0, toNewTracks: true });
  });

  it("applies the pasted rows to the stores and selects exactly them", async () => {
    clipsPaste.mockResolvedValueOnce({
      audioClips: [{ id: "new-a", trackId: "t1" } as Clip],
      midiClips: [{ id: "new-m", trackId: "t2" } as MidiClip],
      createdTracks: [],
      skipped: [],
    });
    clipSelection.apply([{ kind: "audio", id: "a1" }], "replace");
    clipClipboard.payload = payload;
    await clipClipboard.paste(0, false);
    expect(project.clips.some((c) => c.id === "new-a")).toBe(true);
    expect(midi.clips.some((c) => c.id === "new-m")).toBe(true);
    expect(clipSelection.refs()).toEqual([
      { kind: "audio", id: "new-a" },
      { kind: "midi", id: "new-m" },
    ]);
  });

  it("tells the user about skipped clips instead of losing them silently", async () => {
    clipsPaste.mockResolvedValueOnce({
      audioClips: [],
      midiClips: [],
      createdTracks: [],
      skipped: [{ name: "drums", reason: "missing audio source: audio/x.wav" }],
    });
    const before = toasts.items.length;
    clipClipboard.payload = payload;
    await clipClipboard.paste(0, false);
    expect(toasts.items.length).toBeGreaterThan(before);
  });
});
```

(If `toasts.items` is not the store's array name, read
`src/lib/state/toasts.svelte.ts` and use the real one — do not add a field
to the toasts store for the test's convenience.)

- [ ] **Step 6: Run it to verify it fails**

Run: `timeout 300 npx vitest run src/lib/state/clip-clipboard.test.ts`
Expected: FAIL — cannot resolve `./clip-clipboard.svelte`.

- [ ] **Step 7: Add the adapter methods and write the store**

In `src/lib/tauri.ts`'s `Backend` interface (all optional, real-engine only):

```ts
  /** Build the application/x-aura-clips payload for these clips — a pure
   * backend READ. Selection stays frontend-side; the backend is told WHICH
   * clips, never "what is selected". */
  clipsCopy?(audioClipIds: string[], midiClipIds: string[]): Promise<AuraClipsPayload>;
  /** Paste a payload at `atSamples` as ONE transaction (one undo step). */
  clipsPaste?(request: PasteRequest): Promise<PasteResult>;
  /** OS clipboard text slot (cross-instance copy/paste). */
  osClipboardWriteText?(text: string): Promise<void>;
  osClipboardReadText?(): Promise<string>;
```

and in `TauriBackend`:

```ts
  clipsCopy(audioClipIds: string[], midiClipIds: string[]) {
    return invoke<AuraClipsPayload>("clips_copy", { audioClipIds, midiClipIds });
  }
  clipsPaste(request: PasteRequest) {
    return invoke<PasteResult>("clips_paste", { request });
  }
  osClipboardWriteText(text: string) {
    return invoke<void>("os_clipboard_write_text", { text });
  }
  osClipboardReadText() {
    return invoke<string>("os_clipboard_read_text");
  }
```

Create `src/lib/state/clip-clipboard.svelte.ts`:

```ts
/**
 * Multi-clip copy/paste orchestration.
 *
 * The PAYLOAD is built and consumed backend-side (`clips_copy`/`clips_paste`)
 * — that is what keeps the in-app and cross-instance paths byte-identical
 * and keeps every placement decision out of the renderer (ADR 0006). This
 * store only routes: it splits the selection into two id lists, parks the
 * payload in memory, mirrors it onto the OS clipboard, and on paste PREFERS
 * a valid AURA envelope found on the OS clipboard (so a copy in one AURA
 * instance pastes in another) over its own memory.
 */

import { backend } from "../tauri";
import { encodeAuraClips, parseAuraClips } from "../utils/aura-clips";
import { clipSelection } from "./clip-selection.svelte";
import { project } from "./project.svelte";
import { midi } from "./midi.svelte";
import { toasts } from "./toasts.svelte";
import type { AuraClipsPayload } from "../types/ipc";

class ClipClipboardStore {
  /** In-memory fallback: what THIS window last copied. The OS clipboard is
   * the cross-instance channel; this is the one that always works. */
  payload = $state<AuraClipsPayload | null>(null);

  async copy(): Promise<void> {
    const audioIds = clipSelection.audioIds();
    const midiIds = clipSelection.midiIds();
    if (audioIds.length === 0 && midiIds.length === 0) return;
    try {
      const p = await backend.clipsCopy?.(audioIds, midiIds);
      if (!p) return;
      this.payload = p;
      try {
        await backend.osClipboardWriteText?.(encodeAuraClips(p));
      } catch (err) {
        // A machine without a usable clipboard still gets in-app paste.
        console.warn("[aura] OS clipboard write failed:", err);
      }
    } catch (err) {
      console.error("[aura] clips_copy failed:", err);
      toasts.error("COPY FAILED", String(err));
    }
  }

  async paste(atSamples: number, toNewTracks: boolean): Promise<void> {
    let payload = this.payload;
    try {
      const text = await backend.osClipboardReadText?.();
      const fromOs = text ? parseAuraClips(text) : null;
      if (fromOs) payload = fromOs;
    } catch (err) {
      console.warn("[aura] OS clipboard read failed:", err);
    }
    if (!payload) return;
    try {
      const res = await backend.clipsPaste?.({
        payload,
        atSamples: Math.max(0, Math.round(atSamples)),
        toNewTracks,
      });
      if (!res) return;
      for (const t of res.createdTracks) project.tracks = [...project.tracks, t];
      for (const c of res.audioClips) project.upsertClip(c);
      if (res.midiClips.length > 0) midi.clips = [...midi.clips, ...res.midiClips];
      clipSelection.apply(
        [
          ...res.audioClips.map((c) => ({ kind: "audio" as const, id: c.id })),
          ...res.midiClips.map((c) => ({ kind: "midi" as const, id: c.id })),
        ],
        "replace",
      );
      if (res.skipped.length > 0) {
        toasts.error(
          "SOME CLIPS COULD NOT BE PASTED",
          res.skipped.map((s) => `${s.name}: ${s.reason}`).join("\n"),
        );
      }
    } catch (err) {
      console.error("[aura] clips_paste failed:", err);
      toasts.error("PASTE FAILED", String(err));
    }
  }
}

export const clipClipboard = new ClipClipboardStore();
```

- [ ] **Step 8: Run the store test to verify it passes**

Run: `timeout 300 npx vitest run src/lib/state/clip-clipboard.test.ts`
Expected: PASS, 9 tests.

- [ ] **Step 9: Wire the shortcuts in `App.svelte`**

Add `import { clipClipboard } from "./lib/state/clip-clipboard.svelte";` and
`import { clipSelection } from "./lib/state/clip-selection.svelte";`.

Replace the existing Ctrl+C and Ctrl+V branches (:177-184) with:

```ts
    } else if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "c") {
      // Copy the timeline selection (multi-clip, audio + MIDI). Falls back
      // to the legacy single-clip stamp when nothing is multi-selected, so
      // the old Ctrl+C behaviour never regresses.
      e.preventDefault();
      if (clipSelection.count() > 0) void clipClipboard.copy();
      else midi.copySelected();
    } else if ((e.ctrlKey || e.metaKey) && !e.altKey && e.key.toLowerCase() === "v") {
      // Paste at the playhead: Shift pastes onto NEW tracks.
      e.preventDefault();
      if (clipClipboard.payload || clipSelection.count() > 0) {
        void clipClipboard.paste(playhead(), e.shiftKey);
      } else {
        void midi.pasteAtPlayhead(midi.samplesToTicks(playhead()));
      }
    }
```

Note the Ctrl+V branch drops `!e.shiftKey` so Ctrl+Shift+V reaches it. Leave
the Ctrl+D branch exactly as it is (scope ruling F). Note also that
`clipClipboard.paste` reads the OS clipboard first, so a paste in a fresh
window with an empty in-memory payload still works — the
`clipClipboard.payload ||` guard is about which HANDLER runs, not whether a
paste can happen; when neither condition holds the legacy single-clip stamp
runs, which is the pre-existing behaviour.

- [ ] **Step 10: Discoverability — the paste-to-new-tracks affordance**

The owner's requirement 3 asks for "some explicit indication in the UI".
Add a hint to the timeline's ruler `title` attribute and a one-line status
in `Timeline.svelte`'s corner cell, so the modifier is discoverable without
a menu:

```svelte
    <div class="corner silk" title="Ctrl+C copy · Ctrl+V paste at playhead · Ctrl+Shift+V paste onto new tracks">
      {clipSelection.count() > 0
        ? `${clipSelection.count()} clip${clipSelection.count() === 1 ? "" : "s"} selected`
        : `tracks · ${project.tracks.length}`}
    </div>
```

- [ ] **Step 11: Type-check and run the whole frontend suite**

Run: `npx svelte-check --threshold error`
Run: `timeout 300 npx vitest run`
Expected: 262 passed (247 + 6 + 9). Update `README.md:390` to 262.

- [ ] **Step 12: Manual verification — the cross-instance claim**

This is the one claim no test in this repo can make. Launch TWO AURA
instances (the second will log an MCP port-41717 collision — expected, and
explicitly out of scope). In instance A select two clips and press Ctrl+C;
in instance B press Ctrl+V. MIDI clips must land with their notes; audio
clips referencing a project B does not have must appear in a toast listing
the skip reason, not vanish. Record the result in the task's review notes.

**BINDING RULE (fix round 2, Task 11): do not attempt this step by driving
the real app's keyboard/mouse with input-automation tooling (`xdotool` and
similar), and do not retry it after a first inconclusive attempt.** An
implementer tried exactly this in a real X11 + `gpaste-daemon` desktop
(matching Task 10's own environment) and found the sandbox's X display
concurrently driven by another agent session — focus jumped to unrelated
windows mid-test, a keystroke silently queued and fired minutes later, one
attempt toggled a dock panel and duplicated demo tracks that no action in
this task's own sequence explains. A green result from a headless `Xvfb`
run would be **weaker than the unit tests, not stronger**: Xvfb has no
clipboard manager, which is exactly the configuration where the OS-clipboard
transfer already works reliably (Task 10's own measurement) — a pass there
answers a question nobody asked and retires this step without answering it.
Driving `xclip` from a shell proves only what Task 10 already proved
(the `arboard`/X11 mechanism works cross-process); it says nothing about
whether instance B's `os_clipboard_read_text` — reached through this task's
own frontend orchestration — receives instance A's bytes on a real desktop.

**Exactly one link is unproven and belongs to the owner, on a real desktop,
by hand:** that instance B's Ctrl+V (via `clipClipboard.pasteAtPlayhead`)
actually receives and applies instance A's Ctrl+C payload. The owed
procedure:
1. Launch instance A and instance B (`AURA_SIDECAR_SIMULATE=1`, each on its
   own devUrl/dev port if both run from the same worktree — the fixed
   port 1420 in `vite.config.ts`/`tauri.conf.json` only allows one).
2. In A: "+ LOAD DEMO SONG", click the "demo lead" MIDI clip once (the
   corner cell should read "1 clip selected"), press Ctrl+C.
3. In B (a project with at least one MIDI track, e.g. its own loaded demo
   song): press Ctrl+V at the playhead.
4. PASS: the clip's notes land in B, on its own MIDI track (or the source
   track's name if B has no matching track — check `clipsPaste`'s track
   resolution, scope ruling B). FAIL modes to record precisely, not just
   "didn't work": a "NOTHING TO PASTE" toast in B (the OS-clipboard read
   found nothing — check the write actually landed, e.g. via `xclip -o
   -selection clipboard` run AFTER step 2, in a shell, BEFORE step 3), a
   "SOME CLIPS COULD NOT BE PASTED" toast (track-kind mismatch or missing
   audio source — expected and correct for an audio clip B's project
   lacks), or silence with no toast and no clip (a real bug — the
   `pasteAtPlayhead` fallback chain has nowhere left to go quiet).
5. Repeat once more with a MULTI-clip selection (marquee two-plus clips in
   A) to also confirm the anchor/offset math survives the OS clipboard,
   not just single-clip content.

- [ ] **Step 13: Commit**

```bash
git add src/lib/utils/aura-clips.ts src/lib/utils/aura-clips.test.ts \
        src/lib/state/clip-clipboard.svelte.ts src/lib/state/clip-clipboard.test.ts \
        src/lib/types/ipc.ts src/lib/tauri.ts src/App.svelte \
        src/lib/components/Timeline.svelte README.md
git commit -m "feat(timeline): multi-clip copy/paste at the playhead, cross-instance via the OS clipboard"
```

---

### Task 12: Export the selection as a MIDI file (the SMF half)

Scope ruling C: SMF is export-only, to a file, and needs **no new backend
command** — the frozen `midi_export_file(path, clipIds)` already takes a
clip-id filter.

**Files:**
- Modify: `src/lib/state/clip-clipboard.svelte.ts` (add `exportSelectionSmf`)
- Modify: `src/lib/state/clip-clipboard.test.ts` (append a `describe`)
- Modify: `src/App.svelte` (Ctrl+Shift+M)

**Interfaces:**
- Consumes: `backend.midiExportFile(path, clipIds)`,
  `@tauri-apps/plugin-dialog`'s `save` (`dialog:default` already grants
  `allow-save`; the frozen capability file needs no edit).
- Produces: `clipClipboard.exportSelectionSmf(): Promise<void>`.

- [ ] **Step 1: Write the failing test**

Append to `src/lib/state/clip-clipboard.test.ts`:

```ts
describe("clipClipboard.exportSelectionSmf", () => {
  it("does nothing when no MIDI clip is selected", async () => {
    clipSelection.apply([{ kind: "audio", id: "a1" }], "replace");
    await clipClipboard.exportSelectionSmf();
    expect(midiExportFile).not.toHaveBeenCalled();
  });

  it("exports exactly the selected MIDI clip ids to the chosen path", async () => {
    savePath = "/tmp/riff.mid";
    clipSelection.apply(
      [
        { kind: "midi", id: "m1" },
        { kind: "audio", id: "a1" },
      ],
      "replace",
    );
    await clipClipboard.exportSelectionSmf();
    expect(midiExportFile).toHaveBeenCalledWith("/tmp/riff.mid", ["m1"]);
  });

  it("does nothing when the save dialog is cancelled", async () => {
    savePath = null;
    clipSelection.apply([{ kind: "midi", id: "m1" }], "replace");
    await clipClipboard.exportSelectionSmf();
    expect(midiExportFile).not.toHaveBeenCalled();
  });
});
```

with these additions at the top of the file, next to the other mocks:

```ts
const midiExportFile = vi.fn(async (path: string) => path);
let savePath: string | null = "/tmp/out.mid";
vi.mock("@tauri-apps/plugin-dialog", () => ({ save: async () => savePath }));
```

and `midiExportFile: (p: string, ids: string[] | null) => midiExportFile(p, ids as never)`
added to the mocked `backend` object.

- [ ] **Step 2: Run it to verify it fails**

Run: `timeout 300 npx vitest run src/lib/state/clip-clipboard.test.ts`
Expected: FAIL — `clipClipboard.exportSelectionSmf is not a function`.

- [ ] **Step 3: Implement it**

Add to `ClipClipboardStore`:

```ts
  /**
   * Export the selected MIDI clips as a format-1 .mid file — the
   * interchange half of "copy", for DAWs that will never understand the
   * AURA envelope. EXPORT-ONLY on purpose (plan scope ruling C): the OS
   * clipboard slot this app can own is text, so SMF cannot ride on the
   * clipboard, and paste therefore never parses SMF.
   *
   * Reuses the FROZEN `midi_export_file(path, clipIds)` command — its
   * clip-id filter is exactly this feature.
   */
  async exportSelectionSmf(): Promise<void> {
    const ids = clipSelection.midiIds();
    if (ids.length === 0) return;
    const { save } = await import("@tauri-apps/plugin-dialog");
    const path = await save({
      title: "Export selection as MIDI",
      defaultPath: "selection.mid",
      filters: [{ name: "MIDI", extensions: ["mid"] }],
    });
    if (!path) return;
    try {
      await backend.midiExportFile(path, ids);
      toasts.info?.("EXPORTED", path);
    } catch (err) {
      console.error("[aura] midi_export_file failed:", err);
      toasts.error("MIDI EXPORT FAILED", String(err));
    }
  }
```

(If the toasts store has no `info`, use whatever the success-toast method is
— read `src/lib/state/toasts.svelte.ts` and match it; do not add a method.)

- [ ] **Step 4: Run it to verify it passes**

Run: `timeout 300 npx vitest run src/lib/state/clip-clipboard.test.ts`
Expected: PASS, 12 tests.

- [ ] **Step 5: Wire Ctrl+Shift+M in `App.svelte`**

In the guarded (non-text-entry) section, next to the Ctrl+C/V branches:

```ts
    } else if ((e.ctrlKey || e.metaKey) && e.shiftKey && !e.altKey && e.key.toLowerCase() === "m") {
      // Export the selected MIDI clips as a .mid file — the interchange
      // half of copy, for other DAWs (SMF never rides on the clipboard).
      e.preventDefault();
      void clipClipboard.exportSelectionSmf();
    }
```

and extend the corner cell's `title` from Task 11 with
` · Ctrl+Shift+M export selection as MIDI`.

- [ ] **Step 6: Type-check and run the whole frontend suite**

Run: `npx svelte-check --threshold error`
Run: `timeout 300 npx vitest run`
Expected: 265 passed. Update `README.md:390` to 265.

- [ ] **Step 7: Commit**

```bash
git add src/lib/state/clip-clipboard.svelte.ts src/lib/state/clip-clipboard.test.ts \
        src/App.svelte src/lib/components/Timeline.svelte README.md
git commit -m "feat(timeline): export the selection as a MIDI file, reusing the frozen midi_export_file filter"
```

---

### Task 13: Close-out — docs, counts, and the next session's pointers

**Files:**
- Modify: `docs/backlog/multi-clip-selection-and-paste.md`
- Modify: `next-prompt.md` (Track C's entry, §3)
- Modify: `README.md`, `CONTRIBUTING.md` (final count reconciliation)

**Interfaces:** none.

- [ ] **Step 1: Run BOTH suites, foreground, and record the real numbers**

```
timeout 900 cargo test --manifest-path src-tauri/Cargo.toml
timeout 300 npx vitest run
```
Expected: **549 backend + 265 frontend**, all green. If either differs,
find out why before writing a number down — a count that moved without a
task explaining it is a signal, not a typo.

- [ ] **Step 2: Reconcile the dated counts**

Set `README.md:389` / `CONTRIBUTING.md:62` to the real backend number and
`README.md:390` to the real frontend number, both dated `2026-08-14`
(or the execution date). Extend README:389's description list with
`, multi-clip clipboard`.

- [ ] **Step 3: Mark the backlog doc landed**

At the top of `docs/backlog/multi-clip-selection-and-paste.md`, under the
title, add:

```markdown
> **LANDED 2026-08-14** (Track C, branch `multiclip-clipboard`). Plan:
> `docs/superpowers/plans/2026-08-14-multiclip-selection-paste.md` — read its
> scope rulings before changing any of this. Deviations from the design notes
> below, all recorded there rather than silently taken:
>
> - **Plain copy, not instancing** — a paste mints a fresh `ContentId` per
>   clip (the doc's "decide at implementation time" question, decided).
> - **No new op or batch paste op** — paste composes the existing
>   `TrackAdd`/`ClipAdd`/`MidiClipAdd` in ONE transaction, so
>   `OP_FORMAT_VERSION` stays 2.
> - **SMF is export-only, to a file** — the OS clipboard slot a Tauri v2
>   desktop app can own is plain text, so SMF cannot ride on the clipboard
>   and paste never parses it. The `application/x-aura-clips` MIME name
>   lives inside the JSON envelope.
> - **Audio travels by reference**, with a three-step resolution on paste
>   (in-project → copy-in from an absolute path → skip-and-report).
> - **Group resize is MIDI-only**; audio clips have no shipped resize
>   gesture yet.
> - **Multi-clip delete/duplicate and arrow-key nudge stay single-clip** —
>   not in Track C's brief.
```

- [ ] **Step 4: Update `next-prompt.md`'s Track C entry**

Replace the Track C section's body (keep the heading) with a landed
summary: the branch, the plan doc, the five new commands (`move_clips`,
`clips_copy`, `clips_paste`, `os_clipboard_write_text`,
`os_clipboard_read_text`), the new baseline (549 + 265), the scope rulings
that other tracks must not contradict (no new op kind; `OP_FORMAT_VERSION`
still 2; selection is viewer state), and the follow-ons this plan
deliberately left: multi-clip delete/duplicate, arrow-key nudge over a
selection, audio clip resize, true content instancing, SMF paste, and the
MCP port-41717 collision that real two-instance workflows still need.

- [ ] **Step 5: Commit**

```bash
git add docs/backlog/multi-clip-selection-and-paste.md next-prompt.md README.md CONTRIBUTING.md
git commit -m "docs: record Track C's landed shape, scope rulings and follow-ons"
```

---

## Self-review

Run this checklist against the spec before handing the plan to an executor.

**1. Spec coverage — the owner's three requirements + the two extensions:**

| Spec item | Task |
|---|---|
| Multi-select MIDI clips on the timeline | 1, 2, 3, 4 |
| Drag them TOGETHER with individual offsets preserved | 5, 6 |
| Adjust the loop length of a group | 5, 7 |
| Copy across tracks; paste at the playhead, offsets intact, same tracks | 8, 9, 11 |
| Paste onto NEW tracks, with an explicit UI indication | 9 (backend), 11 (Ctrl+Shift+V + the corner hint) |
| `application/x-aura-clips` JSON payload on the OS clipboard | 8, 10, 11 |
| SMF fallback | 12 (export-only — scope ruling C) |
| One undo entry per gesture / per paste | 5 (tests 1, 4), 9 (tests 1, 4) |
| Audio + MIDI mixed in one transaction | 5 (test 1), 9 (test 1) |
| Selection never lands in the document | 2 (the store), Global Constraints |

**2. Design questions the brief demanded be settled up front:** (a) → scope
ruling A; (b) → scope ruling B; (c) → scope ruling C; (d) → scope ruling D.
All four are rulings, not mid-task discoveries.

**3. Type consistency (checked across tasks):** `ClipRef`/`ClipKind` (Task 1)
are used unchanged in 2, 3, 4, 6, 11. `ClipPlacement` has the same field
names in Rust (Task 5) and TS (Task 6): `kind`/`clipId`/
`timelineStartSamples`/`timelineStartTicks`/`lengthTicks`/
`contentLengthTicks`. `AuraClipsPayload`/`ClipboardClip` field names match
between Task 8's Rust `#[serde(rename_all = "camelCase")]` and Task 11's TS
interfaces (`schemaVersion`, `anchorSamples`, `anchorTicks`,
`sourceProjectDir`, `offsetFromAnchorSamples`, `offsetFromAnchorTicks`,
`sourceAbsPath`). `PasteRequest`/`PasteResult`/`SkippedClip` likewise.
`clipDrag`'s `begin/move/end/cancel/computeDelta/mode/active/edgeHoverActive`
are used consistently in 6, 7 and both clip components.

**4. Placeholder scan:** no "TBD", no "add error handling", no "similar to
Task N", no test described without its code. Task 9's Step 3 is the one
place the implementation is given as a commented phase structure rather than
literal Rust — deliberate: its body is ~200 lines of row construction whose
every field is enumerated in the Interfaces block and pinned by nine tests,
and spelling it out verbatim would make the plan longer than the code
without making it more determinate.

**5. Constraint audit:** no new op kind (rulings A, and the Global
Constraint); `OP_FORMAT_VERSION` untouched; no `engine.rs` in any Files
block; every commit closure validates first; the only `.transient()` is
`move_clips`'s gesture fold; `set_track_mix`'s lock shape copied rather than
re-derived; every test run foreground and `timeout`-guarded; every
count-changing task updates the dated counts.

## Risks and open questions

1. **`arboard` on the target platform.** The dependency is new. If it fails
   to build or fails at runtime under Wayland, Task 10's two commands are
   the only casualties: `clipClipboard` already degrades to the in-memory
   payload when `osClipboardWriteText`/`ReadText` reject, so in-app
   copy/paste keeps working and only cross-instance is lost. Fallback if
   needed: `wl-copy`/`xclip` through the already-permitted `shell:allow-execute`.
2. **The `--lib` flake** recorded in Global Constraints. One run failed with
   no identifiable failing test in the captured output; the immediate re-run
   was green at 527. If it recurs, capture
   `cargo test --manifest-path src-tauri/Cargo.toml --lib 2>&1 | grep -A5 '^failures:'`
   before doing anything else.
3. **`test_plane_with_tracks` seeds AUDIO tracks only.** Task 9's
   "MIDI clip onto an audio track is skipped" test depends on that; if the
   harness ever seeds a midi track, that test's premise moves. It asserts on
   the reason string, so it will fail loudly rather than silently pass.
4. **`clips_paste`'s file copy is unbounded work in a command.** It happens
   before the transaction (prepare-outside), so it holds no lock — but it
   does block the command thread. Pasting fifty audio clips from another
   project will feel slow. Accepted for this plan; the fix, if it becomes
   real, is `async` + `spawn_blocking`, mirroring `seed_demo_project`.
5. **Two-instance workflows still hit the hardcoded MCP port 41717.** The
   second instance logs a collision. Out of scope, called out in the backlog
   doc and in Task 11's manual-verification step so nobody reports it as a
   bug introduced here.
6. **Undo depth.** A paste is one entry, but a paste-to-new-tracks entry
   carries full `TrackAdd` rows plus every clip row — for a large selection
   that is a heavy history entry against the 200-entry bound. Bounded and
   acceptable; Plan F owns real retention.

## Execution note

**Executed 2026-08-14/15, subagent-driven** (owner's choice at run start): a
fresh implementer per task, a task review after each, scoped re-reviews on
every fix round; opus for Tasks 9-11 and for the heaviest reviews. Tasks 6,
8, 10, 11 and 12 needed fix rounds (Task 9 needed two, Task 11 two); the
review layer caught four Criticals — the ppq/tempo wire domain, the fatal
duplicate-note-id paste, the unvalidated `source_path`, and cross-instance
paste doing nothing at all in a fresh window.

`origin/main` moved FIVE times during this track and was merged in at the
Task 9/10 boundary (`c26d01d`). The counts the tasks above predict (549 + 265)
are therefore obsolete by construction; the measured baseline at close-out is
**720 backend (691 lib + 29 integration) + 353 frontend**, 2026-08-15
(717 + 348 at the close-out commit, plus the whole-track review's fixes).

**Every mid-flight ruling, deviation, deferred minor and still-owed item is
rolled up in `docs/PHASE4-PLAN.md`'s "Track C handoff" section, per ADR
0007 — including the ONE thing still owed to the owner: Task 11's Step 12
cross-instance verification, which was never performed end to end, together
with the binding no-input-automation rule above.**
