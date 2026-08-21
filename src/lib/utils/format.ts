/** Time/level formatting helpers. */

/** samples → "MM:SS.mmm" */
export function formatClock(samples: number, sampleRate: number): string {
  const totalMs = Math.max(0, (samples / sampleRate) * 1000);
  const m = Math.floor(totalMs / 60000);
  const s = Math.floor((totalMs % 60000) / 1000);
  const ms = Math.floor(totalMs % 1000);
  return `${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}.${String(ms).padStart(3, "0")}`;
}

/** samples → "BBB.B.QQ" bars.beats.divisions (1-based).
 * `beatUnit` is the meter denominator (4 = quarter, 8 = eighth). */
export function formatBarsBeats(
  samples: number,
  sampleRate: number,
  tempoBpm: number,
  beatsPerBar = 4,
  beatUnit = 4,
): string {
  const den = beatUnit > 0 ? beatUnit : 4;
  const beatLen = (60 / tempoBpm) * sampleRate * (4 / den);
  const totalBeats = Math.max(0, samples / beatLen);
  const bar = Math.floor(totalBeats / beatsPerBar) + 1;
  const beat = Math.floor(totalBeats % beatsPerBar) + 1;
  const sixteenth = Math.floor((totalBeats % 1) * 4) + 1;
  return `${String(bar).padStart(3, "0")}.${beat}.${sixteenth}`;
}

/** linear (1.0 = FS) → dBFS, floored */
export function linToDb(x: number): number {
  if (x <= 0.00001) return -100;
  return 20 * Math.log10(x);
}

export function formatDb(db: number): string {
  if (db <= -90) return "-∞";
  return `${db > 0 ? "+" : ""}${db.toFixed(1)}`;
}

/** -1..+1 pan → "C" / "34L" / "34R". Extracted from `TrackHeader.svelte`'s
 * private `formatPan` (design §6.1) so the automation matrix can render the
 * exact same text for a track's pan curve. */
export function formatPan(value: number): string {
  if (Math.abs(value) < 0.005) return "C";
  return String(Math.round(Math.abs(value) * 100)) + (value < 0 ? "L" : "R");
}
