/**
 * Player registry mirror (Plan V — V2, Task 13). Thin renderer (ADR
 * 0006): invoke and hold, nothing more. No time math, no authoritative
 * state — the player's clock lives in `src-tauri/src/audio/clock.rs`; this
 * module only mirrors the document rows `players_get` returns and relays
 * the frozen `player_fire` / `player_stop` / `player_add` commands
 * (Task 9) to `backend`.
 *
 * A different seam from Task 12's `LaunchTarget` (`launch-map.ts`,
 * `launch.svelte.ts`): this is the surface's own `SurfaceTarget` variant
 * (`{ kind: "player" }` in `control-surface.ts`), consumed only by the
 * control-surface panel.
 */

import { backend } from "../tauri";
import type {
  PlayerInfo,
  PlayerQuantize,
  PlayerSource,
  PlayerTriggerMode,
} from "../types/ipc";

export type Player = PlayerInfo;
export type { PlayerQuantize, PlayerSource, PlayerTriggerMode };

export const players = $state<{ list: Player[] }>({ list: [] });

/** Best-effort, like `launch.players` refresh — stays a no-op in demo
 * mode, where `backend.playersGet` is not implemented. */
export async function refreshPlayers(): Promise<void> {
  if (!backend.playersGet) return;
  players.list = await backend.playersGet();
}

/** A dead player id (project switched, player removed) degrades to
 * `null` here exactly as `launch.clipIdForPlayer` degrades — never throws,
 * so a surface pad naming a player that is gone falls back to its own
 * stored label instead of crashing the deck. */
export function playerById(id: string): Player | null {
  return players.list.find((p) => p.id === id) ?? null;
}

export async function firePlayer(id: string): Promise<void> {
  await backend.playerFire?.(id);
}

/** Fire with the press's velocity (V3, V-18). A pointer has none, so the
 * pad keeps calling `firePlayer`; this is the seam a velocity-sensitive
 * controller uses, and the backend's own `player_fire` is this at 127. */
export async function firePlayerWithVelocity(id: string, velocity: number): Promise<void> {
  await backend.playerFireWithVelocity?.(id, velocity);
}

export async function stopPlayer(id: string): Promise<void> {
  await backend.playerStop?.(id);
}

/** Reachability for Gate/Loop (V-12): the pad itself is the only place
 * trigger mode is exposed, per the ledger's Task 11 ruling — there is no
 * separate pad-inspector panel in this codebase, and this task does not
 * invent one. */
export async function setTriggerMode(id: string, mode: PlayerTriggerMode): Promise<void> {
  await backend.playerSetTriggerMode?.(id, mode);
  await refreshPlayers();
}

/** The V3 trio, relayed on the same terms as `setTriggerMode`: invoke, then
 * re-read the registry, because the document is the authority on what the
 * pad now says (ADR 0006 — the renderer holds no state of its own). */
export async function setQuantize(id: string, quantize: PlayerQuantize): Promise<void> {
  await backend.playerSetQuantize?.(id, quantize);
  await refreshPlayers();
}

export async function setChokeGroup(id: string, group: number | null): Promise<void> {
  await backend.playerSetChokeGroup?.(id, group);
  await refreshPlayers();
}

export async function setVelocityToGain(id: string, depth: number): Promise<void> {
  await backend.playerSetVelocityToGain?.(id, depth);
  await refreshPlayers();
}

/**
 * Make a pad out of an audio clip. `raw` is V-6: the file at unity,
 * bypassing the source track entirely — the marker the owner reads off
 * the pad, not decoration.
 */
export async function addAudioPlayer(clipId: string, raw: boolean): Promise<string> {
  if (!backend.playerAdd) throw new Error("player_add is not available on this backend");
  const source: PlayerSource = { kind: "audioClip", clipId };
  const player = await backend.playerAdd(source, raw);
  await refreshPlayers();
  return player.id;
}
