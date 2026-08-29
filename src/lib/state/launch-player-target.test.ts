/**
 * Fix round 1, Critical 2: a migrated `LaunchTarget` is `{kind:"player"}`,
 * not `{kind:"clip"}`. `mapClip` and `clipSelfTriggers` used to match only
 * `"clip"`, so after migration a clip that already had a pad silently got
 * a SECOND binding on a fresh note, and the self-trigger warning silently
 * went quiet. Both are resolved through `launch.players` (from
 * `players_get`), the frontend's only source for a player's source clip.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { LaunchSnapshot, PlayerInfo } from "../types/ipc";

const calls: { name: string; args: unknown[] }[] = [];

vi.mock("../tauri", () => ({
  backend: {
    mode: "tauri",
    on: () => () => {},
    launchGet: () => Promise.resolve({ maps: [] } as LaunchSnapshot),
    launchSet: (...args: unknown[]) => {
      calls.push({ name: "launchSet", args });
      return Promise.resolve({ maps: [] } as LaunchSnapshot);
    },
    playersGet: () => Promise.resolve([] as PlayerInfo[]),
    transportSeek: () => Promise.resolve(undefined),
  },
}));

const { launch } = await import("./launch.svelte");
const { midi } = await import("./midi.svelte");
const { surface } = await import("./surface.svelte");

const playerBinding = (id: string, note: number, playerId: string) => ({
  id,
  name: id,
  note,
  channel: null,
  target: { kind: "player" as const, playerId },
});

beforeEach(() => {
  calls.length = 0;
  launch.maps = [{ ...launch.maps[0], bindings: [] }] as unknown as typeof launch.maps;
  launch.activeMapId = launch.maps[0].id;
  launch.players = [];
  midi.clips = [
    {
      id: "mc1",
      trackId: "t1",
      name: "Verse",
      timelineStartTicks: 0,
      lengthTicks: 960,
      notes: [{ tick: 0, lengthTicks: 240, key: 36, velocity: 100 }],
    },
  ] as unknown as typeof midi.clips;
});

describe("launch.mapClip against a migrated player binding", () => {
  it("finds the existing pad instead of minting a second one", async () => {
    launch.players = [
      { id: "p1", name: "Verse", source: { kind: "midiClip", clipId: "mc1" } },
    ];
    launch.maps = [{ ...launch.maps[0], bindings: [playerBinding("b1", 36, "p1")] }] as unknown as typeof launch.maps;

    const result = await launch.mapClip("mc1");

    expect(result?.id).toBe("b1");
    expect(calls.find((c) => c.name === "launchSet")).toBeUndefined();
  });

  it("still creates a fresh binding for a clip with no player yet", async () => {
    launch.players = [{ id: "p1", name: "Other", source: { kind: "midiClip", clipId: "mc-other" } }];

    const result = await launch.mapClip("mc1");

    expect(result?.target).toEqual({ kind: "clip", clipId: "mc1" });
    expect(calls.find((c) => c.name === "launchSet")).toBeDefined();
  });
});

describe("launch.clipSelfTriggers against a migrated player binding", () => {
  it("still warns when the player's own clip contains the trigger note", () => {
    launch.players = [{ id: "p1", name: "Verse", source: { kind: "midiClip", clipId: "mc1" } }];
    const b = playerBinding("b1", 36, "p1");

    expect(launch.clipSelfTriggers(b)).toBe(true);
  });

  it("does not warn for a different note", () => {
    launch.players = [{ id: "p1", name: "Verse", source: { kind: "midiClip", clipId: "mc1" } }];
    const b = playerBinding("b1", 40, "p1");

    expect(launch.clipSelfTriggers(b)).toBe(false);
  });

  it("does not warn when the player can't be resolved (players not loaded yet)", () => {
    launch.players = [];
    const b = playerBinding("b1", 36, "p1");

    expect(launch.clipSelfTriggers(b)).toBe(false);
  });
});

describe("surface.isClipPlaying for a migrated player binding", () => {
  it("stays lit for the clip the overlay's player actually plays", () => {
    launch.players = [{ id: "p1", name: "Verse", source: { kind: "midiClip", clipId: "mc1" } }];
    launch.maps = [{ ...launch.maps[0], bindings: [playerBinding("b1", 36, "p1")] }] as unknown as typeof launch.maps;
    launch.overlay = { id: "b1", name: "Verse" };

    expect(surface.isClipPlaying("mc1")).toBe(true);
    expect(surface.isClipPlaying("mc-other")).toBe(false);
  });

  it("is false once the overlay clears", () => {
    launch.players = [{ id: "p1", name: "Verse", source: { kind: "midiClip", clipId: "mc1" } }];
    launch.maps = [{ ...launch.maps[0], bindings: [playerBinding("b1", 36, "p1")] }] as unknown as typeof launch.maps;
    launch.overlay = null;

    expect(surface.isClipPlaying("mc1")).toBe(false);
  });
});
