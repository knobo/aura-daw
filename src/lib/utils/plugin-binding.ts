/** Resolve which midi tracks actually host a plugin instance.

A track binds an instance via `instrumentId = "plugin:<id>"`. The instance
row's own `trackId` is optional cache and is wiped on `plugin_list` refresh
(the engine always serializes it as absent), so the UI must read the tracks.
*/

export function pluginInstrumentRef(instanceId: string): string {
  return `plugin:${instanceId}`;
}

export function tracksBoundToInstance<
  T extends { kind: string; instrumentId?: string | null },
>(tracks: readonly T[], instanceId: string): T[] {
  const ref = pluginInstrumentRef(instanceId);
  return tracks.filter((t) => t.kind === "midi" && t.instrumentId === ref);
}

/** Stable caption for instance / params / patches: the bound track, not the
 * selected one. "connection: Midi 5" or "unbound". */
export function instanceConnectionLabel(
  tracks: readonly { kind: string; name: string; instrumentId?: string | null }[],
  instanceId: string,
): string {
  const names = tracksBoundToInstance(tracks, instanceId).map((t) => t.name);
  return names.length === 0 ? "unbound" : `connection: ${names.join(" · ")}`;
}
