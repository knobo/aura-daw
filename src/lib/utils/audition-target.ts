/**
 * What would this browser row sound like?
 *
 * One tagged union, resolved from data alone — no I/O, no runes, no
 * backend. `state/audition.svelte.ts` plays what this returns.
 *
 * The rule these resolvers exist to enforce: **an audition is never a
 * project edit.** A row that can only be heard by instantiating a plugin
 * (`Op::PluginAdd`) or binding a track resolves to `silent` with a reason,
 * never to a target that would commit an op. Design §8.2, ruling R-3.
 */

import type { PluginInstanceInfo, TrackState } from "../types/ipc";

/** C3 — the pitch every pitched audition uses (design §8.2). */
export const AUDITION_KEY = 60;

export type AuditionTarget =
  /** An audio file, played through the preview stream (`library_audition`). */
  | { kind: "sample"; path: string }
  /** A loaded sampler instrument (`sampler_preview_note`). */
  | { kind: "instrument"; instrumentId: string; key: number }
  /** A live plugin instance on a midi track (`plugin_preview_note`). */
  | { kind: "pluginInstance"; instanceId: string; key: number }
  /** Nothing to play, and a sentence saying why. Never an error. */
  | { kind: "silent"; reason: string };

/** The `instrumentId` a midi track carries when a plugin is its instrument. */
function pluginInstrumentId(instanceId: string): string {
  return `plugin:${instanceId}`;
}

function isBoundToMidiTrack(instanceId: string, tracks: readonly TrackState[]): boolean {
  const want = pluginInstrumentId(instanceId);
  return tracks.some((t) => t.kind === "midi" && t.instrumentId === want);
}

/**
 * A specific live instance. Mirrors the backend's own precondition
 * (`midi_track_bound_to_plugin` in `src-tauri/src/plugins/mod.rs`): an
 * insert-only or unplaced instance has no note input, so asking would just
 * earn an error string.
 */
export function resolvePluginInstanceTarget(
  instanceId: string,
  tracks: readonly TrackState[],
): AuditionTarget {
  if (!isBoundToMidiTrack(instanceId, tracks)) {
    return { kind: "silent", reason: "this instance is not on a midi track" };
  }
  return { kind: "pluginInstance", instanceId, key: AUDITION_KEY };
}

/**
 * A catalog row. Borrows an existing active instance of the same plugin if
 * one is already live and track-bound; otherwise silent, because the only
 * way to reach a bare descriptor is to instantiate it, and that is an edit.
 */
export function resolveDescriptorTarget(
  uid: string,
  instances: readonly PluginInstanceInfo[],
  tracks: readonly TrackState[],
): AuditionTarget {
  const live = instances.find(
    (i) => i.uid === uid && i.status === "active" && isBoundToMidiTrack(i.id, tracks),
  );
  if (!live) {
    return { kind: "silent", reason: "no live instance of this plugin to audition" };
  }
  return { kind: "pluginInstance", instanceId: live.id, key: AUDITION_KEY };
}
