/**
 * Pure lookup against the backend-shipped section table (round-2 §3.6):
 * the ONLY tick<->sample conversion left in the frontend. No tempo/bpm/
 * period knowledge lives here (ADR 0006, thin renderer) — this is
 * presentation-layer interpolation of already-derived data, not deriving
 * the bijection itself (that got deleted from midi.svelte.ts/demo.ts).
 */

export interface SectionRow {
  startTick: number;
  startSample: number;
  startBeat: number;
  startBar: number;
  /** Superticks per quarter note, constant across this section. */
  period: number;
}

const SUPERTICKS_PER_SECOND = 508_032_000;

function sectionAtTick(sections: SectionRow[], tick: number): SectionRow {
  let lo = 0,
    hi = sections.length - 1,
    idx = 0;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (sections[mid].startTick <= tick) {
      idx = mid;
      lo = mid + 1;
    } else {
      hi = mid - 1;
    }
  }
  return sections[idx];
}

function sectionAtSample(sections: SectionRow[], samples: number): SectionRow {
  let lo = 0,
    hi = sections.length - 1,
    idx = 0;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (sections[mid].startSample <= samples) {
      idx = mid;
      lo = mid + 1;
    } else {
      hi = mid - 1;
    }
  }
  return sections[idx];
}

/** Absolute tick -> absolute sample, via the shipped section table. */
export function sampleAtTick(
  sections: SectionRow[],
  sampleRate: number,
  ppq: number,
  tick: number,
): number {
  if (sections.length === 0) return 0;
  const s = sectionAtTick(sections, tick);
  const samplesPerTick = (s.period / SUPERTICKS_PER_SECOND) * sampleRate / ppq;
  return s.startSample + (tick - s.startTick) * samplesPerTick;
}

/** Absolute sample -> absolute tick, via the shipped section table. */
export function tickAtSample(
  sections: SectionRow[],
  sampleRate: number,
  ppq: number,
  samples: number,
): number {
  if (sections.length === 0) return 0;
  const s = sectionAtSample(sections, samples);
  const samplesPerTick = (s.period / SUPERTICKS_PER_SECOND) * sampleRate / ppq;
  if (samplesPerTick <= 0) return s.startTick;
  return s.startTick + (samples - s.startSample) / samplesPerTick;
}
