/**
 * Keyboard coverage for lane selection (4.5) — mounted for real, three
 * `TrackHeader` rows inside a shared `role="grid"` container the way
 * `Timeline.svelte`'s rail actually nests them, because the row-to-row
 * ArrowUp/Down handoff and the `[data-track-row]` lookup it relies on only
 * exist once rows share a real DOM parent. A keyboard user has NO other way
 * to reach bulk M/S/A — this is the path the coordinator flagged as
 * unreachable, so it gets DOM coverage rather than staying "obviously
 * correct" from a read of the code.
 *
 * See Global Constraints in
 * docs/superpowers/plans/2026-08-18-dom-test-environment.md: `Meter`'s
 * canvas paint loop needs no stubbing here (jsdom's `getContext("2d")`
 * returns null and it early-returns, same as `AutomationTrackRow`'s), and
 * nothing here touches `setPointerCapture` or `getBoundingClientRect`.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render } from "@testing-library/svelte";
import type { TrackState } from "../types/ipc";

vi.stubGlobal("localStorage", {
  getItem: () => null,
  setItem: () => {},
});

const setTrackMute = vi.fn(() => Promise.resolve());
const setTrackSolo = vi.fn(() => Promise.resolve());
const setTrackArm = vi.fn(() => Promise.resolve());
const gestureBegin = vi.fn((label: string) => Promise.resolve(`gid-${label}`));
const gestureEnd = vi.fn(() => Promise.resolve());

vi.mock("../tauri", () => ({
  backend: {
    mode: "demo",
    on: () => () => {},
    setTrackMute,
    setTrackSolo,
    setTrackArm,
    gestureBegin,
    gestureEnd,
  },
}));

const { default: TrackHeader } = await import("./TrackHeader.svelte");
const { default: LaneGroupHeader } = await import("./LaneGroupHeader.svelte");
const { lanes } = await import("../state/lanes.svelte");
const { project } = await import("../state/project.svelte");

function track(id: string): TrackState {
  return {
    id,
    name: `Track ${id}`,
    kind: "audio",
    gainDb: 0,
    pan: 0,
    muted: false,
    soloed: false,
    armed: false,
    automationMode: "read",
    color: "#38bdf8",
  };
}

const ORDER = ["a", "b", "c"];

/** Mounts three rows into ONE `role="grid"` container — the shape
 * `onHeaderKeydown`'s `closest('[role="grid"]')` and `[data-track-row]`
 * lookup assume. `render`'s `target` option (from Svelte 5's own `mount`)
 * mounts INTO an existing element rather than making a fresh container per
 * call, which is what lets three independently-rendered rows end up as
 * siblings. */
function mountGrid() {
  const grid = document.createElement("div");
  grid.setAttribute("role", "grid");
  document.body.appendChild(grid);
  for (const id of ORDER) {
    render(TrackHeader, {
      target: grid,
      props: { track: track(id), index: ORDER.indexOf(id), collapsed: false, orderedTrackIds: ORDER },
    });
  }
  return grid;
}

function row(grid: HTMLElement, id: string): HTMLElement {
  return grid.querySelector<HTMLElement>(`[data-track-row="${id}"]`)!;
}

afterEach(() => {
  cleanup();
  document.body.replaceChildren();
  vi.clearAllMocks();
  lanes.clearSelection();
  project.tracks = [];
});

