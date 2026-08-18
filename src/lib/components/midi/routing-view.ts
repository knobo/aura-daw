/**
 * The patchbay's view model: turn the flat MIDI-I/O status lists into one row
 * per MIDI track, so the panel can answer "what is patched where?" at a glance.
 *
 * Pure on purpose — no runes, no store imports. The panel hands in plain
 * snapshots of `project.tracks`, `midi.clips` and the `midiIo` status lists and
 * gets back everything it needs to draw, including the per-port accent slot
 * that keeps a cord's colour from jumping between 500 ms polls.
 *
 * The invariant that makes the panel trustworthy: **every route in the input
 * surfaces exactly once** — either on a row (as the track target or as a clip
 * override) or in `orphans`. A live route is never silently dropped, because a
 * route the user cannot see is worse than one shown as broken. `counts.unhealthy`
 * follows the same rule: it is driven by port health alone, wherever the route
 * surfaced.
 */
import type { MidiOutputPortStatus, MidiPortInfo, MidiRouteStatus } from "../../types/ipc";

/**
 * How much of the signal path actually exists right now.
 * - `open`    — the port is open: notes are flowing.
 * - `closed`  — the device is there but nobody opened the port.
 * - `missing` — the device is gone: unplugged, or renamed by the OS.
 */
export type PortHealth = "open" | "closed" | "missing";

export interface RouteTarget {
  portId: string;
  /** Human name from `outputs`/`outPorts`; falls back to the raw portId when
   * the device is missing and there is no name left to look up. */
  portName: string;
  /** Stored 0-15. The panel labels it `ch N+1`. */
  channel: number;
  health: PortHealth;
}

export interface ClipRouteRow {
  clipId: string;
  clipName: string;
  target: RouteTarget;
}

export interface TrackRoutingRow {
  trackId: string;
  trackName: string;
  trackColor: string;
  /** This track is the one MIDI-input target (`targetTrackId`). */
  isInputTarget: boolean;
  /** The track-scope route, or null when the track sends nowhere. */
  target: RouteTarget | null;
  returnDevice: string | null;
  /** Clip-scope routes that beat the track route, in project clip order. */
  overrides: ClipRouteRow[];
}

export interface OrphanRoute {
  scope: "track" | "clip";
  id: string;
  target: RouteTarget;
}

export interface RoutingView {
  /** Every MIDI track, in project order. */
  rows: TrackRoutingRow[];
  /** Routes whose track or clip is gone from the project. */
  orphans: OrphanRoute[];
  counts: { tracks: number; routed: number; unhealthy: number };
}

export interface RoutingInput {
  tracks: { id: string; name: string; kind: string; color: string }[];
  clips: { id: string; name: string; trackId: string }[];
  outputs: MidiOutputPortStatus[];
  outPorts: MidiPortInfo[];
  routes: MidiRouteStatus[];
  targetTrackId: string | null;
}

/** CSS custom-property names, in assignment order. The saturated accents from
 * `src/app.css`, cyan first because cyan is the app's primary. */
export const PORT_ACCENTS: readonly string[] = [
  "--cyan",
  "--magenta",
  "--amber",
  "--green",
  "--orange",
];

/** A route target is unhealthy when notes are not actually leaving AURA. */
function isUnhealthy(target: RouteTarget): boolean {
  return target.health === "closed" || target.health === "missing";
}

