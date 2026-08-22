/**
 * The lane plugin strip (design §3.4, plan §6.3): the chain and its jump
 * targets, right on the track lane — "I am looking at the track" without
 * ever opening the manager. Sibling projection to the rack
 * (`plugin-rack.ts`) and the matrix (`automation-matrix.ts`); this one
 * groups by DEVICE, in chain order, instead of by instance-globally or by
 * parameter, because "what's on this track, in what order" is the strip's
 * one job.
 *
 * `buildLaneStrip` is `buildRack` scoped to one track: `buildRack({
 * instances, descriptors: [], tracks: [track], bindings })` already walks
 * `track.instrumentId`/`track.inserts` in exactly instrument-then-slot-
 * order, already drops a dead insert reference (a placement whose
 * instanceId never resolves in `instances` gets no `RackEntry`), and
 * already gives each entry its `automated` param ids pre-sorted and
 * deduped — reusing it here means the `plugin:<id>` / `inserts[]` join and
 * the bindings → automated-ids walk exist in exactly one place, the same
 * reasoning `automation-matrix.ts` uses for its own `buildRack` call.
 *
 * `buildLaneStrip` returns every device and every one of its chips —
 * trimming for the header's width budget is `fitLaneStrip`'s job, so the
 * same projection serves the folded and unfolded strip without asking the
 * DOM anything (Ruling P-6: the overflow budget is a constant, not a
 * measurement).
 */

import type { Binding, PluginInstanceInfo, PluginParamInfo, TrackState } from "../types/ipc";
import { buildRack, type ParamRef, type Placement, type RackEntry } from "./plugin-rack";
import { formatParamDisplay, shortParamName } from "./plugin-params";

export interface StripChip {
  paramId: number;
  /** Short param name when the param metadata is cached, else `#12`. */
  label: string;
  /** Formatted value, or "~" when not cached. */
  valueText: string;
  /** "automated" when a binding points at it, else "plain". */
  state: "plain" | "automated";
}

export interface StripDevice {
  instanceId: string;
  name: string;
  status: PluginInstanceInfo["status"];
  kind: "instrument" | "insert";
  /** undefined for the instrument. */
  slotIndex?: number;
  /** false for the instrument. */
  bypassed: boolean;
  chips: StripChip[];
}

export interface LaneStripInput {
  track: TrackState;
  instances: readonly PluginInstanceInfo[];
  bindings: readonly Binding[];
  /** uid -> pinned param ids (`plugins.pinnedParamsFor`). */
  pinnedFor: (uid: string) => readonly number[];
  paramInfo: (instanceId: string, paramId: number) => PluginParamInfo | undefined;
}

export interface StripFit {
  shown: StripDevice[];
  /** Devices that did not fit — rendered as `+N`. 0 = no overflow. */
  overflow: number;
}

/** One device's chip set: pinned params first (in `pinnedFor` order), then
 * `entry.automated` (already ascending `paramId`, already deduped by
 * `buildRack`), de-duplicated against the pinned ones. `state` is
 * "automated" whenever a binding targets the param, whichever list put it
 * on the strip. */
function chipsFor(
  instance: PluginInstanceInfo,
  automated: readonly ParamRef[],
  pinnedFor: LaneStripInput["pinnedFor"],
  paramInfo: LaneStripInput["paramInfo"],
): StripChip[] {
  const automatedIds = new Set(automated.map((a) => a.paramId));
  const makeChip = (paramId: number): StripChip => {
    const info = paramInfo(instance.id, paramId);
    return {
      paramId,
      label: info ? shortParamName(info.name) : `#${paramId}`,
      valueText: info ? formatParamDisplay(info) : "~",
      state: automatedIds.has(paramId) ? "automated" : "plain",
    };
  };

  const seen = new Set<number>();
  const chips: StripChip[] = [];
  for (const paramId of pinnedFor(instance.uid)) {
    if (seen.has(paramId)) continue;
    seen.add(paramId);
    chips.push(makeChip(paramId));
  }
  for (const { paramId } of automated) {
    if (seen.has(paramId)) continue;
    seen.add(paramId);
    chips.push(makeChip(paramId));
  }
  return chips;
}

/** One `RackEntry` placement → one strip device. `buildRack` already
 * distinguishes "instrument" vs "insert" placements and carries the
 * insert's `slotIndex`/`bypassed`, so this is a straight field copy. */
function deviceFor(
  entry: RackEntry,
  placement: Placement,
  pinnedFor: LaneStripInput["pinnedFor"],
  paramInfo: LaneStripInput["paramInfo"],
): StripDevice {
  return {
    instanceId: entry.instance.id,
    name: entry.instance.name,
    status: entry.instance.status,
    kind: placement.kind,
    slotIndex: placement.kind === "insert" ? placement.slotIndex : undefined,
    bypassed: placement.kind === "insert" ? placement.bypassed : false,
    chips: chipsFor(entry.instance, entry.automated, pinnedFor, paramInfo),
  };
}

/**
 * Build the strip: the plugin instrument first (when `track.instrumentId`
 * resolves to a live instance — a non-plugin instrument contributes
 * nothing, the existing instrument chip already covers it), then
 * `track.inserts` in slot order. An insert slot whose instance is not live
 * (removed, never resolved) is skipped rather than shown as a gap — that
 * drop happens for free inside `buildRack` (a placement with no matching
 * live instance never gets a `RackEntry`).
 */
export function buildLaneStrip(input: LaneStripInput): StripDevice[] {
  const { track, instances, bindings, pinnedFor, paramInfo } = input;

  // Scoped to THIS track only: an entry with placements is on `track` (the
  // only track `buildRack` walked), an entry with none is not — orphans
  // and instances placed on other tracks read the same way here, neither
  // belongs on this strip.
  const rack = buildRack({ instances, descriptors: [], tracks: [track], bindings });

  return rack.flatMap((entry) =>
    entry.placements.map((placement) => deviceFor(entry, placement, pinnedFor, paramInfo)),
  );
}

/** Trim to the header's width budget: the first `maxEntries` devices, each
 * cut to its first `chipsPerEntry` chips (0 = drop every chip — the folded
 * lane's "dots only"). Does not mutate `devices`. */
export function fitLaneStrip(
  devices: readonly StripDevice[],
  opts: { maxEntries: number; chipsPerEntry: number },
): StripFit {
  const shown = devices.slice(0, opts.maxEntries).map((d) => ({
    ...d,
    chips: d.chips.slice(0, opts.chipsPerEntry),
  }));
  return { shown, overflow: devices.length - shown.length };
}
