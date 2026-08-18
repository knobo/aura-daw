# DOM Test Environment Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give this repo a jsdom-backed DOM test project (alongside the existing plain-node one) and use it to write the first real, DOM-mounted regression tests for a `.svelte` component's pointer/gesture event handlers — the exact class of code the Track D handoff flags as completely uncovered today.

**Architecture:** Vitest 4's `test.projects` splits the suite in two: the existing `unit` project (plain node, SSR-compiled `.svelte`/`.svelte.ts`, unchanged) and a new `dom` project (`jsdom` environment, `resolve.conditions: ["browser"]` so `.svelte` files compile to the CLIENT target `Svelte.mount()` needs, `*.dom.test.ts` files only). `@testing-library/svelte` mounts real components into jsdom and `fireEvent` dispatches real `PointerEvent`s at them, exercising the actual `onpointerdown`/`onpointermove`/`onpointerup` handlers instead of only the store methods they delegate to.

**Tech Stack:** `jsdom` (bundled environment support already in installed `vitest@4.1.10`, no extra environment package needed), `@testing-library/svelte@^5.4.2` (Svelte 5 support), existing `vitest@^4.1.10` + `@sveltejs/vite-plugin-svelte@^6.0.0`.

**Spec:** `docs/PHASE4-PLAN.md` "Track D handoff" § "WARNING for the next track that touches a component" (the DOM-test-environment gap) and `next-prompt.md`'s Track D leftovers list. No separate design doc — this plan **is** the spec, written after hands-on validation in this repo (see Global Constraints below for the exact pitfalls hit and how they're resolved).

## Global Constraints

- **`environmentMatchGlobs` does not exist in vitest 4** (removed from earlier majors) — do not reach for it. Use `test.projects` (verified present in `node_modules/vitest/dist/chunks/reporters.d.DtoKVV2s.d.ts`, `TestProjectConfiguration`).
- **vite-plugin-svelte compiles `.svelte` files SSR by default under vitest**, which makes `Svelte.mount()` throw `lifecycle_function_unavailable: mount(...) is not available on the server`. The `dom` project MUST set `resolve: { conditions: ["browser"] }` to force the client compile target. Do not set this on the `unit` project — it is unnecessary there and changes no currently-passing test, but is out of scope to verify broadly; keep the two projects isolated.
- **`jsdom` implements no Pointer Capture API at all** (`'setPointerCapture' in canvasElement` is `false`; calling it throws `TypeError: ... is not a function`, not a no-op). Any test that drives the **point-edit** drag path (which calls `canvas.setPointerCapture(e.pointerId)` unconditionally, no optional chaining) MUST stub it first: `Object.defineProperty(canvasEl, "setPointerCapture", { value: vi.fn(), writable: true })`.
- **jsdom's `getBoundingClientRect()` defaults to an all-zero rect**, and elements default to `clientWidth`/`clientHeight` of `0`. Two concrete consequences, both already worked around in Task 2's tests below — do not rediscover them from scratch:
  1. `AutomationTrackRow`'s `canvasPos()` (`src/lib/utils/canvas-pos.ts`) divides by `clientWidth`/`clientHeight` only when they are `> 0`, so with the zero default it falls back to a 1:1 scale and **`{x, y} = {clientX - rect.left, clientY - rect.top} = {clientX, clientY}` exactly** — no stubbing needed to get predictable canvas-local coordinates, `rect.left`/`rect.top` are already `0`.
  2. `onPointerDown`'s **near-right-edge test** (`rect.right - e.clientX <= EDGE_PX`) uses the CLIP DIV's rect, not the canvas's, and with the default zero rect this is `0 - clientX <= 8`, which is **true for every non-negative `clientX`** — every click reads as "near the right edge" (`mode: "resize"`) unless the div's `getBoundingClientRect` is stubbed to a realistic size. Task 2's drag test stubs it to `{ left: 0, top: 0, right: 40, bottom: 20, width: 40, height: 20 }` (matching the clip's rendered `left:0px; width:40px` from the seeded geometry below) so the click resolves to the point-edit path, not resize.
