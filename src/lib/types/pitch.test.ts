/**
 * The pitch wire types are hand-written mirrors of the Rust serializers in
 * `src-tauri/src/audio/pitch.rs` (`#[serde(rename_all = "camelCase")]`), and
 * of `docs/ipc-schemas/pitch-frame.schema.json` / `pitch-state.schema.json`.
 * Nothing generates them, so this file is the guard that they keep the shape
 * the backend actually emits: it fails to COMPILE (svelte-check / vitest's
 * transform) the moment a field is renamed or dropped on either side.
 */
import { describe, it, expect } from "vitest";
import pitchFrameSchema from "../../../docs/ipc-schemas/pitch-frame.schema.json";
import pitchStateSchema from "../../../docs/ipc-schemas/pitch-state.schema.json";
import pitchScoreSchema from "../../../docs/ipc-schemas/pitch-score-report.schema.json";
import type { NoteScore, PitchFrame, PitchFrameBatch, PitchScoreReport, PitchState } from "./ipc";

describe("pitch wire types", () => {
  it("accepts a batch shaped exactly as the Rust serializer emits it", () => {
    const frame: PitchFrame = { sample: 480, hz: 220, midi: 57, clarity: 0.9, rms: 0.2, voiced: true };
    const batch: PitchFrameBatch = { frames: [frame], deviceRate: 48000, listening: true, rehearseHold: false };
    expect(batch.frames[0].midi).toBe(57);
    expect(batch.deviceRate).toBe(48000);
  });

  it("carries midi as a float, not a rounded note number", () => {
    // The R3 checkpoint found the note LABEL flips across a semitone
    // boundary while the pitch barely moves. The panel draws this float.
    const frame: PitchFrame = { sample: 0, hz: 811, midi: 79.6, clarity: 0.98, rms: 0.1, voiced: true };
    expect(frame.midi).toBeCloseTo(79.6, 5);
  });

  it("models pitch://state with a nullable reference track", () => {
    const none: PitchState = { listening: false, rehearseHold: false, referenceTrackId: null, deviceRate: 0 };
    const some: PitchState = { ...none, listening: true, referenceTrackId: "trk-1", deviceRate: 44100 };
    expect(none.referenceTrackId).toBeNull();
    expect(some.referenceTrackId).toBe("trk-1");
  });

  it("declares every field the published schemas require, and no others", () => {
    const batchProps = (pitchFrameSchema as { properties: Record<string, unknown> }).properties;
    const frameProps = (
      pitchFrameSchema as { $defs: { pitchFrame: { properties: Record<string, unknown> } } }
    ).$defs.pitchFrame.properties;
    const stateProps = (pitchStateSchema as { properties: Record<string, unknown> }).properties;

    // One sample value per type, keyed exactly as the TS interface is —
    // `satisfies` makes an extra or missing key a compile error, and the
    // key comparison below makes a schema drift a test failure.
    const batch = { frames: [], deviceRate: 0, listening: false, rehearseHold: false } satisfies PitchFrameBatch;
    const frame = { sample: 0, hz: 0, midi: 0, clarity: 0, rms: 0, voiced: false } satisfies PitchFrame;
    const state = {
      listening: false,
      rehearseHold: false,
      referenceTrackId: null,
      deviceRate: 0,
    } satisfies PitchState;

    expect(Object.keys(batch).sort()).toEqual(Object.keys(batchProps).sort());
    expect(Object.keys(frame).sort()).toEqual(Object.keys(frameProps).sort());
    expect(Object.keys(state).sort()).toEqual(Object.keys(stateProps).sort());
  });

  it("declares the score report exactly as the schema publishes it", () => {
    const reportProps = (pitchScoreSchema as { properties: Record<string, unknown> }).properties;
    const noteProps = (
      pitchScoreSchema as { $defs: { noteScore: { properties: Record<string, unknown> } } }
    ).$defs.noteScore.properties;

    const note = {
      noteId: 1,
      startSample: 0,
      endSample: 480,
      key: 60,
      hitFraction: 1,
      coverage: 1,
      meanCents: 0,
      medianCents: 0,
      onsetOffsetMs: 0,
      stabilityCents: 0,
      vibratoRateHz: 0,
      vibratoExtentCents: 0,
      ambiguous: false,
    } satisfies NoteScore;
    const report = {
      notes: [note],
      scoredNotes: 1,
      inTolerancePct: 0,
      meanAbsCents: 0,
      medianSignedCents: 0,
      meanOnsetOffsetMs: 0,
      rating: "Finding it",
      toleranceCents: 25,
      referenceTrackId: "trk-1",
    } satisfies PitchScoreReport;

    expect(Object.keys(report).sort()).toEqual(Object.keys(reportProps).sort());
    expect(Object.keys(note).sort()).toEqual(Object.keys(noteProps).sort());
  });

  it("keeps hitFraction and coverage as separate questions", () => {
    // "Was it in tune" and "was it sung at all" are different advice, and
    // the report must never be able to collapse them into one number.
    const skipped: NoteScore = {
      noteId: 1, startSample: 0, endSample: 480, key: 60,
      hitFraction: 0, coverage: 0, meanCents: 0, medianCents: 0, onsetOffsetMs: 0,
      stabilityCents: 0, vibratoRateHz: 0, vibratoExtentCents: 0, ambiguous: false,
    };
    const flat: NoteScore = { ...skipped, noteId: 2, coverage: 0.95, meanCents: -70 };
    expect(skipped.hitFraction).toBe(flat.hitFraction);
    expect(skipped.coverage).not.toBe(flat.coverage);
  });
});
