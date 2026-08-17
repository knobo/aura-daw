/**
 * Lane geometry for the arrangement timeline: the per-clip sample-space
 * boxes the marquee hit-tests against. Pure, so the marquee's maths test
 * without a DOM.
 *
 * The y -> lane-index half of this module MOVED to `lane-layout.ts` when
 * lanes stopped being uniform: a lane is now full-height, a collapsed
 * strip, or hidden inside a folded group, so a single height constant can
 * no longer answer "which lane is at this y". `TRACK_HEIGHT_PX` is
 * re-exported below for the callers that only need the default height.
 */
import type { LaneBox } from "./clip-selection";
import { LANE_HEIGHT_PX } from "./lane-layout";

/** A full-height lane in LAYOUT px. Re-export, not a second copy:
 * `lane-layout.ts` owns the value, mirrored by `--track-height` in
 * app.css. */
export const TRACK_HEIGHT_PX = LANE_HEIGHT_PX;

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