- **`midi.svelte.ts`'s `sectionTable` defaults to `[]`**, and `sampleAtTick`/`tickAtSample` (`src/lib/sectionTable.ts`) both **return `0` unconditionally when the table is empty** — they do not fall back to a synthesized ppq/tempo mapping. Left unseeded, every clip collapses to zero width and **silently renders nothing** (the `{#each}` runs, the `{#if visR > 0 && visL < widthPx}` guard inside it is always false). Every test that mounts `AutomationTrackRow` MUST seed `midi.sectionTable` with at least one row. Use the exact constant below (120 BPM @ 48 kHz, giving 25 samples/tick, matching `project.sampleRate`'s default of `48000` and `project.tempoBpm`'s default of `120` so the numbers stay self-consistent):
  ```ts
  const SECTION_TABLE = [{ startTick: 0, startSample: 0, startBeat: 0, startBar: 0, period: 254_016_000 }];
  ```
- **Every claimed-regression-catching test in this plan has been sabotage-verified** (per the Track D handoff's own convention, which caught a VACUOUS test the same way): the exact mutation and the resulting failure are recorded in each task's steps. Do not skip re-verifying this after writing the test — a copy-paste slip can silently make an assertion vacuous.
- **Naming convention this plan establishes:** a DOM-mounted component test file is named `<component-in-kebab-case>.dom.test.ts`, colocated next to the component in `src/lib/components/`. `vitest.config.ts`'s `dom` project matches `src/**/*.dom.test.ts`; the `unit` project explicitly excludes that pattern so a file is never picked up by both projects.
- **Dated-count convention** (standing repo rule): this plan changes the frontend test count, so the task that lands the last new test updates `README.md` and `CONTRIBUTING.md`'s test-count lines in the same commit, with today's date, using a freshly measured `npm test` count — not a number copied from this plan or from the current (already-stale) doc text.
- **Do not** touch `engine::rebuild`, the journal reader, or the version graph (Plan F guard — irrelevant here, noted only because it's a standing constraint for this session). This plan is frontend-test-infra only; it does not touch `src-tauri` at all.

---

## Task 1: DOM test project + first mounted-component tests (render, select, delete)

**Files:**
- Modify: `package.json` (add two devDependencies)
- Modify: `vitest.config.ts` (split into `unit` + `dom` projects)
- Create: `src/lib/components/automation-track-row.dom.test.ts`

**Interfaces:**
- Consumes: `AutomationTrackRow.svelte`'s default export (`src/lib/components/AutomationTrackRow.svelte`, unmodified by this plan), `modulation` singleton (`src/lib/state/modulation.svelte.ts`: public `$state` fields `curves: Curve[]`, `automationClips: AutomationClip[]`), `midi` singleton (`src/lib/state/midi.svelte.ts`: public `$state` field `sectionTable: SectionRow[]`), the `backend` object shape from `src/lib/tauri.ts` (mocked wholesale via `vi.mock`, following the existing convention in `src/lib/state/automation-gesture.test.ts`).
- Produces: the `dom` vitest project (Task 2 adds two more tests to the same file, reusing its mocks/seed helpers verbatim).

- [ ] **Step 1: Add the DOM-testing devDependencies**

```bash
npm install --save-dev jsdom@^30.0.1 @testing-library/svelte@^5.4.2
```

Verify: `package.json`'s `devDependencies` now lists both, alongside the existing `svelte`/`vitest`/`@sveltejs/vite-plugin-svelte` entries. No other dependency changes.

- [ ] **Step 2: Split `vitest.config.ts` into `unit` + `dom` projects**

Replace the full contents of `vitest.config.ts` with:

```ts
import { defineConfig } from "vitest/config";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// Two projects because `.svelte` files compile differently per target:
// `unit` runs stores/utils in plain node against the SSR-compiled output
// (no DOM, no `mount()`); `dom` runs *.dom.test.ts under jsdom against the
// client-compiled output (needs `resolve.conditions: ["browser"]` or
// vite-plugin-svelte keeps emitting the SSR build and `mount()` throws
// "not available on the server").
export default defineConfig({
  test: {
    projects: [
      {
        plugins: [svelte()],
        test: {
          name: "unit",
          environment: "node",
          include: ["src/**/*.test.ts"],
          exclude: ["src/**/*.dom.test.ts"],
        },
      },
      {
        resolve: { conditions: ["browser"] },
        plugins: [svelte()],
        test: {
          name: "dom",
          environment: "jsdom",
          include: ["src/**/*.dom.test.ts"],
        },
      },
    ],
  },
});
```

- [ ] **Step 3: Run the existing suite to confirm the split is a no-op for it**

Run: `timeout 300 npx vitest run`
Expected: same test file count and same pass count as on `origin/main` before this change (measure `origin/main`'s count first with the same command if you have not already — do not assume a number). The new `dom` project reports 0 test files at this point, which is expected and not a failure.

- [ ] **Step 4: Write the failing DOM-mounted test file**

Create `src/lib/components/automation-track-row.dom.test.ts`:

```ts
// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import type { AutomationClip, Curve, TrackState } from "../types/ipc";

/**
 * `AutomationTrackRow`'s pointer/gesture handlers, mounted for real. The
 * Track D handoff (`docs/PHASE4-PLAN.md`) names this exact class of code —
 * ordering of async effects inside a `.svelte` event handler — as the
 * uncovered gap where both of that track's real frontend bugs lived,
 * because until now nothing could mount a component to exercise it.
 */

let curvesOnServer: Curve[] = [];
let clipsOnServer: AutomationClip[] = [];

function snapshot() {
  return {
    curves: curvesOnServer.map((c) => structuredClone(c)),
    bindings: [],
    automationClips: clipsOnServer.map((c) => structuredClone(c)),
  };
}

const modulationSetCurve = vi.fn((curve: Curve) => {
  const i = curvesOnServer.findIndex((c) => c.id === curve.id);
  if (i >= 0) curvesOnServer[i] = structuredClone(curve);
  else curvesOnServer.push(structuredClone(curve));
  return Promise.resolve(snapshot());
});

const automationClipSet = vi.fn((clip: AutomationClip, remove?: boolean) => {
  if (remove) clipsOnServer = clipsOnServer.filter((c) => c.id !== clip.id);
  else {
    const i = clipsOnServer.findIndex((c) => c.id === clip.id);
    if (i >= 0) clipsOnServer[i] = structuredClone(clip);
    else clipsOnServer.push(structuredClone(clip));
  }
  return Promise.resolve(snapshot());
});

const gestureBegin = vi.fn((label: string) => Promise.resolve(`gesture-${label}`));
const gestureEnd = vi.fn((_id: string) => Promise.resolve());

vi.mock("../tauri", () => ({
  backend: {
    mode: "tauri",
    on: () => () => {},
    gestureBegin,
    gestureEnd,
    modulationSetCurve,
    automationClipSet,
  },
}));

const { default: AutomationTrackRow } = await import("./AutomationTrackRow.svelte");
const { modulation } = await import("../state/modulation.svelte");
const { midi } = await import("../state/midi.svelte");

// 120 BPM @ 48 kHz, identity-ish (25 samples/tick). See this plan's Global
// Constraints for why an unseeded `sectionTable` silently hides every clip.
const SECTION_TABLE = [{ startTick: 0, startSample: 0, startBeat: 0, startBar: 0, period: 254_016_000 }];

function seedTrack(): TrackState {
  return {
    id: "t-1",
    name: "Auto",
    kind: "automation",
    gainDb: 0,
    pan: 0,
    muted: false,
    soloed: false,
    armed: false,
    color: "#38bdf8",
  };
}

function seedCurve(): Curve {
  return { id: "curve-1", name: "Cutoff", points: [{ tick: 0, value: 1 }] };
}

function seedClip(): AutomationClip {
  return {
    id: "clip-1",
    trackId: "t-1",
    curveId: "curve-1",
    timelineStartTicks: 0,
    lengthTicks: 1920,
    contentLengthTicks: 1920,
  };
}

function seed() {
  midi.sectionTable = SECTION_TABLE;
  modulation.curves = [seedCurve()];
  modulation.automationClips = [seedClip()];
}

afterEach(() => {
  cleanup();
  curvesOnServer = [];
  clipsOnServer = [];
  vi.clearAllMocks();
  modulation.curves = [];
  modulation.automationClips = [];
  midi.sectionTable = [];
});

describe("AutomationTrackRow", () => {
  it("a plain click selects the clip and shows the delete button", async () => {
    seed();
    render(AutomationTrackRow, { props: { track: seedTrack() } });

    const clipEl = screen.getByRole("button", { name: /automation clip/i });
    expect(screen.queryByRole("button", { name: /delete automation clip/i })).toBeNull();

    await fireEvent.pointerDown(clipEl, { clientX: 50, clientY: 5, button: 0 });
    await fireEvent.pointerUp(clipEl, { clientX: 50, clientY: 5, button: 0 });

    expect(screen.getByRole("button", { name: /delete automation clip/i })).toBeTruthy();
    expect(automationClipSet).not.toHaveBeenCalled();
    expect(gestureBegin).not.toHaveBeenCalled();
  });

  it("clicking the delete button removes the clip", async () => {
    seed();
    render(AutomationTrackRow, { props: { track: seedTrack() } });

    const clipEl = screen.getByRole("button", { name: /automation clip/i });
    await fireEvent.pointerDown(clipEl, { clientX: 50, clientY: 5, button: 0 });
    await fireEvent.pointerUp(clipEl, { clientX: 50, clientY: 5, button: 0 });

    const delBtn = screen.getByRole("button", { name: /delete automation clip/i });
    await fireEvent.click(delBtn);

    expect(automationClipSet).toHaveBeenCalledTimes(1);
    expect(automationClipSet).toHaveBeenCalledWith(expect.objectContaining({ id: "clip-1" }), true);
  });
});
```

- [ ] **Step 5: Run it**

Run: `timeout 60 npx vitest run src/lib/components/automation-track-row.dom.test.ts`
Expected: `dom` project, 1 test file, 2 tests, both PASS. If you see `Svelte error: lifecycle_function_unavailable`, re-check Step 2's `resolve.conditions` on the `dom` project. If the clip element cannot be found by role, re-check `seed()` is called (an unseeded `sectionTable` renders nothing — see Global Constraints).

- [ ] **Step 6: Run the full suite**

Run: `timeout 300 npx vitest run`
Expected: `unit` project unchanged from Step 3's count; `dom` project now 1 file / 2 tests, both passing; 0 failures overall.

- [ ] **Step 7: Commit**

```bash
git add package.json package-lock.json vitest.config.ts src/lib/components/automation-track-row.dom.test.ts
git commit -m "test(frontend): add a jsdom DOM test project, mount AutomationTrackRow"
```

---

## Task 2: Gesture-sequencing tests (the actual regression class) + doc count update

**Files:**
- Modify: `src/lib/components/automation-track-row.dom.test.ts` (append two tests)
- Modify: `README.md`, `CONTRIBUTING.md` (dated frontend test-count lines)

**Interfaces:**
- Consumes: everything Task 1's file already sets up (`seed()`, the mocks, `SECTION_TABLE`) — do not duplicate it, append inside the same `describe("AutomationTrackRow", ...)` block from Task 1.

- [ ] **Step 1: Write the failing alt-click delete-point test**

Append inside the same `describe("AutomationTrackRow", ...)` block, after the two tests from Task 1:

```ts
  it("alt-click on an existing point deletes it and closes the SAME gesture it opened", async () => {
    seed();
    render(AutomationTrackRow, { props: { track: seedTrack() } });

    const clipEl = screen.getByRole("button", { name: /automation clip/i });
    // clientX/Y = 0 lands exactly on the seeded point (tick 0, value 1) —
    // canvasPos falls back to a 1:1 scale under jsdom's zero-rect default
    // (see this plan's Global Constraints), so {x, y} = {clientX, clientY}.
    await fireEvent.pointerDown(clipEl, { clientX: 0, clientY: 0, button: 0, altKey: true });
    // commitInGesture's .then(...) is a microtask chain off an unawaited
    // promise in the handler; give it two ticks to run.
    await Promise.resolve();
    await Promise.resolve();

    expect(gestureBegin).toHaveBeenCalledTimes(1);
    expect(gestureBegin).toHaveBeenCalledWith("automation delete point");
    expect(modulationSetCurve).toHaveBeenCalledTimes(1);
    expect(modulationSetCurve.mock.calls[0][0].points).toEqual([]);
    expect(gestureEnd).toHaveBeenCalledTimes(1);
    expect(gestureEnd).toHaveBeenCalledWith("gesture-automation delete point");
  });
```

- [ ] **Step 2: Run it, confirm it passes**

Run: `timeout 60 npx vitest run src/lib/components/automation-track-row.dom.test.ts`
Expected: 3 tests, all PASS.

- [ ] **Step 3: Sabotage-verify Step 1's test is not vacuous**

In `src/lib/components/AutomationTrackRow.svelte`, temporarily change (inside `onPointerDown`'s `mode === "delete"` branch):

```ts
            .then(async () => project.endGesture(await gid));
```
to:
```ts
            .then(async () => project.endGesture());
```

Run: `timeout 60 npx vitest run src/lib/components/automation-track-row.dom.test.ts -t "alt-click"`
Expected: FAILS — `gestureEnd` was called with no argument, so `toHaveBeenCalledWith("gesture-automation delete point")` fails. This proves the test actually pins the gesture-token id, not just that *some* end-call happened.

Revert the temporary change (`git checkout -- src/lib/components/AutomationTrackRow.svelte` or undo by hand) and re-run to confirm it passes again before continuing.

- [ ] **Step 4: Write the failing point-drag test**

Append the same `describe` block:

```ts
  it("dragging a point commits once on pointerup, not on every pointermove", async () => {
    seed();
    render(AutomationTrackRow, { props: { track: seedTrack() } });

    const clipEl = screen.getByRole("button", { name: /automation clip/i });
    const canvasEl = document.querySelector("canvas") as HTMLCanvasElement;
    // jsdom implements no Pointer Capture API at all; the point-edit path
    // calls `canvas.setPointerCapture` unconditionally (see Global
    // Constraints).
    Object.defineProperty(canvasEl, "setPointerCapture", { value: vi.fn(), writable: true });
    // jsdom's default zero rect makes the near-right-edge test
    // (`rect.right - clientX <= EDGE_PX`) true for any non-negative
    // clientX, which would misroute this click to "resize" instead of
    // "point" — stub the div's rect to its actual rendered size.
    clipEl.getBoundingClientRect = () =>
      ({ left: 0, top: 0, right: 40, bottom: 20, width: 40, height: 20 }) as DOMRect;

    await fireEvent.pointerDown(clipEl, { clientX: 0, clientY: 0, button: 0 });
    expect(gestureBegin).toHaveBeenCalledTimes(1);
    expect(gestureBegin).toHaveBeenCalledWith("automation edit");

    await fireEvent.pointerMove(clipEl, { clientX: 0, clientY: 1 });
    await fireEvent.pointerMove(clipEl, { clientX: 0, clientY: 1 });
    expect(modulationSetCurve).not.toHaveBeenCalled();

    await fireEvent.pointerUp(clipEl, { clientX: 0, clientY: 1, button: 0 });
    await Promise.resolve();
    await Promise.resolve();

    expect(modulationSetCurve).toHaveBeenCalledTimes(1);
    expect(gestureEnd).toHaveBeenCalledTimes(1);
  });
```

- [ ] **Step 5: Run it, confirm it passes**

Run: `timeout 60 npx vitest run src/lib/components/automation-track-row.dom.test.ts`
Expected: 4 tests, all PASS.

- [ ] **Step 6: Sabotage-verify Step 4's test is not vacuous**

In `src/lib/components/AutomationTrackRow.svelte`, temporarily change `onPointerMove`'s `dragMode === "point"` branch from:

```ts
      const { tick, value } = localTickValue(clip, e, canvas);
      const moved = movePoint(curve.points, dragIndex, tick, value, 0, 1);
      dragIndex = moved.index;
      modulation.preview(curve.id, moved.points);
      return;
```
to:
```ts
      const { tick, value } = localTickValue(clip, e, canvas);
      const moved = movePoint(curve.points, dragIndex, tick, value, 0, 1);
      dragIndex = moved.index;
      modulation.preview(curve.id, moved.points);
      void modulation.commit({ ...curve, points: moved.points });
      return;
```

Run: `timeout 60 npx vitest run src/lib/components/automation-track-row.dom.test.ts -t "dragging a point"`
Expected: FAILS — `expect(modulationSetCurve).not.toHaveBeenCalled()` (right after the two `pointermove`s, before `pointerup`) now sees a call.

Revert the temporary change and re-run to confirm it passes again before continuing.

- [ ] **Step 7: Run the full suite**

Run: `timeout 300 npx vitest run`
Expected: 0 failures. Note the exact new total (`unit` + `dom` file/test counts) for Step 8 — do not estimate it.

- [ ] **Step 8: Update the dated test-count lines**

In `CONTRIBUTING.md`, find the line matching (currently, on `main`, though re-check it hasn't drifted further):
```
npm test                     # 778 frontend unit tests (vitest; counted 2026-08-18 after the Composer, Plan H1)
```
Replace the count and the parenthetical with the number measured in Step 7 and today's date, e.g.:
```
npm test                     # <N> frontend unit tests (vitest; counted 2026-08-18 after adding the DOM test environment)
```

In `README.md`, find the matching `npm test` line (currently starts `# 778 frontend unit tests (counted 2026-08-18 after the Composer, Plan H1; vitest): ...`) and apply the same count/date update, appending `+ AutomationTrackRow pointer/gesture handlers` to the trailing list of what's covered (that list enumerates coverage areas — follow its existing comma-separated style).

- [ ] **Step 9: Commit**

```bash
git add src/lib/components/automation-track-row.dom.test.ts README.md CONTRIBUTING.md
git commit -m "test(frontend): DOM-mounted gesture-sequencing tests for AutomationTrackRow"
```