export function buildRoutingView(input: RoutingInput): RoutingView {
  const { tracks, clips, outputs, outPorts, routes, targetTrackId } = input;

  const openIds = new Set(outputs.map((o) => o.port.id));
  const knownIds = new Set(outPorts.map((p) => p.id));
  const names = new Map<string, string>();
  // outPorts first, then outputs: an open port carries the freshest name.
  for (const p of outPorts) names.set(p.id, p.name);
  for (const o of outputs) names.set(o.port.id, o.port.name);

  const targetFor = (route: MidiRouteStatus): RouteTarget => ({
    portId: route.portId,
    portName: names.get(route.portId) ?? route.portId,
    channel: route.channel,
    health: openIds.has(route.portId) ? "open" : knownIds.has(route.portId) ? "closed" : "missing",
  });

  // First route of a given scope+id wins; the backend should not emit duplicates.
  const trackRoutes = new Map<string, MidiRouteStatus>();
  const clipRoutes = new Map<string, MidiRouteStatus>();
  for (const route of routes) {
    const bucket = route.scope === "track" ? trackRoutes : clipRoutes;
    if (!bucket.has(route.id)) bucket.set(route.id, route);
  }

  const clipsByTrack = new Map<string, { id: string; name: string; trackId: string }[]>();
  for (const clip of clips) {
    const list = clipsByTrack.get(clip.trackId);
    if (list) list.push(clip);
    else clipsByTrack.set(clip.trackId, [clip]);
  }

  const rows: TrackRoutingRow[] = [];
  let routed = 0;
  // Which routes found a home on a row. Anything left over is an orphan.
  const rowTrackIds = new Set<string>();
  const surfacedClipIds = new Set<string>();

  for (const track of tracks) {
    if (track.kind !== "midi") continue;
    rowTrackIds.add(track.id);

    const route = trackRoutes.get(track.id);
    const target = route ? targetFor(route) : null;
    if (target) routed += 1;

    const overrides: ClipRouteRow[] = [];
    for (const clip of clipsByTrack.get(track.id) ?? []) {
      const clipRoute = clipRoutes.get(clip.id);
      if (!clipRoute) continue;
      surfacedClipIds.add(clip.id);
      overrides.push({ clipId: clip.id, clipName: clip.name, target: targetFor(clipRoute) });
    }

    rows.push({
      trackId: track.id,
      trackName: track.name,
      trackColor: track.color,
      isInputTarget: track.id === targetTrackId,
      target,
      returnDevice: route?.returnDevice ?? null,
      overrides,
    });
  }

  // One rule per scope: a route with no row to live on is an orphan. For clips
  // that covers all three ways a clip route can lose its home — the clip is
  // gone from `clips`, its track is gone from `tracks`, or its track is there
  // but is not a MIDI track. Track-scope orphans first, then clip-scope, each
  // in `routes` order.
  const orphans: OrphanRoute[] = [];
  for (const route of trackRoutes.values()) {
    if (rowTrackIds.has(route.id)) continue;
    orphans.push({ scope: "track", id: route.id, target: targetFor(route) });
  }
  for (const route of clipRoutes.values()) {
    if (surfacedClipIds.has(route.id)) continue;
    orphans.push({ scope: "clip", id: route.id, target: targetFor(route) });
  }

  // Health arithmetic is about ports, not about where a route surfaced: an
  // orphan pointing at an open port is not unhealthy, and a route on a row
  // pointing at a dead port is.
  let unhealthy = 0;
  for (const row of rows) {
    if (row.target && isUnhealthy(row.target)) unhealthy += 1;
    for (const override of row.overrides) if (isUnhealthy(override.target)) unhealthy += 1;
  }
  for (const orphan of orphans) if (isUnhealthy(orphan.target)) unhealthy += 1;

  return { rows, orphans, counts: { tracks: rows.length, routed, unhealthy } };
}

/**
 * Stable accent slot per port so a colour never jumps between polls: open ports
 * in `outputs` order first, then any other portIds appearing in `routes` sorted
 * by id. Value is an index into `PORT_ACCENTS`, wrapped so more ports than
 * accents still resolves.
 */
export function portAccents(
  outputs: MidiOutputPortStatus[],
  routes: MidiRouteStatus[],
): Map<string, number> {
  const order: string[] = [];
  const seen = new Set<string>();
  for (const o of outputs) {
    if (seen.has(o.port.id)) continue;
    seen.add(o.port.id);
    order.push(o.port.id);
  }
  const rest = [...new Set(routes.map((r) => r.portId))]
    .filter((id) => !seen.has(id))
    .sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
  order.push(...rest);

  const accents = new Map<string, number>();
  order.forEach((id, i) => accents.set(id, i % PORT_ACCENTS.length));
  return accents;
}
