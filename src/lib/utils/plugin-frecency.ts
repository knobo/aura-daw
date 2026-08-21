/**
 * Raycast-style frecency for the plugin picker (winner spec §3.1).
 *
 * Machine-global view state, not the catalog and not the op log: how often
 * you reach for a plugin is a habit, not a document. A full or unreadable
 * store degrades to "no history" rather than throwing during render.
 */

export interface FrecencyHit {
  uid: string;
  count: number;
  lastUsedAt: number;
}

export type FrecencyTable = Record<string, { count: number; lastUsedAt: number }>;

const STORAGE_KEY = "aura.plugin.frecency";
const DAY = 86_400_000;

/** Favourite is a nudge, not a lock — a plugin you add every session
 * without starring it must be allowed to climb past a starred one you
 * used once in May. */
const FAVOURITE_BOOST = 200;

export function recencyBoost(lastUsedAt: number, now: number): number {
  const age = now - lastUsedAt;
  if (age < DAY) return 40;
  if (age < 7 * DAY) return 20;
  if (age < 30 * DAY) return 8;
  return 0;
}

export function frecencyScore(
  hit: { count: number; lastUsedAt: number } | undefined,
  now: number,
  favorite: boolean,
): number {
  const fav = favorite ? FAVOURITE_BOOST : 0;
  if (!hit) return fav;
  return fav + hit.count * 100 + recencyBoost(hit.lastUsedAt, now);
}

export function bumpFrecency(table: FrecencyTable, uid: string, now: number): FrecencyTable {
  const prev = table[uid];
  return { ...table, [uid]: { count: (prev?.count ?? 0) + 1, lastUsedAt: now } };
}

export function loadFrecency(): FrecencyTable {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object") return {};
    const out: FrecencyTable = {};
    for (const [uid, hit] of Object.entries(parsed as Record<string, unknown>)) {
      if (!hit || typeof hit !== "object") continue;
      const rec = hit as { count?: unknown; lastUsedAt?: unknown };
      if (typeof rec.count === "number" && typeof rec.lastUsedAt === "number") {
        out[uid] = { count: rec.count, lastUsedAt: rec.lastUsedAt };
      }
    }
    return out;
  } catch {
    return {};
  }
}

export function saveFrecency(table: FrecencyTable): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(table));
  } catch {
    /* quota / private mode — scoring still works this session */
  }
}
