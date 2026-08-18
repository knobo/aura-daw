/**
 * The patchbay's view model. Everything the panel draws comes out of
 * `buildRoutingView`, so the interesting cases — a route pointing at a port
 * nobody opened, a device that got unplugged, a clip route whose clip was
 * deleted — are all reachable without rendering anything.
 */
import { describe, expect, it } from "vitest";
import {
  PORT_ACCENTS,
  buildRoutingView,
  portAccents,
  type RoutingInput,
} from "./routing-view";
import type { MidiOutputPortStatus, MidiPortInfo, MidiRouteStatus } from "../../types/ipc";

function port(id: string, name = `${id} name`): MidiPortInfo {
  return { id, name };
}

function output(id: string, name = `${id} name`): MidiOutputPortStatus {
  return {
    port: port(id, name),
    clockEnabled: false,
    running: false,
    pulsesSent: 0,
    resyncs: 0,
    notesSent: 0,
  };
}

function trackRoute(
  id: string,
  portId: string,
  channel = 0,
  returnDevice?: string | null,
): MidiRouteStatus {
  return { scope: "track", id, portId, channel, returnDevice };
}

function clipRoute(id: string, portId: string, channel = 0): MidiRouteStatus {
  return { scope: "clip", id, portId, channel };
}

function input(over: Partial<RoutingInput> = {}): RoutingInput {
  return {
    tracks: [],
    clips: [],
    outputs: [],
    outPorts: [],
    routes: [],
    targetTrackId: null,
    ...over,
  };
}

const midiTrack = (id: string, name = id, color = "#00e5ff") => ({
  id,
  name,
  kind: "midi",
  color,
});

describe("port health", () => {
  it("calls a route open when its port is in outputs", () => {
    const view = buildRoutingView(
      input({
        tracks: [midiTrack("t1")],
        outputs: [output("p1", "Deluge")],
        outPorts: [port("p1", "Deluge")],
        routes: [trackRoute("t1", "p1", 3)],
      }),
    );
    expect(view.rows[0].target).toEqual({
      portId: "p1",
      portName: "Deluge",
      channel: 3,
      health: "open",
    });
  });

  it("calls a route closed when the device exists but the port is not open", () => {
    const view = buildRoutingView(
      input({
        tracks: [midiTrack("t1")],
        outputs: [],
        outPorts: [port("p1", "Deluge")],
        routes: [trackRoute("t1", "p1")],
      }),
    );
    expect(view.rows[0].target?.health).toBe("closed");
    expect(view.rows[0].target?.portName).toBe("Deluge");
  });

  it("calls a route missing when the port is in neither list, and shows the raw id", () => {
    const view = buildRoutingView(
      input({
        tracks: [midiTrack("t1")],
        outputs: [output("p1")],
        outPorts: [port("p1")],
        routes: [trackRoute("t1", "gone-port-7")],
      }),
    );
    expect(view.rows[0].target).toEqual({
      portId: "gone-port-7",
      portName: "gone-port-7",
      channel: 0,
      health: "missing",
    });
  });

  it("leaves target null when a MIDI track has no route", () => {
    const view = buildRoutingView(input({ tracks: [midiTrack("t1")] }));
    expect(view.rows[0].target).toBeNull();
    expect(view.rows[0].overrides).toEqual([]);
    expect(view.counts).toEqual({ tracks: 1, routed: 0, unhealthy: 0 });
  });
});

describe("rows", () => {
  it("keeps only MIDI tracks, in project order", () => {
    const view = buildRoutingView(
      input({
        tracks: [
          { id: "a", name: "Drums", kind: "audio", color: "#111111" },
          midiTrack("b", "Bass"),
          { id: "c", name: "Reverb bus", kind: "bus", color: "#222222" },
          midiTrack("d", "Lead"),
          { id: "e", name: "Volume", kind: "automation", color: "#333333" },
        ],
      }),
    );
    expect(view.rows.map((r) => r.trackId)).toEqual(["b", "d"]);
    expect(view.rows.map((r) => r.trackName)).toEqual(["Bass", "Lead"]);
    expect(view.counts.tracks).toBe(2);
  });

  it("carries the track colour, the input target flag and the return device", () => {
    const view = buildRoutingView(
      input({
        tracks: [midiTrack("t1", "Bass", "#ff00aa"), midiTrack("t2", "Lead")],
        outputs: [output("p1")],
        outPorts: [port("p1")],
        routes: [trackRoute("t1", "p1", 0, "Scarlett 2i2"), trackRoute("t2", "p1")],
        targetTrackId: "t2",
      }),
    );
    expect(view.rows[0].trackColor).toBe("#ff00aa");
    expect(view.rows[0].isInputTarget).toBe(false);
    expect(view.rows[0].returnDevice).toBe("Scarlett 2i2");
    expect(view.rows[1].isInputTarget).toBe(true);
    // A route with no `returnDevice` field at all still normalises to null.
    expect(view.rows[1].returnDevice).toBeNull();
  });

  it("ignores a track-scope route belonging to another track", () => {
    const view = buildRoutingView(
      input({
        tracks: [midiTrack("t1"), midiTrack("t2")],
        outputs: [output("p1")],
        outPorts: [port("p1")],
        routes: [trackRoute("t2", "p1", 9)],
      }),
    );
    expect(view.rows[0].target).toBeNull();
    expect(view.rows[1].target?.channel).toBe(9);
  });
});

