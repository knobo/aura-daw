/**
 * MIDI launch map: note names, binding lookup, region geometry, and the
 * echo/self-trigger guards. Pure so the panel, the timeline overlay and
 * the backend mapper can share one rule set.
 */

import type { LaunchBinding, LaunchMap, LaunchTarget } from "../types/ipc";

export type { LaunchBinding, LaunchMap, LaunchTarget };

export const DEFAULT_LAUNCH_MAP_ID = "default";

export function defaultLaunchMap(): LaunchMap {
  return {
    id: DEFAULT_LAUNCH_MAP_ID,
    name: "Launcher 1",
    bindings: [],
    driveClipIds: [],
    playMode: "gate",
  };
}

export function nextLauncherName(existing: { name: string }[]): string {
  let n = existing.length + 1;
  const used = new Set(existing.map((m) => m.name));
  while (used.has(`Launcher ${n}`)) n++;
  return `Launcher ${n}`;
}

export function driveMapId(maps: LaunchMap[], clipId: string): string | null {
  return maps.find((m) => m.driveClipIds.includes(clipId))?.id ?? null;
}

const NOTE_NAMES = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"] as const;

/** MIDI 60 = C4. */
export function midiNoteName(note: number): string {
  const n = Math.max(0, Math.min(127, Math.round(note)));
  const name = NOTE_NAMES[n % 12];
  const octave = Math.floor(n / 12) - 1;
  return `${name}${octave}`;
}

