/**
 * The automation matrix (design §6.1, plan §6.1): the same projection as
 * the rack (`plugin-rack.ts`), grouped by parameter instead of by
 * instance, plus track-param curves (gain, pan) — everything that moves
 * in this project, in one list.
 *
 * Derived, never stored. One row per binding in `modulation.bindings`; a
 * binding with no resolvable track is dropped rather than shown with an
 * invented placeholder. `trackId` is resolved from the binding's
 * `source`, not its `target`, because the source is where the curve
 * actually lives (the lane a click needs to land on) — a `curve` source
 * pointing at a `pluginParam` target still has to walk the instance back
 * to a track, which is what `buildRack` already knows how to do.
 */

import type {
  AutomationMode,
  Binding,
  PluginInstanceInfo,
  PluginParamInfo,
  TargetRef,
  TrackState,
} from "../types/ipc";
import { formatDb, formatPan } from "./format";
import { buildRack } from "./plugin-rack";
import { formatParamDisplay, shortParamName } from "./plugin-params";

export interface MatrixRow {
  bindingId: string;
  /** The lane this parameter's curve lives on. */
  trackId: string;
  trackName: string;
  /** Host plugin instance, or null for a track param (gain / pan). */
  instanceId: string | null;
  pluginName: string | null;
  /** Short parameter name — "Cutoff", "gain", "pan". */
  paramLabel: string;
  /** Formatted current value, or "~" when it cannot be resolved. */
  valueText: string;
  target: TargetRef;
  laneVisible: boolean;
  mode: AutomationMode;
}

export interface MatrixInput {
  bindings: readonly Binding[];
  instances: readonly PluginInstanceInfo[];
  tracks: readonly TrackState[];
  /** Which lanes currently show an overlay — `modulation.visible`. */
  visible: ReadonlyMap<string, ReadonlySet<string>>;
  /** Cached plugin param metadata; undefined when not enumerated yet. */
  paramInfo: (instanceId: string, paramId: number) => PluginParamInfo | undefined;
}

/** Resolve `binding`'s lane track id from its SOURCE, not its target. A
 * `curve` source addressing a `pluginParam` target has to walk the
 * instance to its track — reuse `buildRack`'s placement join rather than
 * re-deriving the `plugin:<id>` / `inserts[]` relationship by hand. An
 * instance placed on several tracks uses its first placement, same as the
 * rack. `clipEnvelope` sources are not a lane at all (nothing to reveal)
 * and every other source kind (`lfo`/`macro`/`midiCc`/`envFollower`) has
 * no producer in this app yet — both resolve to undefined and the binding
 * is dropped by the caller. */
function laneTrackId(
  binding: Binding,
  trackForInstance: ReadonlyMap<string, string>,
): string | undefined {
  switch (binding.source.kind) {
    case "automationTrack":
      return binding.source.trackId;
    case "curve":
      if (binding.target.kind === "trackParam") return binding.target.trackId;
      if (binding.target.kind === "pluginParam") {
        return trackForInstance.get(binding.target.instanceId);
      }
      return undefined;
    default:
      return undefined;
  }
}

export function buildMatrix(input: MatrixInput): MatrixRow[] {
  const trackById = new Map(input.tracks.map((t) => [t.id, t]));
  const instanceById = new Map(input.instances.map((i) => [i.id, i]));

  // Only used to resolve a pluginParam's host track via its placements —
  // descriptors and automated-param bookkeeping are irrelevant here.
  const rack = buildRack({
    instances: input.instances,
    descriptors: [],
    tracks: input.tracks,
    bindings: input.bindings,
  });
  const trackForInstance = new Map<string, string>();
  for (const entry of rack) {
    const first = entry.placements[0];
    if (first) trackForInstance.set(entry.instance.id, first.trackId);
  }

  const rows: MatrixRow[] = [];
  for (const binding of input.bindings) {
    const trackId = laneTrackId(binding, trackForInstance);
    const track = trackId ? trackById.get(trackId) : undefined;
    if (!track) continue; // no resolvable lane — do not invent a placeholder

    let instanceId: string | null = null;
    let pluginName: string | null = null;
    let paramLabel: string;
    let valueText: string;

    if (binding.target.kind === "trackParam") {
      paramLabel = binding.target.param;
      if (binding.target.param === "gain") valueText = formatDb(track.gainDb);
      else if (binding.target.param === "pan") valueText = formatPan(track.pan);
      // mute/send0/send1 automation has no display format yet — out of
      // scope for this pass, same as the macro/port target kinds below.
      else valueText = "~";
    } else if (binding.target.kind === "pluginParam") {
      instanceId = binding.target.instanceId;
      pluginName = instanceById.get(instanceId)?.name ?? null;
      const info = input.paramInfo(instanceId, binding.target.paramId);
      paramLabel = info ? shortParamName(info.name) : `#${binding.target.paramId}`;
      valueText = info ? formatParamDisplay(info) : "~";
    } else {
      // macro / port / selfTrackParam / selfInstrumentParam — arrive with
      // modulation §8; out of scope for this pass.
      continue;
    }

    rows.push({
      bindingId: binding.id,
      trackId: track.id,
      trackName: track.name,
      instanceId,
      pluginName,
      paramLabel,
      valueText,
      target: binding.target,
      laneVisible: input.visible.get(track.id)?.has(binding.id) ?? false,
      mode: track.automationMode,
    });
  }

  return rows.sort(
    (a, b) =>
      cmpFold(a.paramLabel, b.paramLabel) ||
      cmpFold(a.trackName, b.trackName) ||
      cmpFold(a.pluginName ?? "", b.pluginName ?? ""),
  );
}

/** Case-insensitive compare with no locale-specific collation — plain
 * lowercasing, not `Intl`/`localeCompare` (whose ordering can vary by
 * runtime locale). "Cutoff" sorts the same everywhere this runs. */
function cmpFold(a: string, b: string): number {
  const x = a.toLowerCase();
  const y = b.toLowerCase();
  return x < y ? -1 : x > y ? 1 : 0;
}

/** Rows bucketed by `paramLabel` — "Cutoff" across three plugins sits
 *  together, which is the point of grouping by parameter. `rows` is
 *  assumed already sorted by `paramLabel` (as `buildMatrix` returns it),
 *  so groups come out in first-appearance order for free. */
export function matrixByParam(
  rows: readonly MatrixRow[],
): { label: string; rows: MatrixRow[] }[] {
  const order: string[] = [];
  const groups = new Map<string, MatrixRow[]>();
  for (const row of rows) {
    let bucket = groups.get(row.paramLabel);
    if (!bucket) {
      bucket = [];
      groups.set(row.paramLabel, bucket);
      order.push(row.paramLabel);
    }
    bucket.push(row);
  }
  return order.map((label) => ({ label, rows: groups.get(label) ?? [] }));
}