describe("TrackHeader row — keyboard selection", () => {
  it("renders each row focusable, aria-selected, with roving tabindex favouring the first row", () => {
    const grid = mountGrid();
    project.tracks = ORDER.map(track);

    expect(row(grid, "a").getAttribute("role")).toBe("row");
    expect(row(grid, "a").getAttribute("aria-selected")).toBe("false");
    // Nothing selected yet: row 0 is the grid's one entry point.
    expect(row(grid, "a").tabIndex).toBe(0);
    expect(row(grid, "b").tabIndex).toBe(-1);
    expect(row(grid, "c").tabIndex).toBe(-1);
  });

  it("Space on a focused row selects it (plain) and updates aria-selected + tabindex", async () => {
    const grid = mountGrid();
    project.tracks = ORDER.map(track);

    row(grid, "b").focus();
    await fireEvent.keyDown(row(grid, "b"), { key: " " });

    expect([...lanes.selection]).toEqual(["b"]);
    expect(row(grid, "b").getAttribute("aria-selected")).toBe("true");
    expect(row(grid, "b").tabIndex).toBe(0);
    expect(row(grid, "a").tabIndex).toBe(-1);
  });

  it("Enter does the same as Space", async () => {
    const grid = mountGrid();
    project.tracks = ORDER.map(track);

    row(grid, "c").focus();
    await fireEvent.keyDown(row(grid, "c"), { key: "Enter" });

    expect([...lanes.selection]).toEqual(["c"]);
  });

  it("Ctrl+Space toggles the focused row into (and back out of) the selection", async () => {
    const grid = mountGrid();
    project.tracks = ORDER.map(track);

    await fireEvent.keyDown(row(grid, "a"), { key: " ", ctrlKey: true });
    expect([...lanes.selection]).toEqual(["a"]);

    await fireEvent.keyDown(row(grid, "a"), { key: " ", ctrlKey: true });
    expect([...lanes.selection]).toEqual([]);
  });

  it("Shift+Space extends the run from the anchor to the focused row", async () => {
    const grid = mountGrid();
    project.tracks = ORDER.map(track);

    await fireEvent.keyDown(row(grid, "a"), { key: " " }); // anchor = a
    await fireEvent.keyDown(row(grid, "c"), { key: " ", shiftKey: true });

    expect([...lanes.selection].sort()).toEqual(["a", "b", "c"]);
  });

  it("Ctrl+A selects every row regardless of which one is focused", async () => {
    const grid = mountGrid();
    project.tracks = ORDER.map(track);

    await fireEvent.keyDown(row(grid, "b"), { key: "a", ctrlKey: true });

    expect([...lanes.selection].sort()).toEqual(["a", "b", "c"]);
  });

  it("ArrowDown/ArrowUp move DOM focus between rows by the given visible order", async () => {
    const grid = mountGrid();
    project.tracks = ORDER.map(track);

    row(grid, "a").focus();
    await fireEvent.keyDown(row(grid, "a"), { key: "ArrowDown" });
    expect(document.activeElement).toBe(row(grid, "b"));

    await fireEvent.keyDown(row(grid, "b"), { key: "ArrowDown" });
    expect(document.activeElement).toBe(row(grid, "c"));

    // Past the last row: no next id, so focus does not move (and does not throw).
    await fireEvent.keyDown(row(grid, "c"), { key: "ArrowDown" });
    expect(document.activeElement).toBe(row(grid, "c"));

    await fireEvent.keyDown(row(grid, "c"), { key: "ArrowUp" });
    expect(document.activeElement).toBe(row(grid, "b"));
  });

  it("a keypress on a child control (the mute button) does not also select the row", async () => {
    const grid = mountGrid();
    project.tracks = ORDER.map(track);

    const muteBtn = row(grid, "a").querySelector<HTMLElement>(".status.mute")!;
    muteBtn.focus();
    await fireEvent.keyDown(muteBtn, { key: " " });

    // The row-level handler must see e.target !== e.currentTarget and bail —
    // pressing Space on the mute button must never fall through to
    // "select this row", or a mute keypress would silently narrow a
    // multi-lane selection down to one lane.
    expect(lanes.selection.size).toBe(0);
  });

  it("clicking the mute button toggles mute, not selection — even though the row has an onclick too", async () => {
    const grid = mountGrid();
    project.tracks = ORDER.map(track);

    const muteBtn = row(grid, "a").querySelector<HTMLElement>(".status.mute")!;
    await fireEvent.click(muteBtn);

    expect(setTrackMute).toHaveBeenCalledWith("a", true);
    expect(lanes.selection.size).toBe(0);
  });
});

/**
 * Structural invariant: per WAI-ARIA, `role="row"` must own a `cell` /
 * `gridcell` / `columnheader` / `rowheader` — a `row` with no cell leaves
 * a screen reader that navigates the grid cell-by-cell with nowhere to
 * land. This is exactly what the coordinator's second review caught (a
 * `role="row"` added without ever giving it a cell); this test exists so
 * the next row type someone adds can't silently repeat it.
 */
function rowOwnsGridcell(rowEl: HTMLElement) {
  expect(rowEl.getAttribute("role")).toBe("row");
  expect(rowEl.querySelectorAll('[role="gridcell"]').length).toBeGreaterThan(0);
}

function automationTrack(id: string): TrackState {
  return { ...track(id), kind: "automation" };
}

describe("grid structure — every row owns at least one gridcell", () => {
  it("a collapsed TrackHeader row", () => {
    const grid = document.createElement("div");
    grid.setAttribute("role", "grid");
    document.body.appendChild(grid);
    render(TrackHeader, {
      target: grid,
      props: { track: track("a"), index: 0, collapsed: true, orderedTrackIds: ["a"] },
    });
    rowOwnsGridcell(row(grid, "a"));
  });

  it("an expanded audio TrackHeader row", () => {
    const grid = document.createElement("div");
    grid.setAttribute("role", "grid");
    document.body.appendChild(grid);
    render(TrackHeader, {
      target: grid,
      props: { track: track("a"), index: 0, collapsed: false, orderedTrackIds: ["a"] },
    });
    // Distinct regions (identity, routing/FX, mix status, automation mode,
    // level) — several cells, not one blob, per the coordinator's own
    // preference for finer-grained cells over one.
    const cells = row(grid, "a").querySelectorAll('[role="gridcell"]');
    expect(cells.length).toBeGreaterThan(1);
  });

  it("an expanded automation TrackHeader row (the targets branch, not status/level)", () => {
    const grid = document.createElement("div");
    grid.setAttribute("role", "grid");
    document.body.appendChild(grid);
    render(TrackHeader, {
      target: grid,
      props: { track: automationTrack("a"), index: 0, collapsed: false, orderedTrackIds: ["a"] },
    });
    rowOwnsGridcell(row(grid, "a"));
  });

  it("a LaneGroupHeader row", () => {
    const grid = document.createElement("div");
    grid.setAttribute("role", "grid");
    document.body.appendChild(grid);
    render(LaneGroupHeader, {
      target: grid,
      props: {
        row: {
          kind: "group",
          group: "Drums",
          top: 0,
          height: 22,
          trackIds: ["a", "b"],
          collapsed: false,
          color: "#38bdf8",
        },
      },
    });
    const groupRow = grid.querySelector<HTMLElement>('[role="row"]')!;
    rowOwnsGridcell(groupRow);
  });
});
