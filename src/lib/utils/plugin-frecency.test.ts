import { describe, expect, it } from "vitest";
import { bumpFrecency, frecencyScore, type FrecencyTable } from "./plugin-frecency";

const day = 86_400_000;
const now = 1_700_000_000_000;

describe("frecencyScore (winner spec §3.1)", () => {
  it("a frequently used unstarred plugin beats a starred one used once last month", () => {
    const daily = frecencyScore({ count: 5, lastUsedAt: now - 3_600_000 }, now, false);
    const starredOnce = frecencyScore({ count: 1, lastUsedAt: now - 40 * day }, now, true);
    expect(daily).toBeGreaterThan(starredOnce);
  });

  it("a favourite you never use still beats a stranger", () => {
    const fav = frecencyScore(undefined, now, true);
    const stranger = frecencyScore(undefined, now, false);
    expect(fav).toBeGreaterThan(stranger);
  });

  it("recency decays: today > this week > this month > older", () => {
    const hit = (age: number) =>
      frecencyScore({ count: 1, lastUsedAt: now - age }, now, false);
    expect(hit(hour())).toBeGreaterThan(hit(3 * day));
    expect(hit(3 * day)).toBeGreaterThan(hit(14 * day));
    expect(hit(14 * day)).toBeGreaterThan(hit(40 * day));
  });
});

function hour() {
  return 3_600_000;
}

describe("bumpFrecency", () => {
  it("inserts a first hit at count 1", () => {
    const next = bumpFrecency({}, "u1", now);
    expect(next.u1).toEqual({ count: 1, lastUsedAt: now });
  });

  it("increments count and moves lastUsedAt", () => {
    const start: FrecencyTable = { u1: { count: 2, lastUsedAt: now - day } };
    const next = bumpFrecency(start, "u1", now);
    expect(next.u1.count).toBe(3);
    expect(next.u1.lastUsedAt).toBe(now);
    expect(start.u1.count).toBe(2);
  });
});
