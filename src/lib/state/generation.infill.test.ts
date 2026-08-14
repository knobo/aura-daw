/**
 * AMT infill note-id rules (Task 15 REVISED, per note-ops' copyNote
 * convention landed on main via PR #11 — noteId is OPTIONAL, absent = the
 * backend mints): the merge in `generation.svelte.ts`'s `land()` must keep
 * the clip's own context notes' ids untouched (ADR 0001's round-trip rule)
 * while every sidecar-filled note carries NO id — even defensively, in case
 * the sidecar result echoes one back — so an absent id lets the backend
 * mint fresh ones instead of the keep-rule re-minting a collision.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { MidiClip, OpenSidecarEvent } from "../types/ipc";

type EventHandler = (e: OpenSidecarEvent) => void;
let handler: EventHandler | null = null;

const midiSetNotes = vi.fn();

vi.mock("../tauri", () => ({
  backend: {
    sidecarRunJob: vi.fn(
      (_kind: string, _params: Record<string, unknown>, onEvent: EventHandler) => {
        handler = onEvent;
        return Promise.resolve("j-infill");
      },
    ),
    midiSetNotes: (clipId: string, notes: unknown) => midiSetNotes(clipId, notes),
  },
}));

const { generation } = await import("./generation.svelte");
const { midi } = await import("./midi.svelte");

const baseClip: MidiClip = {
  id: "clip-1",
  trackId: "t1",
  name: "infill target",
  timelineStartTicks: 0,
  lengthTicks: 4000,
  notes: [
    // Context: entirely before the infill region [1000, 3000) — must round-trip its id.
    { noteId: 1, tick: 0, lengthTicks: 100, key: 60, velocity: 100 },
    // Context: entirely after the infill region — must round-trip its id.
    { noteId: 2, tick: 3500, lengthTicks: 100, key: 62, velocity: 100 },
  ],
};

beforeEach(() => {
  generation.jobs = {};
  handler = null;
  midiSetNotes.mockReset();
  midiSetNotes.mockImplementation((clipId: string, notes: unknown) =>
    Promise.resolve({ ...baseClip, id: clipId, notes }),
  );
  midi.clips = [{ ...baseClip, notes: baseClip.notes.map((n) => ({ ...n })) }];
});

describe("amtInfill merge id rules", () => {
  it("keeps context note ids and strips ids off sidecar-filled notes", async () => {
    const jobId = await generation.run("amtInfill", {}, "infill", {
      clipId: "clip-1",
      startTicks: 1000,
      endTicks: 3000,
    });
    expect(jobId).toBe("j-infill");
    expect(handler).toBeTruthy();

    handler!({
      type: "done",
      jobId: "j-infill",
      result: {
        kind: "amtInfill",
        notes: [
          // A stray id the sidecar might echo back — must be stripped, not kept.
          { noteId: 999, tick: 1200, lengthTicks: 200, key: 64, velocity: 90 },
          // No id at all — the common case — must stay absent.
          { tick: 1800, lengthTicks: 200, key: 65, velocity: 90 },
        ],
      },
    });

    // land() awaits midi.setNotes, whose store mutation happens synchronously
    // before ITS OWN await — but flush a couple of microtask turns to be safe
    // rather than lean on that ordering detail.
    await Promise.resolve();
    await Promise.resolve();

    const clip = midi.clipById("clip-1");
    expect(clip).toBeDefined();
    // Sorted by (tick, key): context(0,id1), filled(1200), filled(1800), context(3500,id2).
    expect(clip!.notes.map((n) => n.tick)).toEqual([0, 1200, 1800, 3500]);
    expect(clip!.notes.map((n) => n.noteId)).toEqual([1, undefined, undefined, 2]);
    // Pin "absent", not merely undefined-valued — copyNote deletes the key.
    expect("noteId" in clip!.notes[1]).toBe(false);
    expect("noteId" in clip!.notes[2]).toBe(false);

    expect(midiSetNotes).toHaveBeenCalledTimes(1);
  });

  it("strips a filled note's id even when the region has no surviving context notes", async () => {
    midi.clips = [{ ...baseClip, notes: [] }];

    await generation.run("amtInfill", {}, "infill", {
      clipId: "clip-1",
      startTicks: 0,
      endTicks: 1000,
    });
    handler!({
      type: "done",
      jobId: "j-infill",
      result: { kind: "amtInfill", notes: [{ noteId: 42, tick: 10, lengthTicks: 50, key: 60, velocity: 80 }] },
    });
    await Promise.resolve();
    await Promise.resolve();

    const clip = midi.clipById("clip-1")!;
    expect(clip.notes).toHaveLength(1);
    expect(clip.notes[0].noteId).toBeUndefined();
  });
});
