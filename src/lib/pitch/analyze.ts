/** UI state for explicitly (re)building an audio clip's persisted APTF cache. */
export type PitchAnalysisState =
  | { phase: "idle" }
  | { phase: "analyzing" }
  | { phase: "done"; frames: number }
  | { phase: "error"; message: string };

/**
 * Run the existing `pitch_analyze_clip` command while keeping the component
 * ignorant of promise ordering and error normalization.
 */
export async function analyzePitchTrack(
  clipId: string,
  analyze: (clipId: string) => Promise<number>,
  update: (state: PitchAnalysisState) => void,
): Promise<void> {
  update({ phase: "analyzing" });
  try {
    const frames = await analyze(clipId);
    update({ phase: "done", frames });
  } catch (error) {
    update({ phase: "error", message: String(error) });
  }
}