describe("clip overrides", () => {
  it("attaches clip routes to their own track, in clip order", () => {
    const view = buildRoutingView(
      input({
        tracks: [midiTrack("t1", "Bass"), midiTrack("t2", "Lead")],
        clips: [
          { id: "c1", name: "Verse", trackId: "t1" },
          { id: "c2", name: "Chorus", trackId: "t2" },
          { id: "c3", name: "Bridge", trackId: "t1" },
        ],
        outputs: [output("p1", "Deluge"), output("p2", "Digitone")],
        outPorts: [port("p1", "Deluge"), port("p2", "Digitone")],
        // Deliberately out of clip order: the view sorts by `clips`, not `routes`.
        routes: [clipRoute("c3", "p2", 5), clipRoute("c2", "p1"), clipRoute("c1", "p1", 1)],
      }),
    );
    expect(view.rows[0].overrides.map((o) => o.clipId)).toEqual(["c1", "c3"]);
    expect(view.rows[0].overrides.map((o) => o.clipName)).toEqual(["Verse", "Bridge"]);
    expect(view.rows[0].overrides[1].target).toEqual({
      portId: "p2",
      portName: "Digitone",
      channel: 5,
      health: "open",
    });
    expect(view.rows[1].overrides.map((o) => o.clipId)).toEqual(["c2"]);
  });

  it("skips clips that have no clip-scope route", () => {
    const view = buildRoutingView(
      input({
        tracks: [midiTrack("t1")],
        clips: [
          { id: "c1", name: "Verse", trackId: "t1" },
          { id: "c2", name: "Chorus", trackId: "t1" },
        ],
        outputs: [output("p1")],
        outPorts: [port("p1")],
        routes: [clipRoute("c2", "p1")],
      }),
    );
    expect(view.rows[0].overrides.map((o) => o.clipId)).toEqual(["c2"]);
  });
});

