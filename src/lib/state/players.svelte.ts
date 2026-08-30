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
import type { PlayerInfo, PlayerSource, PlayerTriggerMode } from "../types/ipc";

export type Player = PlayerInfo;
export type { PlayerSource, PlayerTriggerMode };

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