export function parseMidiNoteName(name: string): number | null {
  const m = name.trim().toUpperCase().match(/^([A-G])(#?)(-?\d+)$/);
  if (!m) return null;
  const letter = m[1] + m[2];
  const idx = NOTE_NAMES.indexOf(letter as (typeof NOTE_NAMES)[number]);
  if (idx < 0) return null;
  const octave = Number(m[3]);
  if (!Number.isInteger(octave)) return null;
  const note = (octave + 1) * 12 + idx;
  if (note < 0 || note > 127) return null;
  return note;
}

export function resolveLaunch(
  bindings: LaunchBinding[],
  note: number,
  channel: number,
): LaunchBinding | undefined {
  return bindings.find(
    (b) => b.note === note && (b.channel === null || b.channel === channel),
  );
}

/** True when launching this clip via `triggerNote` would re-fire itself
 * (the clip contains that note on a matching channel). */
export function clipWouldSelfTrigger(
  clipNotes: { key: number; channel?: number }[],
  triggerNote: number,
  triggerChannel: number | null,
): boolean {
  return clipNotes.some((n) => {
    if (n.key !== triggerNote) return false;
    if (triggerChannel === null) return true;
    return (n.channel ?? 0) === triggerChannel;
  });
}

export interface EchoNote {
  note: number;
  channel: number;
  atMs: number;
}

/** Hardware MIDI-in that just left AURA's own MIDI-out is an echo, not a
 * launch — this is the loopback half of "a clip must not start itself". */
export function incomingIsEcho(
  recentlySent: EchoNote[],
  incoming: EchoNote,
  windowMs: number,
): boolean {
  return recentlySent.some(
    (s) =>
      s.note === incoming.note &&
      s.channel === incoming.channel &&
      incoming.atMs >= s.atMs &&
      incoming.atMs - s.atMs <= windowMs,
  );
}

export interface MarqueeRegion {
  startTicks: number;
  lengthTicks: number;
  trackIds: string[];
}

export function regionFromMarquee(args: {
  startSamples: number;
  endSamples: number;
  laneLo: number;
  laneHi: number;
  tracks: { id: string }[];
  samplesToTicks: (samples: number) => number;
  snapTicks?: number;
}): MarqueeRegion | null {
  if (args.tracks.length === 0) return null;
  const lo = Math.min(args.startSamples, args.endSamples);
  const hi = Math.max(args.startSamples, args.endSamples);
  let startTicks = Math.max(0, Math.round(args.samplesToTicks(lo)));
  let endTicks = Math.max(0, Math.round(args.samplesToTicks(hi)));
  const snap = args.snapTicks && args.snapTicks > 0 ? args.snapTicks : 0;
  if (snap > 0) {
    startTicks = Math.round(startTicks / snap) * snap;
    endTicks = Math.round(endTicks / snap) * snap;
  }
  const lengthTicks = endTicks - startTicks;
  if (lengthTicks < 1) return null;
  const laneLo = Math.max(0, Math.min(args.laneLo, args.laneHi));
  const laneHi = Math.min(args.tracks.length - 1, Math.max(args.laneLo, args.laneHi));
  const trackIds = args.tracks.slice(laneLo, laneHi + 1).map((t) => t.id);
  if (trackIds.length === 0) return null;
  return { startTicks, lengthTicks, trackIds };
}

export function bindingFocusSamples(
  binding: LaunchBinding,
  clips: { id: string; timelineStartTicks: number }[],
  ticksToSamples: (ticks: number) => number,
): number | null {
  const target = binding.target;
  if (target.kind === "region") {
    return ticksToSamples(target.startTicks);
  }
  const clip = clips.find((c) => c.id === target.clipId);
  if (!clip) return null;
  return ticksToSamples(clip.timelineStartTicks);
}

export function nextBindingName(existing: LaunchBinding[]): string {
  let n = existing.length + 1;
  const used = new Set(existing.map((b) => b.name));
  while (used.has(`Scene ${n}`)) n++;
  return `Scene ${n}`;
}

/** First unused note starting at C3 (48), wrapping through the keyboard. */
export function nextFreeNote(existing: LaunchBinding[], start = 48): number {
  const used = new Set(existing.map((b) => b.note));
  for (let i = 0; i < 128; i++) {
    const n = (start + i) % 128;
    if (!used.has(n)) return n;
  }
  return start;
}

export interface OverlayBox {
  startSamples: number;
  endSamples: number;
  laneLo: number;
  laneHi: number;
}

/** Pixel-free geometry for a binding's timeline overlay. */
export function overlayBox(
  binding: LaunchBinding,
  tracks: { id: string }[],
  clips: { id: string; trackId: string; timelineStartTicks: number; lengthTicks: number }[],
  ticksToSamples: (ticks: number) => number,
): OverlayBox | null {
  const laneOf = (trackId: string) => tracks.findIndex((t) => t.id === trackId);
  const target = binding.target;
  if (target.kind === "region") {
    const lanes = target.trackIds
      .map(laneOf)
      .filter((i) => i >= 0)
      .sort((a, b) => a - b);
    if (lanes.length === 0) return null;
    return {
      startSamples: ticksToSamples(target.startTicks),
      endSamples: ticksToSamples(target.startTicks + target.lengthTicks),
      laneLo: lanes[0],
      laneHi: lanes[lanes.length - 1],
    };
  }
  const clip = clips.find((c) => c.id === target.clipId);
  if (!clip) return null;
  const lane = laneOf(clip.trackId);
  if (lane < 0) return null;
  return {
    startSamples: ticksToSamples(clip.timelineStartTicks),
    endSamples: ticksToSamples(clip.timelineStartTicks + clip.lengthTicks),
    laneLo: lane,
    laneHi: lane,
  };
}

export const MIDI_NOTES: { note: number; name: string }[] = Array.from(
  { length: 128 },
  (_, note) => ({ note, name: midiNoteName(note) }),
);

export type RegionTarget = Extract<LaunchTarget, { kind: "region" }>;

/** Shift a region in time and/or across lanes. Time is clamped at 0;
 * the lane set slides as a block and the shift is reduced so every
 * member stays on an existing track. */
export function shiftRegion(
  target: RegionTarget,
  deltaTicks: number,
  deltaLanes: number,
  tracks: { id: string }[],
): RegionTarget {
  const startTicks = Math.max(0, target.startTicks + deltaTicks);
  if (tracks.length === 0) {
    return { ...target, startTicks };
  }
  const index = new Map(tracks.map((t, i) => [t.id, i]));
  const lanes = target.trackIds.map((id) => index.get(id)).filter((i): i is number => i !== undefined);
  if (lanes.length === 0) {
    return { ...target, startTicks };
  }
  const lo = Math.min(...lanes);
  const hi = Math.max(...lanes);
  let shift = deltaLanes;
  if (lo + shift < 0) shift = -lo;
  if (hi + shift > tracks.length - 1) shift = tracks.length - 1 - hi;
  const trackIds = lanes
    .map((i) => tracks[i + shift]?.id)
    .filter((id): id is string => !!id);
  return { kind: "region", startTicks, lengthTicks: target.lengthTicks, trackIds };
}

export function resizeRegion(
  startTicks: number,
  lengthTicks: number,
  edge: "start" | "end",
  toTick: number,
): { startTicks: number; lengthTicks: number } {
  const end = startTicks + lengthTicks;
  if (edge === "start") {
    const start = Math.max(0, Math.min(toTick, end - 1));
    return { startTicks: start, lengthTicks: end - start };
  }
  return { startTicks, lengthTicks: Math.max(1, toTick - startTicks) };
}