describe("orphans", () => {
  it("reports track-scope routes with no MIDI track, then clip-scope routes with no clip", () => {
    const view = buildRoutingView(
      input({
        tracks: [midiTrack("t1"), { id: "a1", name: "Drums", kind: "audio", color: "#111111" }],
        clips: [{ id: "c1", name: "Verse", trackId: "t1" }],
        outputs: [output("p1", "Deluge")],
        outPorts: [port("p1", "Deluge")],
        routes: [
          clipRoute("c-deleted", "p1", 2),
          trackRoute("t-deleted", "p1", 7),
          trackRoute("t1", "p1"),
          clipRoute("c1", "p1"),
        ],
      }),
    );
    expect(view.orphans).toEqual([
      {
        scope: "track",
        id: "t-deleted",
        target: { portId: "p1", portName: "Deluge", channel: 7, health: "open" },
      },
      {
        scope: "clip",
        id: "c-deleted",
        target: { portId: "p1", portName: "Deluge", channel: 2, health: "open" },
      },
    ]);
  });

  it("treats a route pointing at a non-MIDI track as an orphan", () => {
    const view = buildRoutingView(
      input({
        tracks: [{ id: "a1", name: "Drums", kind: "audio", color: "#111111" }],
        outPorts: [port("p1")],
        routes: [trackRoute("a1", "p1")],
      }),
    );
    expect(view.rows).toEqual([]);
    expect(view.orphans).toHaveLength(1);
    expect(view.orphans[0]).toMatchObject({ scope: "track", id: "a1" });
    expect(view.orphans[0].target.health).toBe("closed");
  });

  it("orphans a clip route whose clip belongs to a track missing from tracks", () => {
    const view = buildRoutingView(
      input({
        tracks: [midiTrack("t1")],
        // c9 still exists in the clip list, but its track was deleted.
        clips: [
          { id: "c1", name: "Verse", trackId: "t1" },
          { id: "c9", name: "Stray", trackId: "t-deleted" },
        ],
        outputs: [output("p1", "Deluge")],
        outPorts: [port("p1", "Deluge"), port("shut-port", "Digitone")],
        routes: [clipRoute("c1", "p1"), clipRoute("c9", "shut-port", 4)],
      }),
    );
    expect(view.rows[0].overrides.map((o) => o.clipId)).toEqual(["c1"]);
    expect(view.orphans).toEqual([
      {
        scope: "clip",
        id: "c9",
        target: { portId: "shut-port", portName: "Digitone", channel: 4, health: "closed" },
      },
    ]);
    // The orphan's port is closed, so it counts — because the port is dead, not
    // because the route is orphaned.
    expect(view.counts).toEqual({ tracks: 1, routed: 0, unhealthy: 1 });
  });

  it("orphans a clip route whose clip belongs to an audio track", () => {
    const view = buildRoutingView(
      input({
        tracks: [midiTrack("t1"), { id: "a1", name: "Drums", kind: "audio", color: "#111111" }],
        clips: [{ id: "c-audio", name: "Loop", trackId: "a1" }],
        outputs: [output("p1", "Deluge")],
        outPorts: [port("p1", "Deluge")],
        routes: [clipRoute("c-audio", "p1", 2)],
      }),
    );
    expect(view.rows.map((r) => r.trackId)).toEqual(["t1"]);
    expect(view.rows[0].overrides).toEqual([]);
    expect(view.orphans).toEqual([
      {
        scope: "clip",
        id: "c-audio",
        target: { portId: "p1", portName: "Deluge", channel: 2, health: "open" },
      },
    ]);
    // Open port: orphan-ness alone must not make it unhealthy.
    expect(view.counts).toEqual({ tracks: 1, routed: 0, unhealthy: 0 });
  });

  it("surfaces every input route exactly once, on a row or in orphans", () => {
    const routes = [
      trackRoute("t1", "p1"), // on a row
      clipRoute("c1", "p1"), // an override on that row
      trackRoute("t-gone", "p1"), // orphan: no such track
      clipRoute("c-gone", "p1"), // orphan: no such clip
      clipRoute("c-audio", "p1"), // orphan: clip lives on an audio track
      clipRoute("c-homeless", "p1"), // orphan: clip's track was deleted
      trackRoute("a1", "p1"), // orphan: the track is audio, so it has no row
    ];
    const view = buildRoutingView(
      input({
        tracks: [midiTrack("t1"), { id: "a1", name: "Drums", kind: "audio", color: "#111111" }],
        clips: [
          { id: "c1", name: "Verse", trackId: "t1" },
          { id: "c-audio", name: "Loop", trackId: "a1" },
          { id: "c-homeless", name: "Stray", trackId: "t-deleted" },
        ],
        outputs: [output("p1")],
        outPorts: [port("p1")],
        routes,
      }),
    );
    const surfaced = [
      ...view.rows.flatMap((r) => [
        ...(r.target ? [`track:${r.trackId}`] : []),
        ...r.overrides.map((o) => `clip:${o.clipId}`),
      ]),
      ...view.orphans.map((o) => `${o.scope}:${o.id}`),
    ];
    expect(surfaced.sort()).toEqual(
      routes.map((r) => `${r.scope}:${r.id}`).sort(),
    );
    expect(view.orphans).toHaveLength(5);
    expect(view.orphans.map((o) => o.scope)).toEqual(["track", "track", "clip", "clip", "clip"]);
  });

  it("finds nothing to orphan when every route has a home", () => {
    const view = buildRoutingView(
      input({
        tracks: [midiTrack("t1")],
        clips: [{ id: "c1", name: "Verse", trackId: "t1" }],
        outputs: [output("p1")],
        outPorts: [port("p1")],
        routes: [trackRoute("t1", "p1"), clipRoute("c1", "p1")],
      }),
    );
    expect(view.orphans).toEqual([]);
  });
});

