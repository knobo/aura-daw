/**
 * Lane geometry for the arrangement timeline: the lane height, y -> lane
 * index, and the per-clip sample-space boxes the marquee hit-tests against.
 * Pure, so the marquee's maths test without a DOM.
 */
import type { LaneBox } from "./clip-selection";

/** One lane's height in LAYOUT px. Mirrors `--track-height` in the app's
 * CSS and the grid canvas's own `trackH`; kept here so the marquee and the
 * grid cannot disagree. */
export const TRACK_HEIGHT_PX = 88;

/** Which lane a lane-area y coordinate falls in, clamped to the tracks that
 * exist (0 when there are none). */
export function laneIndexAt(y: number, trackCount: number): number {
  const raw = Math.floor(y / TRACK_HEIGHT_PX);
  return Math.max(0, Math.min(raw, Math.max(0, trackCount - 1)));
}

export interface BuildLaneBoxesArgs {
  /** Track ids in timeline order — the index into this array IS the lane. */
  trackIds: string[];
  audioClips: { id: string; trackId: string; timelineStartSamples: number; lengthSamples: number }[];
  midiClips: { id: string; trackId: string; timelineStartTicks: number; lengthTicks: number }[];
  /** The shipped section-table bijection (midi.ticksToSamples). */
  ticksToSamples: (ticks: number) => number;
}

/** One box per clip whose track is on the timeline. A MIDI clip's END is the
 * conversion of `start + length` ticks, NOT the start plus a converted
 * length — the latter drifts under a non-constant tempo map (the same bug
 * App.svelte's edge-jump already fixed). */
export function buildLaneBoxes(args: BuildLaneBoxesArgs): LaneBox[] {
  const lane = new Map(args.trackIds.map((id, i) => [id, i]));
  const boxes: LaneBox[] = [];
  for (const c of args.audioClips) {
    const i = lane.get(c.trackId);
    if (i === undefined) continue;
    boxes.push({
      ref: { kind: "audio", id: c.id },
      laneIndex: i,
      startSamples: c.timelineStartSamples,
      endSamples: c.timelineStartSamples + c.lengthSamples,
    });
  }
  for (const c of args.midiClips) {
    const i = lane.get(c.trackId);
    if (i === undefined) continue;
    boxes.push({
      ref: { kind: "midi", id: c.id },
      laneIndex: i,
      startSamples: args.ticksToSamples(c.timelineStartTicks),
      endSamples: args.ticksToSamples(c.timelineStartTicks + c.lengthTicks),
    });
  }
  return boxes;
}
