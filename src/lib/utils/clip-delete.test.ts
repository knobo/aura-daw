/**
 * Global Delete/Backspace on a selected timeline clip. The clip views
 * bind those keys to their own element, but a pointer-select does not
 * focus it — so the window-level handler in App.svelte is the path that
 * actually fires after a click. This helper is what that handler asks.
 */
import { describe, expect, it } from "vitest";
import { clipDeleteTarget } from "./clip-delete";

describe("clipDeleteTarget", () => {
  it("picks the sole selected audio clip", () => {
    expect(clipDeleteTarget([{ kind: "audio", id: "c-1" }])).toEqual({ kind: "audio", id: "c-1" });
  });

  it("picks the sole selected MIDI clip", () => {
    expect(clipDeleteTarget([{ kind: "midi", id: "m-1" }])).toEqual({ kind: "midi", id: "m-1" });
  });

  it("returns null when nothing is selected", () => {
    expect(clipDeleteTarget([])).toBeNull();
  });

  it("returns null when more than one clip is selected (batch delete is not wired yet)", () => {
    expect(
      clipDeleteTarget([
        { kind: "audio", id: "c-1" },
        { kind: "midi", id: "m-1" },
      ]),
    ).toBeNull();
  });
});