describe("counts", () => {
  it("counts routed tracks and every unhealthy target, overrides included", () => {
    const view = buildRoutingView(
      input({
        tracks: [
          midiTrack("t1", "Bass"),
          midiTrack("t2", "Lead"),
          midiTrack("t3", "Pad"),
          midiTrack("t4", "Silent"),
          { id: "a1", name: "Drums", kind: "audio", color: "#111111" },
        ],
        clips: [
          { id: "c1", name: "Verse", trackId: "t1" },
          { id: "c2", name: "Chorus", trackId: "t2" },
        ],
        outputs: [output("open-port")],
        outPorts: [port("open-port"), port("shut-port")],
        routes: [
          trackRoute("t1", "open-port"), // healthy
          trackRoute("t2", "shut-port"), // closed  → unhealthy
          trackRoute("t3", "vanished"), // missing → unhealthy
          clipRoute("c1", "open-port"), // healthy override
          clipRoute("c2", "vanished"), // missing override → unhealthy
        ],
      }),
    );
    // t4 has no route, so it is a row but not a routed one.
    expect(view.counts).toEqual({ tracks: 4, routed: 3, unhealthy: 3 });
  });

  it("never counts an orphan as a track or as routed", () => {
    const view = buildRoutingView(
      input({
        tracks: [midiTrack("t1")],
        outputs: [output("p1")],
        outPorts: [port("p1")],
        routes: [trackRoute("t1", "p1"), trackRoute("t-gone", "p1")],
      }),
    );
    // Both routes point at the same open port, so nothing is unhealthy.
    expect(view.counts).toEqual({ tracks: 1, routed: 1, unhealthy: 0 });
    expect(view.orphans).toHaveLength(1);
  });

  it("counts an orphan pointing at a dead port, and not one pointing at an open port", () => {
    const base = {
      tracks: [midiTrack("t1")],
      outputs: [output("p1")],
      outPorts: [port("p1")],
    };
    const healthy = buildRoutingView(input({ ...base, routes: [trackRoute("t-gone", "p1")] }));
    expect(healthy.counts.unhealthy).toBe(0);

    const dead = buildRoutingView(input({ ...base, routes: [trackRoute("t-gone", "vanished")] }));
    expect(dead.counts.unhealthy).toBe(1);
    expect(dead.orphans[0].target.health).toBe("missing");
  });
});

describe("PORT_ACCENTS", () => {
  it("names five saturated CSS custom properties, cyan first", () => {
    expect(PORT_ACCENTS).toEqual(["--cyan", "--magenta", "--amber", "--green", "--orange"]);
    for (const name of PORT_ACCENTS) expect(name.startsWith("--")).toBe(true);
  });
});

describe("portAccents", () => {
  it("assigns open ports first, in outputs order", () => {
    const accents = portAccents([output("p2"), output("p1")], []);
    expect(accents.get("p2")).toBe(0);
    expect(accents.get("p1")).toBe(1);
  });

  it("appends the remaining routed ports sorted by id", () => {
    const accents = portAccents([output("zz-open")], [
      trackRoute("t1", "banana"),
      trackRoute("t2", "apple"),
      trackRoute("t3", "zz-open"),
    ]);
    expect(accents.get("zz-open")).toBe(0);
    expect(accents.get("apple")).toBe(1);
    expect(accents.get("banana")).toBe(2);
  });

  it("gives the same assignment whatever order the routes arrive in", () => {
    const outputs = [output("live-a"), output("live-b")];
    const routes = [
      trackRoute("t1", "live-b"),
      clipRoute("c1", "delta"),
      trackRoute("t2", "alpha"),
      trackRoute("t3", "charlie"),
    ];
    const first = portAccents(outputs, routes);
    const shuffled = portAccents(outputs, [routes[3], routes[0], routes[2], routes[1]]);
    expect([...shuffled.entries()].sort()).toEqual([...first.entries()].sort());
    expect(first.get("live-a")).toBe(0);
    expect(first.get("live-b")).toBe(1);
    expect(first.get("alpha")).toBe(2);
    expect(first.get("charlie")).toBe(3);
    expect(first.get("delta")).toBe(4);
  });

  it("ignores a port mentioned by several routes", () => {
    const accents = portAccents([], [trackRoute("t1", "p9"), clipRoute("c1", "p9")]);
    expect(accents.size).toBe(1);
    expect(accents.get("p9")).toBe(0);
  });

  it("wraps past the end of PORT_ACCENTS without colliding inside the first five", () => {
    const ids = ["p0", "p1", "p2", "p3", "p4", "p5", "p6"];
    const accents = portAccents(
      ids.map((id) => output(id)),
      [],
    );
    const slots = ids.map((id) => accents.get(id));
    expect(slots).toEqual([0, 1, 2, 3, 4, 0, 1]);
    expect(new Set(slots.slice(0, PORT_ACCENTS.length)).size).toBe(PORT_ACCENTS.length);
    for (const slot of slots) {
      expect(slot).toBeGreaterThanOrEqual(0);
      expect(slot).toBeLessThan(PORT_ACCENTS.length);
    }
  });

  it("returns an empty map when nothing is open and nothing is routed", () => {
    expect(portAccents([], []).size).toBe(0);
  });
});
