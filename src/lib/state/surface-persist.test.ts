/**
 * Which project a deck is written under.
 *
 * The layout is chrome, so it lives in localStorage keyed on the project dir
 * (ADR 0006). The write is debounced, which puts a window between the last
 * edit and the write in which the open project can change — and a timer that
 * fires after the swap must NOT write the deck it was holding into the new
 * project's key, nor drop the edit it was holding for the old one.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { storageKey } from "../utils/control-surface";

vi.mock("../tauri", () => ({
  backend: new Proxy(
    { mode: "tauri" },
    {
      get: (target: Record<string, unknown>, prop: string) =>
        prop in target ? target[prop] : () => Promise.resolve(undefined),
    },
  ),
}));

const { surface } = await import("./surface.svelte");
const { project } = await import("./project.svelte");

const disk = new Map<string, string>();

beforeEach(() => {
  disk.clear();
  vi.useFakeTimers();
  vi.stubGlobal("localStorage", {
    getItem: (k: string) => disk.get(k) ?? null,
    setItem: (k: string, v: string) => void disk.set(k, v),
    removeItem: (k: string) => void disk.delete(k),
  });
});

function widgetCount(key: string): number {
  const raw = disk.get(key);
  if (!raw) return -1;
  return JSON.parse(raw).pages[0].widgets.length;
}

/** Open a project the way App does: dir first, then the panel hydrates. */
function open(dir: string) {
  project.projectDir = dir;
  surface.hydrate(dir);
}

describe("the deck's localStorage key", () => {
  it("keeps an edit made just before a project switch", () => {
    open("/proj/a");
    surface.choose("gauge"); // an edit, debounced
    open("/proj/b"); // switch inside the debounce window
    vi.runAllTimers();

    expect(widgetCount(storageKey("/proj/a"))).toBe(1);
    // B never inherits A's deck — hydrating it found nothing and left it alone
    expect(widgetCount(storageKey("/proj/b"))).toBe(-1);
  });

  it("brings each project back to its own deck", () => {
    open("/proj/a");
    surface.choose("gauge");
    vi.runAllTimers();
    open("/proj/b");
    surface.choose("pad");
    surface.choose("pad");
    vi.runAllTimers();

    expect(widgetCount(storageKey("/proj/a"))).toBe(1);
    expect(widgetCount(storageKey("/proj/b"))).toBe(2);

    open("/proj/a");
    expect(surface.page.widgets).toHaveLength(1);
    expect(surface.page.widgets[0].kind).toBe("gauge");
  });
});
