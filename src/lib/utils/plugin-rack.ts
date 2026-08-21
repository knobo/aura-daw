/**
 * The rack projection (design §4.4, plan §5.1): what plugins are actually
 * live in this project, where each one sits, and which of its parameters
 * move.
 *
 * Derived, never stored. `plugins.instances` is the spine; tracks and
 * modulation bindings are joined onto it. That direction matters — an
 * instance nothing points at still gets an entry, with an empty
 * `placements`, because an orphaned or crashed instance is exactly what no
 * other view in the app will ever show you, and a track-first projection
 * would silently drop it.
 */

import type { Binding, PluginDescriptor, PluginInstanceInfo, TrackState } from "../types/ipc";

export type Placement =
  | { kind: "instrument"; trackId: string; trackName: string }
  | {
      kind: "insert";
      trackId: string;
      trackName: string;
      slotIndex: number;
      bypassed: boolean;
    };

/** One plugin parameter, addressed the way a `pluginParam` binding does. */
export interface ParamRef {
  instanceId: string;
  paramId: number;
}

export interface RackEntry {
  instance: PluginInstanceInfo;
  /** Undefined when the instance's uid is not in the current scan — an
   * instance can outlive the descriptor that produced it (uninstalled
   * bundle, scan not run yet on this machine). */
  descriptor?: PluginDescriptor;
  /** Every place this instance sits. Empty = orphan. */
  placements: Placement[];
  /** Params with a modulation curve pointed at them, ascending by id. */
  automated: ParamRef[];
  /** Params bound to a MIDI controller. Always empty until MIDI Learn
   * lands (design §7) — the shape is here so the chip can already ask. */
  mapped: ParamRef[];
}

export interface RackInput {
  instances: readonly PluginInstanceInfo[];
  descriptors: readonly PluginDescriptor[];
  tracks: readonly TrackState[];
  bindings: readonly Binding[];
}

export interface RackCounts {
  total: number;
  stub: number;
  crashed: number;
  orphans: number;
  /** Automated PARAMETERS, not instances — "11 params move" is the useful
   * number; "3 plugins have automation" hides the density. */
  automated: number;
}

/**
 * Build the rack. Ordering is the track rail's order — an instrument
 * before that track's inserts, inserts in slot order — with orphans last,
 * so the ledger reads the same way the arrangement does.
 */
export function buildRack(input: RackInput): RackEntry[] {
  const { instances, descriptors, tracks, bindings } = input;

  const descriptorByUid = new Map(descriptors.map((d) => [d.uid, d]));
  const placements = new Map<string, Placement[]>();
  /** Sort key per instance: where it first appears walking the tracks. */
  const order = new Map<string, number>();

  const addPlacement = (instanceId: string, placement: Placement, rank: number) => {
    const list = placements.get(instanceId);
    if (list) list.push(placement);
    else placements.set(instanceId, [placement]);
    if (!order.has(instanceId)) order.set(instanceId, rank);
  };

  let rank = 0;
  for (const track of tracks) {
    if (track.instrumentId?.startsWith("plugin:")) {
      const instanceId = track.instrumentId.slice("plugin:".length);
      addPlacement(instanceId, { kind: "instrument", trackId: track.id, trackName: track.name }, rank++);
    }
    const inserts = track.inserts ?? [];
    for (let slotIndex = 0; slotIndex < inserts.length; slotIndex++) {
      const slot = inserts[slotIndex];
      addPlacement(
        slot.instanceId,
        {
          kind: "insert",
          trackId: track.id,
          trackName: track.name,
          slotIndex,
          bypassed: slot.bypassed,
        },
        rank++,
      );
    }
  }

  const automatedIds = new Map<string, Set<number>>();
  for (const binding of bindings) {
    if (binding.target.kind !== "pluginParam") continue;
    const { instanceId, paramId } = binding.target;
    let ids = automatedIds.get(instanceId);
    if (!ids) {
      ids = new Set();
      automatedIds.set(instanceId, ids);
    }
    ids.add(paramId);
  }

  const ORPHAN_RANK = Number.MAX_SAFE_INTEGER;
  return instances
    .map((instance, index) => ({ instance, index }))
    .sort(
      (a, b) =>
        (order.get(a.instance.id) ?? ORPHAN_RANK) - (order.get(b.instance.id) ?? ORPHAN_RANK) ||
        a.index - b.index,
    )
    .map(({ instance }) => ({
      instance,
      descriptor: descriptorByUid.get(instance.uid),
      placements: placements.get(instance.id) ?? [],
      automated: [...(automatedIds.get(instance.id) ?? [])]
        .sort((a, b) => a - b)
        .map((paramId) => ({ instanceId: instance.id, paramId })),
      mapped: [],
    }));
}

/** The footer line: what is broken, what is unused, how much moves. */
export function rackCounts(rack: readonly RackEntry[]): RackCounts {
  return {
    total: rack.length,
    stub: rack.filter((e) => e.instance.status === "stub").length,
    crashed: rack.filter((e) => e.instance.status === "crashed").length,
    orphans: rack.filter((e) => e.placements.length === 0).length,
    automated: rack.reduce((sum, e) => sum + e.automated.length, 0),
  };
}

/**
 * Group a rack by track for rendering, orphans in a trailing bucket with a
 * null `trackId`. An instance placed on several tracks appears under each
 * of them — that IS the useful reading of "one reverb on three tracks" —
 * but only once per track, however many slots it holds there.
 */
export function rackByTrack(
  rack: readonly RackEntry[],
): { trackId: string | null; trackName: string; entries: RackEntry[] }[] {
  const order: string[] = [];
  const groups = new Map<string, { trackName: string; entries: RackEntry[] }>();
  const orphans: RackEntry[] = [];

  for (const entry of rack) {
    if (entry.placements.length === 0) {
      orphans.push(entry);
      continue;
    }
    const seen = new Set<string>();
    for (const placement of entry.placements) {
      if (seen.has(placement.trackId)) continue;
      seen.add(placement.trackId);
      let group = groups.get(placement.trackId);
      if (!group) {
        group = { trackName: placement.trackName, entries: [] };
        groups.set(placement.trackId, group);
        order.push(placement.trackId);
      }
      group.entries.push(entry);
    }
  }

  const out: { trackId: string | null; trackName: string; entries: RackEntry[] }[] = order.map(
    (trackId) => {
      const group = groups.get(trackId);
      return { trackId, trackName: group?.trackName ?? "", entries: group?.entries ?? [] };
    },
  );
  if (orphans.length > 0) {
    out.push({ trackId: null, trackName: "Not on any track", entries: orphans });
  }
  return out;
}
