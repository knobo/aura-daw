/**
 * The lane plugin strip (design §3.4, plan §6.3): the chain and its jump
 * targets, right on the track lane — "I am looking at the track" without
 * ever opening the manager. Sibling projection to the rack
 * (`plugin-rack.ts`) and the matrix (`automation-matrix.ts`); this one
 * groups by DEVICE, in chain order, instead of by instance-globally or by
 * parameter, because "what's on this track, in what order" is the strip's
 * one job.
 *
 * `buildLaneStrip` returns every device and every one of its chips —
 * trimming for the header's width budget is `fitLaneStrip`'s job, so the
 * same projection serves the folded and unfolded strip without asking the
 * DOM anything (Ruling P-6: the overflow budget is a constant, not a
 * measurement).
 */

import type {
  Binding,
  PluginInstanceInfo,
  PluginParamInfo,
  TrackState,
} from "../types/ipc";
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
 * automated params in ascending `paramId`, de-duplicated against the pinned
 * ones. `state` is "automated" whenever a binding targets the param,
 * whichever list put it on the strip. */
function chipsFor(
  instance: PluginInstanceInfo,
  automated: ReadonlySet<number>,
  pinnedFor: LaneStripInput["pinnedFor"],
  paramInfo: LaneStripInput["paramInfo"],
): StripChip[] {
  const makeChip = (paramId: number): StripChip => {
    const info = paramInfo(instance.id, paramId);
    return {
      paramId,
      label: info ? shortParamName(info.name) : `#${paramId}`,
      valueText: info ? formatParamDisplay(info) : "~",
      state: automated.has(paramId) ? "automated" : "plain",
    };
  };

  const seen = new Set<number>();
  const chips: StripChip[] = [];
  for (const paramId of pinnedFor(instance.uid)) {
    if (seen.has(paramId)) continue;
    seen.add(paramId);
    chips.push(makeChip(paramId));
  }
  const rest = [...automated].filter((id) => !seen.has(id)).sort((a, b) => a - b);
  for (const paramId of rest) chips.push(makeChip(paramId));
  return chips;
}

/**
 * Build the strip: the plugin instrument first (when `track.instrumentId`
 * resolves to a live instance — a non-plugin instrument contributes
 * nothing, the existing instrument chip already covers it), then
 * `track.inserts` in slot order. An insert slot whose instance is not live
 * (removed, never resolved) is skipped rather than shown as a gap.
 */
export function buildLaneStrip(input: LaneStripInput): StripDevice[] {
  const { track, instances, bindings, pinnedFor, paramInfo } = input;
  const instanceById = new Map(instances.map((i) => [i.id, i]));

  const automatedByInstance = new Map<string, Set<number>>();
  for (const binding of bindings) {
    if (binding.target.kind !== "pluginParam") continue;
    const { instanceId, paramId } = binding.target;
    let ids = automatedByInstance.get(instanceId);
    if (!ids) {
      ids = new Set();
      automatedByInstance.set(instanceId, ids);
    }
    ids.add(paramId);
  }
  const EMPTY = new Set<number>();
  const automatedFor = (instanceId: string) => automatedByInstance.get(instanceId) ?? EMPTY;

  const devices: StripDevice[] = [];

  if (track.instrumentId?.startsWith("plugin:")) {
    const instance = instanceById.get(track.instrumentId.slice("plugin:".length));
    if (instance) {
      devices.push({
        instanceId: instance.id,
        name: instance.name,
        status: instance.status,
        kind: "instrument",
        bypassed: false,
        chips: chipsFor(instance, automatedFor(instance.id), pinnedFor, paramInfo),
      });
    }
  }

  const inserts = track.inserts ?? [];
  for (let slotIndex = 0; slotIndex < inserts.length; slotIndex++) {
    const slot = inserts[slotIndex];
    const instance = instanceById.get(slot.instanceId);
    if (!instance) continue;
    devices.push({
      instanceId: instance.id,
      name: instance.name,
      status: instance.status,
      kind: "insert",
      slotIndex,
      bypassed: slot.bypassed,
      chips: chipsFor(instance, automatedFor(instance.id), pinnedFor, paramInfo),
    });
  }

  return devices;
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
