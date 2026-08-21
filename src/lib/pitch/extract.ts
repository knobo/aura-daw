import type { ExtractMelodyReply, ExtractMelodyRequest } from "../types/ipc";

/** UI state for extracting melody from an audio clip into a MIDI clip. */
export type MelodyExtractionState =
  | { phase: "idle" }
  | { phase: "extracting" }
  | { phase: "done"; reply: ExtractMelodyReply }
  | { phase: "error"; message: string };

/**
 * Run the `pitch_extract_melody` command while keeping the caller/component
 * insulated from promise ordering and error normalization.
 */
export async function extractMelodyFromAudio(
  request: ExtractMelodyRequest,
  extract: (request: ExtractMelodyRequest) => Promise<ExtractMelodyReply>,
  update: (state: MelodyExtractionState) => void,
): Promise<ExtractMelodyReply | null> {
  update({ phase: "extracting" });
  try {
    const reply = await extract(request);
    update({ phase: "done", reply });
    return reply;
  } catch (error) {
    update({ phase: "error", message: String(error) });
    return null;
  }
}
