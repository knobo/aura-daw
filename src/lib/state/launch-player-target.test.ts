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
    transportSeek: (samples: number) => {
      calls.push({ name: "transportSeek", args: [samples] });
      return Promise.resolve(undefined);
    },
  },
}));

const { launch } = await import("./launch.svelte");
const { midi } = await import("./midi.svelte");
const { surface } = await import("./surface.svelte");
const { view } = await import("./view.svelte");

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

describe("surface.fireClip against an already-bound clip (fix round 2, Item 1)", () => {
  it("does not seek the transport or scroll the view for a clip binding", async () => {
    launch.maps = [
      { ...launch.maps[0], bindings: [{ id: "b1", name: "Verse", note: 36, channel: null, target: { kind: "clip", clipId: "mc1" } }] },
    ] as unknown as typeof launch.maps;
    const viewStartBefore = view.viewStart;

    await surface.fireClip("mc1");

    expect(calls.find((c) => c.name === "transportSeek")).toBeUndefined();
    expect(view.viewStart).toBe(viewStartBefore);
    // `preview()` DOES select the pad it fires — that's its own,
    // deliberate, unrelated-to-this-round behaviour (the panel highlights
    // whatever is currently playing); the regression this round fixed was
    // only the SEEK/SCROLL side effect `mapClip`'s internal `focus()` call
    // added when `fireClip` started delegating to it unconditionally.
    expect(launch.selectedId).toBe("b1");
  });

  it("does not seek the transport for a migrated player binding either", async () => {
    launch.players = [{ id: "p1", name: "Verse", source: { kind: "midiClip", clipId: "mc1" } }];
    launch.maps = [{ ...launch.maps[0], bindings: [playerBinding("b1", 36, "p1")] }] as unknown as typeof launch.maps;
    const viewStartBefore = view.viewStart;

    await surface.fireClip("mc1");

    expect(calls.find((c) => c.name === "transportSeek")).toBeUndefined();
    expect(view.viewStart).toBe(viewStartBefore);
    expect(launch.selectedId).toBe("b1");
  });
});

describe("launch.reload's players/maps ordering (fix round 2, Item 2)", () => {
  it("has players loaded by the time reload() resolves, even when playersGet is slower than launchGet", async () => {
    // A REAL delay (setTimeout), not just an unresolved Promise resolved
    // manually before the `await` below — that version of this test
    // passed even against `void this.reloadPlayers()` (fire-and-forget,
    // never awaited): the assignment inside `reloadPlayers()` still got a
    // microtask turn to run before `await reloadDone` returned, either
    // way. A timer forces `playersGet` onto a LATER tick than
    // `launchGet`'s already-resolved Promise, so only the path that
    // actually AWAITS it can still be pending when `reload()` resolves.
    const { backend } = await import("../tauri");
    const originalPlayersGet = backend.playersGet;
    const resolved: PlayerInfo[] = [{ id: "p1", name: "Verse", source: { kind: "midiClip", clipId: "mc1" } }];
    backend.playersGet = () =>
      new Promise<PlayerInfo[]>((res) => {
        setTimeout(() => res(resolved), 5);
      });

    await launch.reload();

    expect(launch.players).toEqual(resolved);
    backend.playersGet = originalPlayersGet;
  });
});
