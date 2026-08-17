/**
 * Demo engine — a fully synthetic backend used when the page runs outside
 * Tauri (plain browser `vite dev`). It fakes the real engine's observable
 * behavior: a sample-accurate transport clock, 60 Hz meter frames, AWTF
 * waveform tiles (bit-identical wire format), and sidecar jobs with staged
 * progress. The rest of the app cannot tell the difference.
 */

import {
  AWTF_BINS_PER_TILE,
  AWTF_MAGIC,
  type AudioDevice,
  type AuraEventMap,
  type AuraEventName,
  type ChordSpan,
  type CircleWedge,
  type Clip,
  type ComposerGenerateReply,
  type ComposerGenerateRequest,
  type ComposerSuggestion,
  type EvolveOptions,
  type ExportCapabilities,
  type ExportJobStatus,
  type ExportRequest,
  type ExportResult,
  type GeneratedClipInfo,
  type GenJobResult,
  type HarmonyDoc,
  type HarmonyView,
  type HumToSongRequest,
  type ImportClipRequest,
  type ImportSplitReply,
  type LoopJamStatus,
  type InstrumentInfo,
  type KeySpan,
  type McpPolicy,
  type McpStatus,
  type MeterFrame,
  type MidiClip,
  type MidiNote,
  type NoteClass,
  type NoteRole,
  type NoteScore,
  type OpenSidecarEvent,
  type PaletteView,
  type PendingConfirmation,
  type PluginDescriptor,
  type PluginInstanceInfo,
  type PluginListResult,
  type PluginParamChange,
  type PluginParamInfo,
  type PitchFrame,
  type PitchFrameBatch,
  type PitchPoint,
  type PitchScoreReport,
  type PitchTrackView,
  type Project,
  type ProjectSnapshot,
  type SidecarEvent,
  type SidecarJobStatus,
  type StemName,
  type StemSplitRequest,
  type TempoEvent,
  type TrackMeter,
  type TrackState,
  type TranscribeRequest,
  type TransportState,
  type UserThemeFile,
  type WaveformTileRequest,
  type ZynPatch,
} from "./types/ipc";
import type { Backend, Unsubscribe } from "./tauri";
import { sampleAtTick, tickAtSample, type SectionRow } from "./sectionTable";
import { byTickKey } from "./utils/note-ops";

const SR = 48000;
const TEMPO = 120;
const BAR = (60 / TEMPO) * 4 * SR; // samples per bar (4/4)
const SUPERTICKS_PER_SECOND = 508_032_000;

const uuid = () =>
  typeof crypto !== "undefined" && crypto.randomUUID
    ? crypto.randomUUID()
    : `${Date.now()}-${Math.random().toString(16).slice(2)}`;

function hashString(s: string): number {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return h >>> 0;
}

/** Deterministic 0..1 noise from integers. */
function noise(seed: number, n: number): number {
  let x = Math.imul(seed ^ n, 2654435761);
  x ^= x >>> 13;
  x = Math.imul(x, 1274126177);
  x ^= x >>> 16;
  return (x >>> 0) / 4294967296;
}

export type ClipCharacter = "drums" | "bass" | "vocals" | "pad" | "other";

/**
 * Amplitude envelope (0..1) of a synthetic clip at time t (seconds from the
 * clip's own start). This drives both waveform tiles and playback meters, so
 * what you hear-see in the meters matches the drawn waveform.
 */
function envelope(character: ClipCharacter, seed: number, t: number): number {
  const beat = 60 / TEMPO;
  switch (character) {
    case "drums": {
      const bt = t / beat; // beats
      const beatIdx = Math.floor(bt);
      const frac = bt - beatIdx;
      // kick on quarters
      let v = Math.exp(-frac * 9) * 0.95;
      // snare on 2 & 4
      if (beatIdx % 2 === 1) v = Math.max(v, Math.exp(-frac * 7) * 0.8);
      // 8th hats
      const eighth = (bt * 2) % 1;
      v = Math.max(v, Math.exp(-eighth * 14) * 0.35);
      return v * (0.92 + 0.08 * noise(seed, beatIdx));
    }
    case "bass": {
      const step = Math.floor(t / (beat / 2)); // 8ths
      const gate = noise(seed, step) > 0.2 ? 1 : 0;
      const level = 0.55 + 0.35 * noise(seed ^ 0x9e37, step);
      const frac = (t / (beat / 2)) % 1;
      const shape = Math.min(1, frac * 10) * Math.exp(-frac * 1.2);
      return gate * level * (0.65 + 0.35 * shape);
    }
    case "vocals": {
      // phrases ~2.4 s with gaps, vibrato ripple
      const phrase = Math.floor(t / 3.2);
      const pt = t - phrase * 3.2;
      const sing = noise(seed, phrase) > 0.25 && pt < 2.4;
      if (!sing) return 0.02;
      const env = Math.sin((Math.PI * pt) / 2.4) ** 0.6;
      const vib = 0.85 + 0.15 * Math.sin(t * 35 + noise(seed, phrase) * 7);
      const syll = 0.7 + 0.3 * Math.abs(Math.sin(t * 9 + phrase));
      return 0.85 * env * vib * syll;
    }
    case "pad": {
      const slow = 0.45 + 0.3 * Math.sin(t * 0.5 + seed % 7) + 0.12 * Math.sin(t * 1.7);
      return Math.max(0.12, Math.min(0.8, slow));
    }
    default: {
      const s = Math.floor(t * 8);
      return 0.25 + 0.45 * noise(seed, s) * (0.6 + 0.4 * Math.sin(t * 2.2));
    }
  }
}

// ── Demo MIDI content ───────────────────────────────────────────────────────

const PPQ = 960;
const BAR_TICKS = PPQ * 4;

/** A synthwave A-minor arpeggio: 16th-note cycle over Am–F–C–G, 4 bars. */
function demoArpNotes(): MidiNote[] {
  const chords = [
    [57, 60, 64, 69], // Am
    [53, 57, 60, 65], // F
    [48, 52, 55, 60], // C
    [55, 59, 62, 67], // G
  ];
  const step = PPQ / 4; // 16ths
  const notes: MidiNote[] = [];
  for (let bar = 0; bar < 4; bar++) {
    const chord = chords[bar % chords.length];
    for (let s = 0; s < 16; s++) {
      const idx = s % 8 < 4 ? s % 4 : 3 - (s % 4); // up-down
      const octave = s % 8 < 4 ? 0 : 12;
      notes.push({
        tick: bar * BAR_TICKS + s * step,
        lengthTicks: Math.round(step * 0.9),
        key: chord[idx] + octave,
        velocity: s % 4 === 0 ? 112 : 82 + ((s * 7) % 20),
        channel: 0,
      });
    }
  }
  return notes;
}

/** Held pad chords, 4 bars. */
function demoChordNotes(): MidiNote[] {
  const chords = [
    [57, 64, 69, 72],
    [53, 60, 65, 69],
    [48, 55, 60, 64],
    [55, 62, 67, 71],
  ];
  const notes: MidiNote[] = [];
  chords.forEach((chord, bar) => {
    for (const key of chord) {
      notes.push({
        tick: bar * BAR_TICKS,
        lengthTicks: BAR_TICKS - PPQ / 8,
        key,
        velocity: 74,
        channel: 0,
      });
    }
  });
  return notes;
}

const DEMO_INSTRUMENTS: InstrumentInfo[] = [
  {
    id: "d3m0-inst-neon-keys",
    name: "Neon Keys",
    sfzPath: "/home/demo/instruments/neon-keys/neon-keys.sfz",
    regionCount: 26,
    keyLow: 36,
    keyHigh: 96,
  },
  {
    id: "d3m0-inst-glass-bells",
    name: "Glass Bells",
    sfzPath: "/home/demo/instruments/glass-bells/glass-bells.sfz",
    regionCount: 13,
    keyLow: 48,
    keyHigh: 108,
  },
  {
    id: "d3m0-inst-tape-bass",
    name: "Tape Bass 77",
    sfzPath: "/home/demo/instruments/tape-bass-77/tape-bass-77.sfz",
    regionCount: 18,
    keyLow: 24,
    keyHigh: 72,
  },
];

// ── Demo plugins (phase 3) ──────────────────────────────────────────────────
//
// A plausible machine: ZynAddSubFX (the LV2 flagship the real backend hosts),
// a fake CLAP synth, and one effect so the "instruments only in v1" rejection
// path demos honestly.

const DEMO_PLUGINS: PluginDescriptor[] = [
  {
    uid: "lv2:http://zynaddsubfx.sourceforge.net",
    format: "lv2",
    name: "ZynAddSubFX",
    vendor: "ZynAddSubFX Team",
    version: "3.0.6",
    path: "/usr/lib/lv2/ZynAddSubFX.lv2",
    isInstrument: true,
    audioInputs: 0,
    audioOutputs: 2,
    hasNoteInput: true,
    categories: ["Instrument Plugin", "synthesizer"],
  },
  {
    uid: "clap:/usr/lib/clap/PolyCascade.clap#com.glasscoast.polycascade",
    format: "clap",
    name: "PolyCascade",
    vendor: "Glasscoast Audio",
    version: "1.4.2",
    path: "/usr/lib/clap/PolyCascade.clap",
    isInstrument: true,
    audioInputs: 0,
    audioOutputs: 2,
    hasNoteInput: true,
    categories: ["instrument", "synthesizer", "stereo"],
  },
  {
    uid: "clap:/usr/lib/clap/ZamComp.clap#com.zamaudio.ZamComp",
    format: "clap",
    name: "ZamComp",
    vendor: "Damien Zammit",
    version: "4.1",
    path: "/usr/lib/clap/ZamComp.clap",
    isInstrument: false,
    audioInputs: 2,
    audioOutputs: 2,
    hasNoteInput: false,
    categories: ["audio-effect", "compressor"],
  },
];

/** Fresh per-instance parameter list for a demo plugin uid. */
function demoPluginParams(uid: string): PluginParamInfo[] {
  const p = (
    id: number,
    name: string,
    min: number,
    max: number,
    def: number,
    steps = 0,
  ): PluginParamInfo => ({ id, name, min, max, default: def, value: def, steps });

  if (uid.includes("polycascade")) {
    return [
      p(0, "Master Volume", 0, 1, 0.78),
      p(1, "Cutoff", 40, 12000, 2600),
      p(2, "Resonance", 0, 1, 0.25),
      p(3, "Detune", -50, 50, 6),
      p(4, "Osc Wave", 0, 3, 0, 4),
      p(5, "Sub Osc", 0, 1, 1, 2),
      p(6, "Attack", 0.001, 2, 0.01),
      p(7, "Release", 0.02, 4, 0.4),
      p(8, "Glide", 0, 1, 0),
      p(9, "Drive", 0, 1, 0.15),
    ];
  }
  if (uid.includes("zynaddsubfx")) {
    // ZynAddSubFX-scale surface: master + 16 parts × 8 controls ≈ 134 params,
    // exercising the grouped/scrollable generic UI.
    const params: PluginParamInfo[] = [
      p(0, "Master / Volume", 0, 1, 0.72),
      p(1, "Master / Cutoff", 40, 12000, 3400),
      p(2, "Master / Detune", -50, 50, 0),
      p(3, "Master / Pan", -1, 1, 0),
      p(4, "Master / Key Shift", -36, 36, 0, 73),
      p(5, "Master / NRPN Enable", 0, 1, 1, 2),
    ];
    let id = 8;
    for (let part = 1; part <= 16; part++) {
      const g = `Part ${String(part).padStart(2, "0")}`;
      params.push(
        p(id++, `${g} / Enable`, 0, 1, part <= 2 ? 1 : 0, 2),
        p(id++, `${g} / Volume`, 0, 1, 0.6),
        p(id++, `${g} / Pan`, -1, 1, 0),
        p(id++, `${g} / Velocity Sense`, 0, 127, 64, 128),
        p(id++, `${g} / Min Key`, 0, 127, 0, 128),
        p(id++, `${g} / Max Key`, 0, 127, 127, 128),
        p(id++, `${g} / Portamento`, 0, 1, 0, 2),
        p(id++, `${g} / Kit Mode`, 0, 2, 0, 3),
      );
    }
    return params;
  }
  return [];
}

// ── Demo Zyn patch bank (wave 1.5) ──────────────────────────────────────────
//
// A believable slice of the real /usr/share/zynaddsubfx/banks layout: a few
// banks, dozens of entries each — enough rows to exercise the search +
// virtualized list without shipping 1318 strings.

const ZYN_BANK_SEED: [string, string[]][] = [
  ["Arpeggios", ["Arpeggio", "Sequence", "Echoed Arp", "Broken Chord"]],
  ["Bass", ["Analog Bass", "Wah Bass", "Sub Bass", "Metal Bass", "Fretless"]],
  ["Choir_and_Voice", ["Choir", "Voice", "Aahs", "Space Voice"]],
  ["Pads", ["Warm Pad", "Analog Pad", "Sweep Pad", "Dream of the Saw", "Glass Pad"]],
  ["Plucked", ["Plucked String", "Nylon", "Harp", "Koto", "Music Box"]],
  ["Synth_Leads", ["Analog Lead", "Soft Lead", "Screaming Lead", "Sync Lead", "Fat Lead"]],
  ["The_Mysterious_Bank", ["Signal", "Anomaly", "Transmission"]],
];

function demoZynPatches(): ZynPatch[] {
  const out: ZynPatch[] = [];
  for (const [bank, names] of ZYN_BANK_SEED) {
    let program = 1;
    for (const base of names) {
      const variants = 2 + (hashString(bank + base) % 5); // 2..6 per family
      for (let v = 1; v <= variants; v++) {
        const name = `${base} ${v}`;
        out.push({
          bank,
          name,
          program,
          path: `/usr/share/zynaddsubfx/banks/${bank}/${String(program).padStart(4, "0")}-${name.replace(/\s+/g, "_")}.xiz`,
        });
        program++;
      }
    }
  }
  return out;
}

/** What the WebAudio voices read from a plugin instance's live params. */
interface DemoVoiceShape {
  cutoffHz: number | null;
  detuneCents: number;
  wave: OscillatorType | null;
  volume: number;
}

// ── Demo song ───────────────────────────────────────────────────────────────

interface DemoTrackInit {
  name: string;
  color: string;
  character: ClipCharacter;
  kind?: "audio" | "midi";
  instrumentId?: string;
  clips: { name: string; startBar: number; lengthBars: number }[];
  midiClips?: { name: string; startBar: number; lengthBars: number; notes: MidiNote[] }[];
}

const DEMO_SONG: DemoTrackInit[] = [
  {
    name: "Vox Lead",
    color: "#52e5ff",
    character: "vocals",
    clips: [
      { name: "verse take 3", startBar: 4, lengthBars: 8 },
      { name: "chorus comp", startBar: 12, lengthBars: 8 },
    ],
  },
  {
    name: "Drum Machine",
    color: "#ffc857",
    character: "drums",
    clips: [{ name: "beat 909", startBar: 0, lengthBars: 16 }],
  },
  {
    name: "Synth Bass",
    color: "#ff4fd8",
    character: "bass",
    clips: [
      { name: "bassline a", startBar: 0, lengthBars: 8 },
      { name: "bassline b", startBar: 8, lengthBars: 8 },
    ],
  },
  {
    name: "Nebula Pad",
    color: "#9d7bff",
    character: "pad",
    clips: [{ name: "swell", startBar: 4, lengthBars: 12 }],
  },
  {
    name: "Hyper Keys",
    color: "#5cf2b8",
    character: "other",
    kind: "midi",
    instrumentId: "d3m0-inst-neon-keys",
    clips: [],
    midiClips: [
      { name: "arp seq", startBar: 0, lengthBars: 4, notes: demoArpNotes() },
      { name: "chorus chords", startBar: 8, lengthBars: 4, notes: demoChordNotes() },
    ],
  },
];

// ── Engine ──────────────────────────────────────────────────────────────────

interface DemoJob {
  status: SidecarJobStatus;
  onEvent: (e: SidecarEvent) => void;
  timer: ReturnType<typeof setInterval> | null;
  request: StemSplitRequest | TranscribeRequest | Record<string, unknown>;
}

export class DemoBackend implements Backend {
  readonly mode = "demo" as const;

  private tracks: TrackState[] = [];
  private clips: Clip[] = [];
  private characters = new Map<string, ClipCharacter>();
  private clipSeeds = new Map<string, number>();

  private tState: "stopped" | "playing" | "recording" = "stopped";
  private anchorSamples = 0; // position when the clock was (re)anchored
  private anchorWall = 0; // performance.now() at anchor
  private recordStart = 0;

  // Loop region (samples). Mirrors the engine's transport loop semantics:
  // active only when enabled && end > start; positions already past the end
  // free-run.
  private loopEnabled = false;
  private loopStart = 0;
  private loopEnd = 0;
  private loopTimer: ReturnType<typeof setTimeout> | null = null;
  private stopAtEnd = true;
  private endTimer: ReturnType<typeof setTimeout> | null = null;

  private meterSubs = new Set<(f: MeterFrame) => void>();
  private meterTimer: ReturnType<typeof setInterval> | null = null;

  // pitch coach (browser mock — see the "pitch coach" section below)
  private pitchSubs = new Set<(b: PitchFrameBatch) => void>();
  private pitchTimer: ReturnType<typeof setInterval> | null = null;
  private pitchListening = false;
  private pitchRehearseHold = false;
  private pitchReferenceTrackId: string | null = null;
  private pitchNextSample = 0;
  private meterSeq = 0;

  private jobs = new Map<string, DemoJob>();
  private listeners = new Map<string, Set<(payload: unknown) => void>>();

  // phase 2 — midi / instruments / mcp
  private ppq = PPQ;
  private tempoEvents: TempoEvent[] = [{ tick: 0, bpm: TEMPO }];
  private meterEvents = [{ tick: 0, num: 4, den: 4 }];
  private midiClips: MidiClip[] = [];
  private instruments: InstrumentInfo[] = DEMO_INSTRUMENTS.map((i) => ({ ...i }));
  private mcpPolicy: McpPolicy = { mode: "confirmDestructive", toolOverrides: {}, port: 41717 };
  private mcpPending = new Map<string, PendingConfirmation & { apply?: () => void }>();
  private mcpScriptStarted = false;
  private audioCtx: AudioContext | null = null;

  // phase 3 — plugins (mirrors the backend PluginRegistry semantics)
  private pluginScanned = false;
  private pluginDescriptors: PluginDescriptor[] = [];
  private pluginInstances = new Map<string, PluginInstanceInfo>();
  private pluginParams = new Map<string, PluginParamInfo[]>();
  /** Coalesced audio re-schedule after param edits (knob-drag safe). */
  private pluginResyncTimer: ReturnType<typeof setTimeout> | null = null;

  // wave 1.5 — export / loop-jam / zyn patches
  private exportJobs = new Map<string, ExportJobStatus>();
  private ljState: LoopJamStatus["state"] = "idle";
  private ljJobId: string | null = null;
  private ljTrackId: string | null = null;
  private ljGeneration = 0;
  private ljLastError: string | null = null;
  private ljTimers: ReturnType<typeof setTimeout>[] = [];
  private zynPatches: ZynPatch[] = demoZynPatches();
  private zynLoaded = new Map<string, ZynPatch>();

  // WebAudio playback graph (see "demo playback audio" section below)
  private master: GainNode | null = null;
  private trackChains = new Map<string, { gain: GainNode; pan: StereoPannerNode }>();
  private playNodes: AudioScheduledSourceNode[] = [];
  private clipBuffers = new Map<string, AudioBuffer>();

  constructor() {
    for (const t of DEMO_SONG) {
      const track = this.makeTrack({
        name: t.name,
        color: t.color,
        kind: t.kind ?? "audio",
        instrumentId: t.instrumentId ?? null,
      });
      this.tracks.push(track);
      for (const c of t.clips) {
        const clip = this.makeClip(track.id, c.name, c.startBar * BAR, c.lengthBars * BAR);
        this.characters.set(clip.id, t.character);
        this.clips.push(clip);
      }
      for (const m of t.midiClips ?? []) {
        this.midiClips.push({
          id: uuid(),
          trackId: track.id,
          name: m.name,
          timelineStartTicks: m.startBar * BAR_TICKS,
          lengthTicks: m.lengthBars * BAR_TICKS,
          notes: m.notes,
        });
      }
    }
    this.scheduleMcpScript();
  }

  // ── helpers ──

  private makeTrack(init: Partial<TrackState>): TrackState {
    return {
      id: uuid(),
      name: init.name ?? `Track ${this.tracks.length + 1}`,
      kind: init.kind ?? "audio",
      gainDb: init.gainDb ?? 0,
      pan: init.pan ?? 0,
      muted: init.muted ?? false,
      soloed: init.soloed ?? false,
      armed: init.armed ?? false,
      color: init.color ?? "#52e5ff",
      instrumentId: init.instrumentId ?? null,
    };
  }

  /** samples -> ticks via the shipped section table (round-2 §3.6). This
   * used to hardcode TEMPO (120bpm) regardless of `tempoEvents` — a real
   * bug (a `setTempoMap` call would desync it from `ticksToSamples`, which
   * DID read `tempoEvents`); fixed as part of consuming one shared
   * bijection instead of two independently-wrong ones. */
  private samplesToTicks(samples: number): number {
    return tickAtSample(this.sectionTable, SR, this.ppq, samples);
  }

  private makeClip(trackId: string, name: string, start: number, length: number): Clip {
    return {
      id: uuid(),
      trackId,
      name,
      sourcePath: `audio/${name.replace(/\s+/g, "-")}.wav`,
      sourceChannels: 2,
      sourceSampleRate: SR,
      sourceLengthSamples: length,
      timelineStartSamples: start,
      offsetSamples: 0,
      lengthSamples: length,
      gainDb: 0,
      fadeInSamples: 0,
      fadeOutSamples: 0,
    };
  }

  private seedOf(clipId: string): number {
    let s = this.clipSeeds.get(clipId);
    if (s === undefined) {
      s = hashString(clipId);
      this.clipSeeds.set(clipId, s);
    }
    return s;
  }

  private characterOf(clipId: string): ClipCharacter {
    return this.characters.get(clipId) ?? "other";
  }

  private now(): number {
    return performance.now();
  }

  private position(): number {
    if (this.tState === "stopped") return this.anchorSamples;
    const raw = this.anchorSamples + ((this.now() - this.anchorWall) / 1000) * SR;
    // Same wrap rule as the engine's transport::advance.
    if (
      this.loopEnabled &&
      this.loopEnd > this.loopStart &&
      this.anchorSamples < this.loopEnd &&
      raw >= this.loopEnd
    ) {
      return this.loopStart + ((raw - this.loopStart) % (this.loopEnd - this.loopStart));
    }
    return raw;
  }

  private snapshot(): TransportState {
    return {
      state: this.tState,
      positionSamples: Math.max(0, Math.round(this.position())),
      sampleRate: SR,
      tempoBpm: this.tempoEvents[0]?.bpm ?? TEMPO,
      loopEnabled: this.loopEnabled,
      loopStartSamples: Math.round(this.loopStart),
      loopEndSamples: Math.round(this.loopEnd),
      songEndSamples: this.songEnd(),
      stopAtEnd: this.stopAtEnd,
    };
  }

  /** Last audible sample of the demo material (0 = nothing to play). */
  private songEnd(): number {
    let end = 0;
    for (const c of this.clips) {
      end = Math.max(end, c.timelineStartSamples + c.lengthSamples);
    }
    return Math.round(end);
  }

  /**
   * (Re)arm the auto-stop timer — the demo mirror of the engine's boundary
   * crossing: stop when the playhead reaches the end of the material, unless
   * a loop owns it or the policy is off.
   */
  private armEndStop() {
    if (this.endTimer) {
      clearTimeout(this.endTimer);
      this.endTimer = null;
    }
    if (this.tState !== "playing" || !this.stopAtEnd) return;
    if (this.loopEnabled && this.loopEnd > this.loopStart) return;
    const end = this.songEnd();
    const pos = this.position();
    if (end <= 0 || pos >= end) return;
    this.endTimer = setTimeout(
      () => {
        if (this.tState !== "playing" || !this.stopAtEnd) return;
        void this.transportStop().then(() => {
          void this.transportSeek(end);
        });
      },
      Math.max(0, ((end - pos) / SR) * 1000),
    );
  }

  async transportSetStopAtEnd(enabled: boolean): Promise<TransportState> {
    this.stopAtEnd = enabled;
    this.armEndStop();
    this.emitTransport();
    return this.snapshot();
  }

  /**
   * (Re)arm the loop-wrap timer: when the rolling playhead reaches the loop
   * end, re-anchor at the loop start and re-schedule the WebAudio graph so
   * demo playback audibly wraps (not just the visual playhead).
   */
  private armLoopWrap() {
    if (this.loopTimer) {
      clearTimeout(this.loopTimer);
      this.loopTimer = null;
    }
    if (this.tState === "stopped" || !this.loopEnabled || this.loopEnd <= this.loopStart) return;
    const pos = this.position();
    if (pos >= this.loopEnd) return; // past the region: free-running
    const ms = ((this.loopEnd - pos) / SR) * 1000;
    this.loopTimer = setTimeout(() => {
      if (this.tState === "stopped" || !this.loopEnabled) return;
      this.anchorSamples = this.loopStart;
      this.anchorWall = this.now();
      this.resyncAudio();
      this.emitTransport();
      this.armLoopWrap();
      this.armEndStop();
    }, Math.max(0, ms));
  }

  private emit<K extends AuraEventName>(event: K, payload: AuraEventMap[K]) {
    const subs = this.listeners.get(event);
    if (subs) for (const cb of subs) cb(payload);
  }

  private emitTransport() {
    this.emit("transport://state", this.snapshot());
  }

  // ── transport ──

  async transportPlay(): Promise<TransportState> {
    if (this.tState === "stopped") {
      this.anchorWall = this.now();
      this.tState = "playing";
      // Called from the user's play gesture — the AudioContext may be
      // created/resumed here, so playback is actually audible.
      this.startAudio();
      this.armLoopWrap();
      this.armEndStop();
      this.emitTransport();
    }
    return this.snapshot();
  }

  async transportStop(): Promise<TransportState> {
    if (this.tState === "recording") await this.stopRecording();
    if (this.tState !== "stopped") {
      this.anchorSamples = this.position();
      this.tState = "stopped";
      this.stopAudioNodes();
      this.armLoopWrap(); // clears the wrap timer
      this.emitTransport();
    }
    return this.snapshot();
  }

  async transportSeek(positionSamples: number): Promise<TransportState> {
    this.anchorSamples = Math.max(0, positionSamples);
    this.anchorWall = this.now();
    this.resyncAudio();
    this.armLoopWrap();
    this.armEndStop();
    this.emitTransport();
    return this.snapshot();
  }

  async transportSetLoop(
    enabled: boolean,
    startSamples: number,
    endSamples: number,
  ): Promise<TransportState> {
    if (enabled && endSamples <= startSamples) {
      throw new Error(`loop region is empty (start ${startSamples} >= end ${endSamples})`);
    }
    this.loopEnabled = enabled;
    this.loopStart = Math.max(0, startSamples);
    this.loopEnd = Math.max(0, endSamples);
    this.armLoopWrap();
    this.armEndStop();
    this.emitTransport();
    return this.snapshot();
  }

  async getTransportState(): Promise<TransportState> {
    return this.snapshot();
  }

  // ── demo playback audio (WebAudio) ──
  //
  // The demo engine is only honest if pressing PLAY makes sound. Audio clips
  // are synthesized once into AudioBuffers from the SAME envelope() that
  // drives the meters and waveform tiles, so what you hear matches what you
  // see; midi clips schedule real oscillator voices from their note data.
  // Track gain/pan/mute/solo apply live through per-track gain+panner nodes.

  private ensureCtx(): AudioContext {
    const ctx = (this.audioCtx ??= new (window.AudioContext ??
      (window as unknown as { webkitAudioContext: typeof AudioContext }).webkitAudioContext)());
    if (ctx.state === "suspended") void ctx.resume();
    return ctx;
  }

  private masterBus(ctx: AudioContext): GainNode {
    if (!this.master) {
      this.master = ctx.createGain();
      this.master.gain.value = 0.8;
      this.master.connect(ctx.destination);
    }
    return this.master;
  }

  private trackChain(trackId: string): { gain: GainNode; pan: StereoPannerNode } | null {
    const ctx = this.audioCtx;
    if (!ctx || !this.track(trackId)) return null;
    let chain = this.trackChains.get(trackId);
    if (!chain) {
      const gain = ctx.createGain();
      const pan = ctx.createStereoPanner();
      gain.connect(pan);
      pan.connect(this.masterBus(ctx));
      chain = { gain, pan };
      this.trackChains.set(trackId, chain);
    }
    this.applyChainParams(trackId, chain);
    return chain;
  }

  private applyChainParams(trackId: string, chain: { gain: GainNode; pan: StereoPannerNode }) {
    const track = this.track(trackId);
    if (!track) return;
    const anySolo = this.tracks.some((t) => t.soloed);
    const audible = !track.muted && (!anySolo || track.soloed);
    const lin = track.gainDb <= -160 ? 0 : Math.pow(10, track.gainDb / 20);
    chain.gain.gain.value = audible ? lin : 0;
    chain.pan.pan.value = Math.max(-1, Math.min(1, track.pan));
  }

  /** Re-apply gain/pan/mute/solo to the live audio graph (knob-rate safe). */
  private refreshTrackAudio() {
    for (const [id, chain] of this.trackChains) this.applyChainParams(id, chain);
  }

  private dropTrackChain(trackId: string) {
    const chain = this.trackChains.get(trackId);
    if (chain) {
      try {
        chain.gain.disconnect();
        chain.pan.disconnect();
      } catch {
        /* already disconnected */
      }
      this.trackChains.delete(trackId);
    }
  }

  private startAudio() {
    const ctx = this.ensureCtx();
    this.stopAudioNodes();
    this.scheduleFrom(ctx, this.position());
  }

  private stopAudioNodes() {
    for (const n of this.playNodes) {
      try {
        n.stop();
      } catch {
        /* not started yet / already stopped */
      }
      try {
        n.disconnect();
      } catch {
        /* already disconnected */
      }
    }
    this.playNodes = [];
  }

  /** Content or timing changed while rolling: re-schedule from the playhead. */
  private resyncAudio() {
    if (!this.audioCtx || this.tState === "stopped") return;
    this.stopAudioNodes();
    this.scheduleFrom(this.audioCtx, this.position());
  }

  /** Constant-tempo segments derived from `tempoEvents` (round-2 §3.4):
   * the demo backend has no ramp-editing feature, so a segment per
   * TempoEvent is exact — no subdivision needed, unlike the real backend's
   * `midi::section_table::SectionTable::build`, which also handles ramps.
   * This IS the demo's simulation of what the real backend would ship; the
   * frontend proper (`midi.svelte.ts`) never re-derives this itself. */
  private get sectionTable(): SectionRow[] {
    const sections: SectionRow[] = [];
    let sample = 0;
    for (let i = 0; i < this.tempoEvents.length; i++) {
      const e = this.tempoEvents[i];
      if (i > 0) {
        const prev = this.tempoEvents[i - 1];
        const prevPeriod = (60 / prev.bpm) * SUPERTICKS_PER_SECOND;
        const samplesPerTick = (prevPeriod / SUPERTICKS_PER_SECOND) * (SR / this.ppq);
        sample += (e.tick - prev.tick) * samplesPerTick;
      }
      sections.push({
        startTick: e.tick,
        startSample: Math.round(sample),
        // Not consumed by sectionTable.ts's lookups; the demo doesn't
        // model a meter map (round-2's meter work is UI-deferred).
        startBeat: 0,
        startBar: 0,
        period: (60 / e.bpm) * SUPERTICKS_PER_SECOND,
      });
    }
    return sections;
  }

  private ticksToSamples(ticks: number): number {
    return sampleAtTick(this.sectionTable, SR, this.ppq, ticks);
  }

  private scheduleFrom(ctx: AudioContext, pos: number) {
    const t0 = ctx.currentTime + 0.05;

    // Audio clips → envelope-synthesized buffers.
    for (const clip of this.clips) {
      const end = clip.timelineStartSamples + clip.lengthSamples;
      if (end <= pos) continue;
      const chain = this.trackChain(clip.trackId);
      if (!chain) continue;
      const src = ctx.createBufferSource();
      src.buffer = this.bufferFor(ctx, clip);
      const clipGain = ctx.createGain();
      clipGain.gain.value = clip.gainDb <= -160 ? 0 : Math.pow(10, clip.gainDb / 20);
      src.connect(clipGain);
      clipGain.connect(chain.gain);
      const startsIn = (clip.timelineStartSamples - pos) / SR;
      if (startsIn >= 0) src.start(t0 + startsIn);
      else src.start(t0, -startsIn); // already inside the clip: offset seconds
      this.playNodes.push(src);
    }

    // MIDI clips → oscillator voices from the actual note data.
    for (const mc of this.midiClips) {
      const clipStart = this.ticksToSamples(mc.timelineStartTicks);
      const clipEnd = this.ticksToSamples(mc.timelineStartTicks + mc.lengthTicks);
      if (clipEnd <= pos) continue;
      const chain = this.trackChain(mc.trackId);
      if (!chain) continue;
      const instId = this.track(mc.trackId)?.instrumentId;
      const inst = instId ? this.instruments.find((i) => i.id === instId) : undefined;
      const bells = !!inst?.name.toLowerCase().includes("bell");
      // Plugin-bound track? "stub" instances honestly render silence; "active"
      // ones voice through the demo synth shaped by their live parameters.
      let shape: DemoVoiceShape | null = null;
      if (instId?.startsWith("plugin:")) {
        const pinst = this.pluginInstances.get(instId.slice("plugin:".length));
        if (!pinst || pinst.status !== "active") continue;
        shape = this.pluginVoiceShape(pinst.id);
      }
      // Content repeats every contentLengthTicks until the placement
      // (lengthTicks) ends — mirrors schedule.rs::clip_events()'s
      // repetition loop. Absent contentLengthTicks (repeats === 1) is
      // byte-identical to the pre-looping single pass.
      const contentTicks = mc.contentLengthTicks ?? mc.lengthTicks;
      const repeats = Math.max(1, Math.ceil(mc.lengthTicks / contentTicks));
      for (let rep = 0; rep < repeats; rep++) {
        const repOffsetTicks = rep * contentTicks;
        if (repOffsetTicks >= mc.lengthTicks) break;
        for (const n of mc.notes) {
          const tick = repOffsetTicks + n.tick;
          if (tick >= mc.lengthTicks) continue; // onset at/after placement end: dropped
          const ns = clipStart + this.ticksToSamples(tick);
          const ne = Math.min(ns + this.ticksToSamples(n.lengthTicks), clipEnd);
          if (ne <= pos) continue;
          const when = t0 + Math.max(0, ns - pos) / SR;
          const dur = Math.max(0.05, (ne - Math.max(ns, pos)) / SR);
          this.scheduleVoice(ctx, chain.gain, when, dur, n.key, n.velocity, bells, shape);
        }
      }
    }
  }

  /** One poly-synth voice: detuned saws (or sine for bells) + LP + envelope. */
  private scheduleVoice(
    ctx: AudioContext,
    dest: AudioNode,
    when: number,
    dur: number,
    key: number,
    velocity: number,
    bells: boolean,
    shape: DemoVoiceShape | null = null,
  ) {
    const freq = 440 * Math.pow(2, (key - 69) / 12);
    const vel = (Math.max(1, Math.min(127, velocity)) / 127) * (shape ? shape.volume : 1);
    const gain = ctx.createGain();
    gain.gain.setValueAtTime(0, when);
    gain.gain.linearRampToValueAtTime(0.13 * vel, when + 0.01);
    gain.gain.setTargetAtTime(0.045 * vel, when + 0.01, 0.28);
    gain.gain.setTargetAtTime(0, when + dur, 0.07); // release
    const filter = ctx.createBiquadFilter();
    filter.type = "lowpass";
    const openHz = shape?.cutoffHz ?? Math.min(12000, freq * (bells ? 8 : 4.5));
    filter.frequency.setValueAtTime(Math.max(60, openHz), when);
    filter.frequency.setTargetAtTime(
      Math.max(200, Math.min(openHz, Math.max(400, freq * 1.6))),
      when + 0.02,
      0.3,
    );
    filter.Q.value = 0.9;
    filter.connect(gain);
    gain.connect(dest);
    const stopAt = when + dur + 0.5;
    const wave: OscillatorType = shape?.wave ?? (bells ? "sine" : "sawtooth");
    const spread = shape ? Math.max(1, Math.abs(shape.detuneCents)) : 0;
    const detunes = shape ? [-spread, spread] : bells ? [0] : [-6, 5];
    for (const detune of detunes) {
      const osc = ctx.createOscillator();
      osc.type = wave;
      osc.frequency.value = freq;
      osc.detune.value = detune;
      osc.connect(filter);
      osc.start(when);
      osc.stop(stopAt);
      this.playNodes.push(osc);
    }
  }

  /** Read the audible controls out of a plugin instance's param mirror. */
  private pluginVoiceShape(instanceId: string): DemoVoiceShape {
    const params = this.pluginParams.get(instanceId) ?? [];
    const find = (frag: string) =>
      params.find((p) => p.name.toLowerCase().includes(frag));
    const cutoff = find("cutoff");
    const detune = find("detune");
    const volume = find("volume");
    const waveP = find("wave");
    const WAVES: OscillatorType[] = ["sawtooth", "square", "triangle", "sine"];
    return {
      cutoffHz: cutoff ? cutoff.value : null,
      detuneCents: detune ? detune.value : 6,
      wave: waveP ? (WAVES[Math.round(waveP.value)] ?? "sawtooth") : null,
      volume: volume ? volume.value / (volume.max || 1) : 1,
    };
  }

  /**
   * Synthesize a clip's audio from its meter/waveform envelope. Mono at a
   * reduced sample rate (plenty for demo timbres, 4x lighter on memory);
   * cached per clip id.
   */
  private bufferFor(ctx: AudioContext, clip: Clip): AudioBuffer {
    const cached = this.clipBuffers.get(clip.id);
    if (cached) return cached;
    const rate = 24000;
    const secs = Math.min(180, clip.lengthSamples / SR);
    const frames = Math.max(1, Math.ceil(secs * rate));
    const buffer = ctx.createBuffer(1, frames, rate);
    const out = buffer.getChannelData(0);
    const character = this.characterOf(clip.id);
    const seed = this.seedOf(clip.id);
    const beat = 60 / TEMPO;
    let rng = (seed >>> 0) || 0x9e3779b9;
    const white = () => {
      rng ^= (rng << 13) >>> 0;
      rng ^= rng >>> 17;
      rng ^= (rng << 5) >>> 0;
      rng >>>= 0;
      return (rng / 4294967296) * 2 - 1;
    };
    const off = clip.offsetSamples / SR;
    const TAU = 2 * Math.PI;
    for (let i = 0; i < frames; i++) {
      const t = off + i / rate;
      const env = envelope(character, seed, t);
      let s = 0;
      switch (character) {
        case "drums": {
          const bt = t / beat;
          const frac = bt - Math.floor(bt);
          const kick = Math.exp(-frac * 11);
          s =
            0.75 * kick * Math.sin(TAU * (50 + 45 * kick) * t) +
            0.3 * env * white();
          break;
        }
        case "bass": {
          const bar = Math.floor(t / (beat * 4));
          const root = [33, 29, 36, 31][bar % 4]; // A1 F1 C2 G1
          const f = 440 * Math.pow(2, (root - 69) / 12);
          s =
            env *
            (0.55 * Math.sin(TAU * f * t) +
              0.28 * Math.sin(TAU * 2 * f * t) +
              0.12 * Math.sin(TAU * 3 * f * t));
          break;
        }
        case "vocals": {
          const f = 329.63 * (1 + 0.006 * Math.sin(TAU * 5.3 * t));
          s =
            env *
            (0.55 * Math.sin(TAU * f * t) +
              0.26 * Math.sin(TAU * 2 * f * t) +
              0.12 * Math.sin(TAU * 3 * f * t));
          break;
        }
        case "pad": {
          s =
            env *
            0.28 *
            (Math.sin(TAU * 110 * t) +
              Math.sin(TAU * 164.81 * t + 0.5) +
              Math.sin(TAU * 220.6 * t) +
              0.6 * Math.sin(TAU * 329.63 * t));
          break;
        }
        default: {
          s =
            env *
            (0.5 * Math.sin(TAU * 523.25 * t) +
              0.22 * Math.sin(TAU * 659.25 * t) +
              0.12 * white());
        }
      }
      out[i] = Math.max(-1, Math.min(1, s * 0.85));
    }
    this.clipBuffers.set(clip.id, buffer);
    return buffer;
  }

  // ── devices ──

  async listInputDevices(): Promise<AudioDevice[]> {
    return [
      { id: "demo-in", name: "Demo Interface — In 1/2", isDefault: true, maxChannels: 2, defaultSampleRate: SR },
      { id: "demo-mic", name: "Built-in Microphone", isDefault: false, maxChannels: 1, defaultSampleRate: 44100 },
    ];
  }
  async listOutputDevices(): Promise<AudioDevice[]> {
    return [
      { id: "demo-out", name: "Demo Interface — Out 1/2", isDefault: true, maxChannels: 2, defaultSampleRate: SR },
      { id: "demo-spk", name: "Built-in Speakers", isDefault: false, maxChannels: 2, defaultSampleRate: 44100 },
    ];
  }
  async selectInputDevice(_id: string): Promise<void> {}
  async selectOutputDevice(_id: string): Promise<void> {}

  // ── recording ──

  async startRecording(trackIds: string[] | null = null): Promise<void> {
    const armed = trackIds ?? this.tracks.filter((t) => t.armed).map((t) => t.id);
    if (armed.length === 0) return;
    this.recordStart = Math.round(this.position());
    this.anchorSamples = this.recordStart;
    this.anchorWall = this.now();
    this.tState = "recording";
    this.startAudio(); // existing clips keep playing under the take
    this.emitTransport();
    this.emit("recording://state", {
      recording: true,
      trackIds: armed,
      startedAtSamples: this.recordStart,
      xruns: 0,
    });
  }

  async stopRecording(): Promise<Clip[]> {
    if (this.tState !== "recording") return [];
    const end = Math.round(this.position());
    const armed = this.tracks.filter((t) => t.armed);
    const clips: Clip[] = [];
    const length = Math.max(SR / 4, end - this.recordStart);
    for (const track of armed) {
      const clip = this.makeClip(track.id, `take ${new Date().toLocaleTimeString()}`, this.recordStart, length);
      this.characters.set(clip.id, "vocals");
      this.clips.push(clip);
      clips.push(clip);
    }
    this.anchorSamples = end;
    this.anchorWall = this.now();
    this.tState = "playing";
    this.resyncAudio(); // the new takes are audible immediately
    this.emitTransport();
    this.emit("recording://state", { recording: false, trackIds: armed.map((t) => t.id), clips });
    return clips;
  }

  // ── meters ──

  async subscribeMeters(onFrame: (frame: MeterFrame) => void): Promise<Unsubscribe> {
    this.meterSubs.add(onFrame);
    if (!this.meterTimer) {
      this.meterTimer = setInterval(() => this.tickMeters(), 1000 / 60);
    }
    return () => {
      this.meterSubs.delete(onFrame);
      if (this.meterSubs.size === 0 && this.meterTimer) {
        clearInterval(this.meterTimer);
        this.meterTimer = null;
      }
    };
  }

  private tickMeters() {
    const pos = this.position();
    const anySolo = this.tracks.some((t) => t.soloed);
    const wall = this.now() / 1000;
    let sumL = 0;
    let sumR = 0;
    const trackMeters: TrackMeter[] = this.tracks.map((track) => {
      let env = 0;
      if (this.tState === "recording" && track.armed) {
        // live input: speech-ish noise
        const n = noise(hashString(track.id), Math.floor(wall * 30));
        env = 0.25 + 0.55 * n * (0.5 + 0.5 * Math.sin(wall * 6.1));
      } else if (this.tState !== "stopped") {
        for (const clip of this.clips) {
          if (clip.trackId !== track.id) continue;
          if (pos < clip.timelineStartSamples || pos >= clip.timelineStartSamples + clip.lengthSamples) continue;
          const t = (pos - clip.timelineStartSamples + clip.offsetSamples) / SR;
          env = Math.max(env, envelope(this.characterOf(clip.id), this.seedOf(clip.id), t));
        }
        // midi clips: envelope from the actual note data (attack + decay)
        const posTicks = this.samplesToTicks(pos);
        for (const mc of this.midiClips) {
          if (mc.trackId !== track.id) continue;
          const local = posTicks - mc.timelineStartTicks;
          if (local < 0 || local >= mc.lengthTicks) continue;
          const contentLen = mc.contentLengthTicks ?? mc.lengthTicks;
          const localInContent = local % Math.max(1, contentLen);
          for (const n of mc.notes) {
            if (localInContent < n.tick || localInContent >= n.tick + n.lengthTicks) continue;
            const age = (localInContent - n.tick) / this.ppq; // beats since onset
            const e = (n.velocity / 127) * (0.35 + 0.65 * Math.exp(-age * 2.2));
            if (e > env) env = e;
          }
        }
      }
      const audible = !track.muted && (!anySolo || track.soloed);
      const gain = track.gainDb <= -160 ? 0 : Math.pow(10, track.gainDb / 20);
      const level = audible ? env * gain : 0;
      const jitterL = 0.92 + 0.08 * noise(hashString(track.id) ^ 1, Math.floor(wall * 60));
      const jitterR = 0.92 + 0.08 * noise(hashString(track.id) ^ 2, Math.floor(wall * 60));
      const panL = Math.min(1, 1 - track.pan);
      const panR = Math.min(1, 1 + track.pan);
      const peakL = level * jitterL * panL;
      const peakR = level * jitterR * panR;
      sumL += peakL * peakL;
      sumR += peakR * peakR;
      return {
        trackId: track.id,
        peakL,
        peakR,
        rmsL: peakL * 0.62,
        rmsR: peakR * 0.62,
        clipped: peakL >= 1 || peakR >= 1,
      };
    });
    const mL = Math.sqrt(sumL) * 0.7;
    const mR = Math.sqrt(sumR) * 0.7;
    const frame: MeterFrame = {
      seq: this.meterSeq++,
      positionSamples: Math.max(0, Math.round(pos)),
      tracks: trackMeters,
      master: {
        trackId: "master",
        peakL: mL,
        peakR: mR,
        rmsL: mL * 0.62,
        rmsR: mR * 0.62,
        clipped: mL >= 1 || mR >= 1,
      },
    };
    for (const cb of this.meterSubs) cb(frame);
  }

  // ── pitch coach ──
  //
  // The browser has no microphone here (all audio is Rust — ADR 0006), so
  // this synthesizes a singer: a slowly wandering pitch with vibrato and
  // breath gaps, timestamped like the real backend. It exists so the lane
  // can be developed under `vite dev`; it is NOT a detector, and nothing
  // about its accuracy says anything about the real one.

  async pitchListenStart(): Promise<void> {
    this.pitchListening = true;
    this.pitchNextSample = Math.max(0, Math.round(this.position()));
    if (!this.pitchTimer) this.pitchTimer = setInterval(() => this.tickPitch(), 1000 / 60);
    this.emitPitchState();
  }

  async pitchListenStop(): Promise<void> {
    this.pitchListening = false;
    if (this.pitchTimer) {
      clearInterval(this.pitchTimer);
      this.pitchTimer = null;
    }
    this.emitPitchState();
  }

  async subscribePitch(onBatch: (b: PitchFrameBatch) => void): Promise<Unsubscribe> {
    this.pitchSubs.add(onBatch);
    return () => {
      this.pitchSubs.delete(onBatch);
    };
  }

  async setRehearseHold(enabled: boolean): Promise<void> {
    this.pitchRehearseHold = enabled;
    this.emitPitchState();
  }

  async pitchSetReference(trackId: string | null): Promise<void> {
    this.pitchReferenceTrackId = trackId;
    this.emitPitchState();
  }

  /**
   * A plausible take report, so the report UI is developable under
   * `vite dev`. It is a MOCK: the numbers are a deterministic function of
   * the reference melody's own notes, not of anything anybody sang, and
   * they say nothing about the detector or the scorer. Deterministic on
   * purpose — a report that reshuffled on every refetch would make a real
   * sort bug invisible.
   */
  async pitchScore(
    clipId: string,
    referenceTrackId: string,
    toleranceCents: number,
  ): Promise<PitchScoreReport> {
    // Both halves are checked the way the real command checks them, so a
    // caller that passes a stale id fails here too rather than only in Tauri.
    if (!this.clips.some((c) => c.id === clipId)) throw new Error(`unknown audio clip ${clipId}`);
    if (!this.tracks.some((t) => t.id === referenceTrackId)) {
      throw new Error(`unknown reference track ${referenceTrackId}`);
    }
    const notes: NoteScore[] = [];
    for (const mc of this.midiClips) {
      if (mc.trackId !== referenceTrackId) continue;
      const contentLen = mc.contentLengthTicks ?? mc.lengthTicks;
      // The demo's melody is whatever is drawn on the track; overlapping
      // notes are flagged the way the real chord resolver flags them.
      for (const n of mc.notes) {
        if (n.tick >= contentLen) continue;
        const startTicks = mc.timelineStartTicks + n.tick;
        const seed = hashString(`${mc.id}:${n.tick}:${n.key}`);
        const meanCents = (noise(seed, 1) - 0.5) * 2 * toleranceCents * 2.2;
        const inTune = Math.abs(meanCents) <= toleranceCents;
        const ambiguous = mc.notes.some(
          (o) => o !== n && o.tick === n.tick && o.key > n.key,
        );
        notes.push({
          noteId: n.noteId ?? seed >>> 8,
          startSample: Math.round(this.ticksToSamples(startTicks)),
          endSample: Math.round(this.ticksToSamples(startTicks + n.lengthTicks)),
          // Clamped like the backend's `clip_note_key_vel` and the lane's
          // `targetNotesFor`: in demo mode this IS the report, and an
          // unclamped key renders a note name that does not exist.
          key: Math.max(0, Math.min(127, n.key + (mc.transposeSemitones ?? 0))),
          hitFraction: inTune ? 0.72 + 0.28 * noise(seed, 2) : 0.15 * noise(seed, 3),
          coverage: 0.55 + 0.45 * noise(seed, 4),
          meanCents,
          medianCents: meanCents * 0.9,
          onsetOffsetMs: (noise(seed, 5) - 0.35) * 120,
          stabilityCents: 4 + 22 * noise(seed, 6),
          vibratoRateHz: noise(seed, 7) > 0.6 ? 4.5 + 1.5 * noise(seed, 8) : 0,
          vibratoExtentCents: noise(seed, 7) > 0.6 ? 30 + 40 * noise(seed, 9) : 0,
          ambiguous,
        });
      }
    }
    notes.sort((a, b) => a.startSample - b.startSample);
    if (notes.length === 0) {
      throw new Error(`track ${referenceTrackId} has no notes to score against`);
    }
    const scored = notes.filter((n) => !n.ambiguous);
    const weight = scored.reduce((a, n) => a + (n.endSample - n.startSample), 0) || 1;
    // `hitFraction * coverage`, like the backend: `hitFraction` is measured
    // over what was voiced, so one blip per note must not claim the note's
    // whole duration.
    const pct =
      (scored.reduce((a, n) => a + n.hitFraction * n.coverage * (n.endSample - n.startSample), 0) /
        weight) *
      100;
    const signed = scored.map((n) => n.medianCents).sort((a, b) => a - b);
    return {
      notes,
      scoredNotes: scored.length,
      inTolerancePct: pct,
      meanAbsCents: scored.reduce((a, n) => a + Math.abs(n.meanCents), 0) / (scored.length || 1),
      medianSignedCents: signed.length ? signed[signed.length >> 1] : 0,
      meanOnsetOffsetMs: scored.reduce((a, n) => a + n.onsetOffsetMs, 0) / (scored.length || 1),
      // Mirrors the backend's five words; the demo is the one place the
      // frontend is allowed to invent them, because there is no backend.
      rating:
        pct >= 95 ? "Locked in" : pct >= 85 ? "Sharp" : pct >= 70 ? "Solid" : pct >= 45 ? "Getting there" : "Finding it",
      toleranceCents,
      referenceTrackId,
    };
  }

  async pitchTrack(clipId: string, maxPoints = 2000): Promise<PitchTrackView> {
    const clip = this.clips.find((c) => c.id === clipId);
    if (!clip) throw new Error(`unknown audio clip ${clipId}`);
    const hop = Math.round(SR / 100);
    const total = Math.max(1, Math.floor(clip.lengthSamples / hop));
    const stride = Math.max(1, Math.ceil(total / Math.max(1, maxPoints)));
    const points: PitchPoint[] = [];
    for (let i = 0; i < total; i += stride) {
      const sample = clip.timelineStartSamples + i * hop;
      const t = sample / SR;
      const midi = 57 + 2.5 * Math.sin(t * 0.7) + 0.35 * Math.sin(t * 5.5);
      const voiced = noise(0x51ce, Math.floor(t * 4)) > 0.18;
      points.push({ sample, midi: voiced ? midi : 0, clarity: voiced ? 0.93 : 0.1, voiced });
    }
    return { clipId, sourceRate: SR, hop, provenance: "causalMedian", totalPoints: total, points };
  }

  async pitchAnalyzeClip(clipId: string): Promise<number> {
    const clip = this.clips.find((c) => c.id === clipId);
    if (!clip) throw new Error(`unknown audio clip ${clipId}`);
    return Math.max(1, Math.floor(clip.lengthSamples / Math.round(SR / 100)));
  }

  private emitPitchState() {
    this.emit("pitch://state", {
      listening: this.pitchListening,
      rehearseHold: this.pitchRehearseHold,
      referenceTrackId: this.pitchReferenceTrackId,
      deviceRate: this.pitchListening ? SR : 0,
    });
  }

  private tickPitch() {
    const hop = Math.round(SR / 100); // 10 ms, the real frame rate
    // While rolling, the frames follow the transport; while stopped, they
    // keep advancing on their own so tuner mode still has something to
    // draw. Either way a wild jump re-anchors instead of emitting a burst.
    const end = this.tState === "stopped" ? this.pitchNextSample + hop * 6 : Math.round(this.position());
    if (end < this.pitchNextSample || end - this.pitchNextSample > SR) this.pitchNextSample = end;

    const frames: PitchFrame[] = [];
    for (; this.pitchNextSample <= end; this.pitchNextSample += hop) {
      const t = this.pitchNextSample / SR;
      // A2-ish drift with vibrato: the wobble the lane has to survive.
      const midi = 57 + 2.5 * Math.sin(t * 0.7) + 0.35 * Math.sin(t * 5.5);
      // Breaths: roughly one gap every few seconds, ~250 ms long.
      const voiced = noise(0x51ce, Math.floor(t * 4)) > 0.18;
      frames.push({
        sample: this.pitchNextSample,
        hz: voiced ? 440 * Math.pow(2, (midi - 69) / 12) : 0,
        midi: voiced ? midi : 0,
        clarity: voiced ? 0.93 : 0.1,
        rms: voiced ? 0.18 : 0.002,
        voiced,
      });
    }

    const batch: PitchFrameBatch = {
      frames,
      deviceRate: SR,
      listening: this.pitchListening,
      rehearseHold: this.pitchRehearseHold,
    };
    for (const cb of this.pitchSubs) cb(batch);
  }

  // ── waveform tiles (AWTF wire format) ──

  async getWaveformTile(req: WaveformTileRequest): Promise<ArrayBuffer> {
    const channels = 2;
    const bins = AWTF_BINS_PER_TILE;
    const binWidth = Math.pow(2, 8 + req.lod); // source samples per bin
    const headerBytes = 4 + 2 + 2 + 4 + 8 + 4;
    const buf = new ArrayBuffer(headerBytes + channels * bins * 4);
    const view = new DataView(buf);
    view.setUint32(0, AWTF_MAGIC, true);
    view.setUint16(4, 1, true);
    view.setUint16(6, channels, true);
    view.setUint32(8, req.lod, true);
    view.setBigUint64(12, BigInt(req.tileIndex), true);
    view.setUint32(20, bins, true);

    const character = this.characterOf(req.clipId);
    const seed = this.seedOf(req.clipId);
    const i16 = new Int16Array(buf, headerBytes);
    const firstBin = req.tileIndex * bins;
    for (let ch = 0; ch < channels; ch++) {
      const chSeed = seed ^ (ch * 0x5bd1);
      for (let b = 0; b < bins; b++) {
        const t = ((firstBin + b) * binWidth) / SR;
        // sample the envelope a few times across the bin, keep extremes
        let hi = 0;
        for (let s = 0; s < 3; s++) {
          const e = envelope(character, seed, t + (s * binWidth) / (3 * SR));
          if (e > hi) hi = e;
        }
        const rough = 0.75 + 0.25 * noise(chSeed, firstBin + b);
        const max = Math.min(1, hi * rough);
        const min = -Math.min(1, hi * (0.7 + 0.3 * noise(chSeed ^ 0xa5a5, firstBin + b)));
        i16[ch * bins * 2 + b * 2] = Math.round(min * 32767);
        i16[ch * bins * 2 + b * 2 + 1] = Math.round(max * 32767);
      }
    }
    // simulate a little IPC latency so loading states are honest
    await new Promise((r) => setTimeout(r, 2 + Math.random() * 8));
    return buf;
  }

  // ── tracks ──

  async addTrack(init: Partial<TrackState> = {}): Promise<TrackState> {
    const track = this.makeTrack(init);
    this.tracks.push(track);
    return track;
  }

  async removeTrack(trackId: string): Promise<void> {
    this.tracks = this.tracks.filter((t) => t.id !== trackId);
    this.clips = this.clips.filter((c) => c.trackId !== trackId);
    this.dropTrackChain(trackId);
    this.resyncAudio();
  }

  async removeClip(clipId: string): Promise<void> {
    const before = this.clips.length;
    this.clips = this.clips.filter((c) => c.id !== clipId);
    if (this.clips.length === before) throw new Error(`unknown clip: ${clipId}`);
    this.resyncAudio();
  }

  async getTracks(): Promise<TrackState[]> {
    return this.tracks.map((t) => ({ ...t }));
  }

  private track(id: string): TrackState | undefined {
    return this.tracks.find((t) => t.id === id);
  }

  async setTrackGain(trackId: string, gainDb: number) {
    const t = this.track(trackId);
    if (t) t.gainDb = gainDb;
    this.refreshTrackAudio();
  }
  async setTrackPan(trackId: string, pan: number) {
    const t = this.track(trackId);
    if (t) t.pan = pan;
    this.refreshTrackAudio();
  }
  async setTrackMute(trackId: string, muted: boolean) {
    const t = this.track(trackId);
    if (t) t.muted = muted;
    this.refreshTrackAudio();
  }
  async setTrackSolo(trackId: string, soloed: boolean) {
    const t = this.track(trackId);
    if (t) t.soloed = soloed;
    this.refreshTrackAudio();
  }
  async setTrackArm(trackId: string, armed: boolean) {
    const t = this.track(trackId);
    if (t) t.armed = armed;
  }

  /** Mirrors the Rust write side: trim, reject empty. The demo engine is
   * how the browser-only mode is developed, so a validation rule the real
   * backend enforces has to exist here too or the UI gets exercised
   * against a more permissive engine than it will ever ship against. */
  async setTrackName(trackId: string, name: string): Promise<TrackState> {
    const t = this.track(trackId);
    if (!t) throw new Error(`unknown track: ${trackId}`);
    const trimmed = name.trim();
    if (!trimmed) throw new Error("name: must not be empty");
    t.name = trimmed;
    return { ...t };
  }

  async setTrackGroup(trackId: string, group: string | null): Promise<TrackState> {
    const t = this.track(trackId);
    if (!t) throw new Error(`unknown track: ${trackId}`);
    t.group = group?.trim() ? group.trim() : null;
    return { ...t };
  }

  async arrangeLanes(
    lanes: { trackId: string; group: string | null }[],
  ): Promise<TrackState[]> {
    // Same permutation contract as `Op::TrackReorder` — reject rather than
    // silently drop rows, so a frontend bug fails loudly in demo mode too.
    const ids = new Set(this.tracks.map((t) => t.id));
    if (lanes.length !== ids.size) throw new Error("arrange_lanes: incomplete arrangement");
    const seen = new Set<string>();
    for (const l of lanes) {
      if (seen.has(l.trackId)) throw new Error(`arrange_lanes: duplicate ${l.trackId}`);
      if (!ids.has(l.trackId)) throw new Error(`arrange_lanes: unknown track ${l.trackId}`);
      seen.add(l.trackId);
    }
    const byId = new Map(this.tracks.map((t) => [t.id, t]));
    this.tracks = lanes.map((l) => {
      const t = byId.get(l.trackId)!;
      t.group = l.group?.trim() ? l.group.trim() : null;
      return t;
    });
    this.refreshTrackAudio();
    return this.tracks.map((t) => ({ ...t }));
  }

  // ── project ──

  private projectSnapshot(): Project {
    return {
      schemaVersion: 1,
      name: "Neon District",
      sampleRate: SR,
      tempoBpm: this.tempoEvents[0]?.bpm ?? TEMPO,
      timeSignature: [this.meterEvents[0]?.num ?? 4, this.meterEvents[0]?.den ?? 4],
      tracks: this.tracks.map((t) => ({ ...t })),
      clips: this.clips.map((c) => ({ ...c })),
      transport: this.snapshot(),
    };
  }

  async createProject(name: string, _parentDir: string | null = null): Promise<Project> {
    return { ...this.projectSnapshot(), name };
  }
  async openProject(_path: string): Promise<Project> {
    return this.projectSnapshot();
  }
  async saveProject(): Promise<void> {}
  async saveProjectAs(name: string, _parentDir: string): Promise<Project> {
    // Demo mode has no filesystem; the UI guards on `mode` before calling.
    return { ...this.projectSnapshot(), name };
  }
  async getProject(): Promise<Project> {
    return this.projectSnapshot();
  }

  // ── sidecars ──

  sidecarSplitStems(request: StemSplitRequest, onEvent: (e: SidecarEvent) => void) {
    return this.runFakeJob("stemSplit", request, onEvent);
  }
  sidecarTranscribe(request: TranscribeRequest, onEvent: (e: SidecarEvent) => void) {
    return this.runFakeJob("transcribe", request, onEvent);
  }

  private async runFakeJob(
    kind: "stemSplit" | "transcribe",
    request: StemSplitRequest | TranscribeRequest,
    onEvent: (e: SidecarEvent) => void,
  ): Promise<string> {
    const jobId = uuid();
    const stages =
      kind === "stemSplit"
        ? [
            { until: 0.12, stage: "loading model" },
            { until: 0.82, stage: "separating" },
            { until: 1.0, stage: "writing stems" },
          ]
        : [
            { until: 0.1, stage: "loading model" },
            { until: 0.95, stage: "transcribing" },
            { until: 1.0, stage: "writing transcript" },
          ];
    const job: DemoJob = {
      status: { jobId, kind, state: "queued", progress: 0, stage: "queued" },
      onEvent,
      timer: null,
      request,
    };
    this.jobs.set(jobId, job);

    const durationMs = 6500 + Math.random() * 1500;
    const started = this.now();
    job.status.state = "running";
    job.timer = setInterval(() => {
      const p = Math.min(1, (this.now() - started) / durationMs);
      const stage = stages.find((s) => p <= s.until)?.stage ?? stages[stages.length - 1].stage;
      job.status.progress = p;
      job.status.stage = stage;
      const ev: SidecarEvent = { type: "progress", jobId, progress: p, stage };
      onEvent(ev);
      this.emit("sidecar://progress", ev);
      if (p >= 1) {
        if (job.timer) clearInterval(job.timer);
        job.timer = null;
        job.status.state = "done";
        const result =
          kind === "stemSplit"
            ? {
                kind: "stemSplit" as const,
                stems: {
                  vocals: "stems/demo/vocals.wav",
                  drums: "stems/demo/drums.wav",
                  bass: "stems/demo/bass.wav",
                  other: "stems/demo/other.wav",
                } as Partial<Record<StemName, string>>,
                elapsedSeconds: durationMs / 1000,
              }
            : {
                kind: "transcribe" as const,
                language: "en",
                segments: [
                  { startSeconds: 0, endSeconds: 2.4, text: "neon rain on empty streets" },
                  { startSeconds: 3.1, endSeconds: 5.8, text: "i hear the city breathing" },
                ],
              };
        job.status.result = result;
        const done: SidecarEvent = { type: "done", jobId, result };
        onEvent(done);
        this.emit("sidecar://done", done);
      }
    }, 100);
    return jobId;
  }

  async sidecarJobStatus(jobId: string): Promise<SidecarJobStatus> {
    const job = this.jobs.get(jobId);
    if (!job) throw new Error(`unknown job ${jobId}`);
    return { ...job.status };
  }

  async sidecarCancelJob(jobId: string): Promise<void> {
    const job = this.jobs.get(jobId);
    if (!job || job.status.state !== "running") return;
    if (job.timer) clearInterval(job.timer);
    job.timer = null;
    job.status.state = "cancelled";
    const ev: SidecarEvent = { type: "error", jobId, message: "cancelled" };
    job.onEvent(ev);
    this.emit("sidecar://error", ev);
  }

  async sidecarListJobs(): Promise<SidecarJobStatus[]> {
    return [...this.jobs.values()].map((j) => ({ ...j.status }));
  }

  // ── phase 2: control plane ──

  /** Additive v3 fields shared by `getProjectState` and `setTempoMap`
   * (round-2 §3.6) — one derivation, not two independently-drifting ones. */
  private tempoMapAdditiveFields() {
    return {
      meterMap: this.meterEvents.map((m) => ({ ...m })),
      periodEvents: this.tempoEvents.map((e) => {
        const period = Math.round((60 / e.bpm) * SUPERTICKS_PER_SECOND);
        return { tick: e.tick, periodStart: period, periodEnd: period };
      }),
      sectionTable: this.sectionTable,
      sectionTableRuleVersion: 1,
    };
  }

  async getProjectState(): Promise<ProjectSnapshot> {
    return {
      projectName: "Neon District",
      projectDir: "/home/demo/Neon District.aura",
      transport: this.snapshot(),
      tracks: this.tracks.map((t) => ({ ...t })),
      clips: this.clips.map((c) => ({ ...c })),
      midiClips: this.midiClips.map((c) => ({ ...c, notes: [...c.notes] })),
      ppq: this.ppq,
      tempoEvents: [...this.tempoEvents],
      harmony: this.harmony,
      ...this.tempoMapAdditiveFields(),
    };
  }

  async importAudioClip(request: ImportClipRequest): Promise<Clip> {
    let trackId = request.trackId ?? null;
    if (!trackId) {
      const base = request.path.split("/").pop()?.replace(/\.\w+$/, "") ?? "import";
      const track = this.makeTrack({ name: base, color: "#ff8b5c" });
      this.tracks.push(track);
      trackId = track.id;
    }
    const clip = this.makeClip(
      trackId,
      request.path.split("/").pop() ?? "import",
      request.atSamples ?? 0,
      8 * BAR,
    );
    this.characters.set(clip.id, "other");
    this.clips.push(clip);
    this.resyncAudio();
    return clip;
  }

  // ── phase 2: midi ──

  async setTempoMap(ppq: number | null, events: TempoEvent[], meter?: { tick: number; num: number; den: number }[] | null) {
    if (events.length === 0 || events[0].tick !== 0) {
      throw new Error("tempo map must start at tick 0");
    }
    if (ppq) this.ppq = ppq;
    this.tempoEvents = events.map((e) => ({ ...e }));
    if (meter?.length) this.meterEvents = meter.map((m) => ({ ...m }));
    this.resyncAudio();
    return { ppq: this.ppq, events: [...this.tempoEvents], ...this.tempoMapAdditiveFields() };
  }

  async midiAddClip(
    trackId: string,
    name: string | null,
    timelineStartTicks: number,
    lengthTicks: number,
  ): Promise<MidiClip> {
    const track = this.track(trackId);
    if (!track) throw new Error(`unknown track: ${trackId}`);
    if (track.kind !== "midi") throw new Error(`track ${trackId} is not a midi track`);
    const clip: MidiClip = {
      id: uuid(),
      trackId,
      name: name ?? `MIDI Clip ${this.midiClips.length + 1}`,
      timelineStartTicks,
      lengthTicks,
      notes: [],
    };
    this.midiClips.push(clip);
    this.resyncAudio();
    return { ...clip };
  }

  async midiSetNotes(clipId: string, notes: MidiNote[]): Promise<MidiClip> {
    const clip = this.midiClips.find((c) => c.id === clipId);
    if (!clip) throw new Error(`unknown MIDI clip: ${clipId}`);
    clip.notes = [...notes].sort(byTickKey);
    this.resyncAudio();
    return { ...clip, notes: [...clip.notes] };
  }

  async midiSetClipBounds(
    clipId: string,
    timelineStartTicks: number,
    lengthTicks: number,
    contentLengthTicks: number | null,
  ): Promise<MidiClip> {
    const clip = this.midiClips.find((c) => c.id === clipId);
    if (!clip) throw new Error(`unknown MIDI clip: ${clipId}`);
    clip.timelineStartTicks = timelineStartTicks;
    clip.lengthTicks = lengthTicks;
    clip.contentLengthTicks = contentLengthTicks ?? undefined;
    this.resyncAudio();
    return { ...clip };
  }

  async midiSetClipPlacement(
    clipId: string,
    transposeSemitones: number | null,
    velocityOffset: number | null,
  ): Promise<MidiClip> {
    const clip = this.midiClips.find((c) => c.id === clipId);
    if (!clip) throw new Error(`unknown MIDI clip: ${clipId}`);
    if (transposeSemitones !== null) clip.transposeSemitones = transposeSemitones;
    if (velocityOffset !== null) clip.velocityOffset = velocityOffset;
    this.resyncAudio();
    return { ...clip };
  }

  async midiRenameClip(clipId: string, name: string): Promise<MidiClip> {
    const clip = this.midiClips.find((c) => c.id === clipId);
    if (!clip) throw new Error(`unknown MIDI clip: ${clipId}`);
    const trimmed = name.trim();
    if (!trimmed) throw new Error("name: must not be empty");
    clip.name = trimmed;
    return { ...clip };
  }

  async midiRemoveClip(clipId: string): Promise<void> {
    const before = this.midiClips.length;
    this.midiClips = this.midiClips.filter((c) => c.id !== clipId);
    if (this.midiClips.length === before) throw new Error(`unknown MIDI clip: ${clipId}`);
    this.resyncAudio();
  }

  // ── the Composer (Plan H1, ruling H-12) ──────────────────────────────────
  //
  // A FIXTURE, not a second implementation. The theory lives in Rust
  // (`src-tauri/src/theory/`, ADR 0006) and duplicating any of it here would
  // be the exact mistake the thin-renderer rule exists to prevent — so what
  // follows is canned DATA for one key, plus the arithmetic needed to place
  // notes the fixture already names. In demo mode the key is always C major:
  // changing it stores the change and keeps the C-major circle, which is
  // honest about being a mock.

  private harmony: HarmonyDoc = {
    keys: [{ tick: 0, key: "C ionian" }],
    chords: [
      { tick: 0, lengthTicks: 3840, chord: "C" },
      { tick: 3840, lengthTicks: 3840, chord: "G" },
      { tick: 7680, lengthTicks: 3840, chord: "Am" },
      { tick: 11520, lengthTicks: 3840, chord: "F" },
    ],
  };

  /** The C-major circle, hard-coded: twelve slots clockwise from D♭. */
  private static readonly DEMO_WEDGES: CircleWedge[] = [
    { fifths: -5, major: "Db", minor: "Bb", inKey: false, borrowedChord: "Db", roman: "♭II", isTonic: false },
    { fifths: -4, major: "Ab", minor: "F", inKey: false, borrowedChord: "Ab", roman: "♭VI", isTonic: false },
    { fifths: -3, major: "Eb", minor: "C", inKey: false, borrowedChord: "Eb", roman: "♭III", isTonic: false },
    { fifths: -2, major: "Bb", minor: "G", inKey: false, borrowedChord: "Bb", roman: "♭VII", isTonic: false },
    { fifths: -1, major: "F", minor: "D", inKey: true, chord: "F", roman: "IV", function: "predominant", isTonic: false },
    { fifths: 0, major: "C", minor: "A", inKey: true, chord: "C", roman: "I", function: "tonic", isTonic: true },
    { fifths: 1, major: "G", minor: "E", inKey: true, chord: "G", roman: "V", function: "dominant", isTonic: false },
    { fifths: 2, major: "D", minor: "B", inKey: true, chord: "Dm", roman: "ii", function: "predominant", isTonic: false },
    { fifths: 3, major: "A", minor: "F#", inKey: true, chord: "Am", roman: "vi", function: "tonic", isTonic: false },
    { fifths: 4, major: "E", minor: "C#", inKey: true, chord: "Em", roman: "iii", function: "tonic", isTonic: false },
    { fifths: 5, major: "B", minor: "G#", inKey: true, chord: "Bdim", roman: "vii°", function: "dominant", isTonic: false },
    { fifths: 6, major: "F#", minor: "D#", inKey: false, borrowedChord: "F#", roman: "♯IV", isTonic: false },
  ];

  /** Chord tones as MIDI keys, named rather than derived (see above). */
  private static readonly DEMO_CHORD_KEYS: Record<string, number[]> = {
    C: [60, 64, 67], G: [59, 62, 67], Am: [60, 64, 69], F: [60, 65, 69],
    Dm: [62, 65, 69], Em: [59, 64, 67], Bdim: [59, 62, 65], G7: [59, 62, 65, 67],
    Db: [61, 65, 68], Ab: [60, 63, 68], Eb: [58, 63, 67], Bb: [58, 62, 65], "F#": [58, 61, 66],
  };

  private static readonly DEMO_ROMAN: Record<string, string> = {
    C: "I", G: "V", Am: "vi", F: "IV", Dm: "ii", Em: "iii", Bdim: "vii°", G7: "V7",
  };

  private harmonyView(): HarmonyView {
    return {
      harmony: { keys: [...this.harmony.keys], chords: [...this.harmony.chords] },
      key: this.harmony.keys[0]?.key ?? "C ionian",
      keyLabel: "C major",
      keySignature: 0,
      wedges: DemoBackend.DEMO_WEDGES.map((w: CircleWedge) => ({ ...w })),
      spans: this.harmony.chords.map((c) => ({
        tick: c.tick,
        lengthTicks: c.lengthTicks,
        symbol: c.chord,
        pretty: c.chord,
        roman: DemoBackend.DEMO_ROMAN[c.chord] ?? "?",
        function: DemoBackend.DEMO_WEDGES.find((w: CircleWedge) => w.chord === c.chord)?.function ?? "chromatic",
        borrowed: !DemoBackend.DEMO_ROMAN[c.chord],
        why: "Demo mode ships a fixture, not the theory engine — run the real app for the analysis.",
        tones: [],
      })),
      neighbours: [
        { key: "G ionian", label: "G major", steps: 1, why: "One step clockwise: the dominant key.", pivots: ["C", "Am"] },
        { key: "F ionian", label: "F major", steps: 1, why: "One step counter-clockwise: the subdominant key.", pivots: ["C", "Dm"] },
        { key: "A aeolian", label: "A minor", steps: 0, why: "The relative minor — the same seven notes.", pivots: ["C", "Am"] },
      ],
      borrowed: [
        { symbol: "Bb", pretty: "B♭", roman: "♭VII", why: "Borrowed from mixolydian, one step past IV." },
        { symbol: "Fm", pretty: "Fm", roman: "iv", why: "Borrowed from the parallel minor." },
      ],
      schemas: [
        { id: "axis", label: "I–V–vi–IV (the axis)", numerals: "I V vi IV", minor: false, why: "The four chords most of popular music is built from." },
        { id: "doo-wop", label: "I–vi–IV–V (doo-wop)", numerals: "I vi IV V", minor: false, why: "The fifties changes — it ends on the dominant, so it loops." },
        { id: "12-bar-blues", label: "12-bar blues", numerals: "I7*4 IV7*2 I7*2 V7 IV7 I7 V7", minor: false, why: "Twelve bars, three chords, all dominant sevenths." },
      ],
      genres: ["rock", "funk", "hipHop", "house", "jazz", "latin", "ballad", "metal"],
      voicingStyles: ["close", "drop2", "shell", "spread"],
      bassStyles: ["root", "rootFifth", "walking", "pedal"],
      ppq: this.ppq,
      ticksPerBar: this.ppq * 4,
    };
  }

  async harmonyGet(): Promise<HarmonyView> {
    return this.harmonyView();
  }

  async harmonySet(keys: KeySpan[], chords: ChordSpan[]): Promise<HarmonyView> {
    this.harmony = { keys: [...keys], chords: [...chords] };
    return this.harmonyView();
  }

  async composerPalette(tick: number): Promise<PaletteView> {
    const span = this.harmony.chords.find((c) => tick >= c.tick && tick < c.tick + c.lengthTicks);
    const tones = span ? (DemoBackend.DEMO_CHORD_KEYS[span.chord] ?? []).map((k: number) => k % 12) : [];
    const scale = [0, 2, 4, 5, 7, 9, 11];
    const NAMES = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    const classes: NoteClass[] = Array.from({ length: 12 }, (_, pc) => {
      const chordTone = tones.includes(pc);
      const inScale = scale.includes(pc);
      // The fixture's one rule, and it is the real one: a scale note a
      // semitone above a chord tone is an avoid note.
      const above = tones.includes((pc + 11) % 12);
      // Role by POSITION in the fixture's stack (root, third, fifth), so the
      // tint's colours mean the same thing in demo mode as in the real app.
      const CHORD_ROLES: NoteRole[] = ["root", "third", "fifth", "seventh"];
      const role: NoteRole = chordTone
        ? (CHORD_ROLES[tones.indexOf(pc)] ?? "extension")
        : inScale
          ? above
            ? "avoid"
            : "extension"
          : "outside";
      return { pitchClass: pc, tpc: NAMES[pc], role, degree: "", why: null };
    });
    return {
      tick,
      key: "C ionian",
      keyLabel: "C major",
      chord: span?.chord ?? null,
      chordPretty: span?.chord ?? null,
      roman: span ? (DemoBackend.DEMO_ROMAN[span.chord] ?? null) : null,
      classes,
    };
  }

  async composerSuggest(): Promise<ComposerSuggestion[]> {
    return [
      { chord: "G7", roman: "V7", function: "dominant", score: 0.82, why: "The dominant: its tritone resolves inward to the tonic." },
      { chord: "F", roman: "IV", function: "predominant", score: 0.71, why: "Departure — it sets up the dominant." },
      { chord: "Am", roman: "vi", function: "tonic", score: 0.64, why: "The relative minor: home with the lights down." },
      { chord: "Dm", roman: "ii", function: "predominant", score: 0.6, why: "The other predominant; ii–V–I is the oldest sentence in tonal music." },
    ];
  }

  async composerGenerate(request: ComposerGenerateRequest): Promise<ComposerGenerateReply> {
    const bars = request.bars ?? 4;
    const at = request.atTicks ?? 0;
    const bar = this.ppq * 4;
    const chords = this.harmony.chords.slice(0, bars);
    const parts = request.parts?.length ? request.parts : ["chords", "bass", "drums"];
    const clips: GeneratedClipInfo[] = [];
    for (const part of parts) {
      const notes: MidiNote[] = [];
      chords.forEach((span, i) => {
        const keys = DemoBackend.DEMO_CHORD_KEYS[span.chord] ?? [60];
        if (part === "chords") {
          for (const key of keys) {
            notes.push({ tick: i * bar, lengthTicks: bar, key, velocity: 84, channel: 0, noteId: 0 });
          }
        } else if (part === "bass") {
          notes.push({ tick: i * bar, lengthTicks: bar, key: keys[0] - 24, velocity: 92, channel: 0, noteId: 0 });
        } else if (part === "melody") {
          for (let b = 0; b < 4; b++) {
            notes.push({
              tick: i * bar + b * (bar / 4),
              lengthTicks: bar / 4,
              key: keys[b % keys.length] + 12,
              velocity: b === 0 ? 96 : 76,
              channel: 0,
              noteId: 0,
            });
          }
        } else {
          for (let b = 0; b < 4; b++) {
            const t = i * bar + b * (bar / 4);
            notes.push({ tick: t, lengthTicks: 120, key: b % 2 === 0 ? 36 : 38, velocity: 100, channel: 9, noteId: 0 });
            notes.push({ tick: t, lengthTicks: 120, key: 42, velocity: 56, channel: 9, noteId: 0 });
          }
        }
      });
      const track = this.makeTrack({ name: `Composer ${part}`, kind: "midi", color: "#8b8bff" });
      this.tracks.push(track);
      const clip = await this.midiAddClip(track.id, `Composer ${part}`, at, bars * bar);
      await this.midiSetNotes(clip.id, notes);
      clips.push({
        part,
        clipId: clip.id,
        trackId: track.id,
        trackName: track.name,
        createdTrack: true,
        noteCount: notes.length,
        annotations: [
          {
            tick: 0,
            lengthTicks: bar,
            label: `${part} · demo fixture`,
            why: "Demo mode plays a fixture so the panel is not dead in the browser. The real generators — voice leading, motif development, Euclidean drums — run in the Rust backend.",
          },
        ],
      });
    }
    this.resyncAudio();
    return {
      clips,
      harmony: this.harmonyView(),
      progression: {
        key: "C ionian",
        keyLabel: "C major",
        why: "Demo fixture: I–V–vi–IV in C major.",
        slots: chords.map((c) => ({
          chord: c.chord,
          bars: 1,
          roman: DemoBackend.DEMO_ROMAN[c.chord] ?? "?",
          why: "Fixture — the real analysis is in the backend.",
        })),
      },
      seed: request.seed ?? 1,
      bars,
    };
  }

  async midiGetClips(): Promise<MidiClip[]> {
    return this.midiClips.map((c) => ({ ...c, notes: [...c.notes] }));
  }

  async midiImportFile(path: string): Promise<MidiClip[]> {
    // Fabricate one imported clip on a fresh midi track.
    const base = path.split("/").pop()?.replace(/\.\w+$/, "") ?? "imported";
    const track = this.makeTrack({ name: base, color: "#5cf2b8", kind: "midi" });
    this.tracks.push(track);
    const clip: MidiClip = {
      id: uuid(),
      trackId: track.id,
      name: base,
      timelineStartTicks: 0,
      lengthTicks: 4 * BAR_TICKS,
      notes: demoArpNotes(),
    };
    this.midiClips.push(clip);
    this.resyncAudio();
    return [{ ...clip }];
  }

  async midiExportFile(path: string): Promise<string> {
    if (this.midiClips.length === 0) throw new Error("no MIDI clips to export");
    return path;
  }

  // ── phase 2: sampler ──

  async samplerLoadInstrument(sfzPath: string, name: string | null = null): Promise<InstrumentInfo> {
    const base = name ?? sfzPath.split("/").pop()?.replace(/\.sfz$/, "") ?? "instrument";
    const info: InstrumentInfo = {
      id: uuid(),
      name: base,
      sfzPath,
      regionCount: 12 + (hashString(sfzPath) % 20),
      keyLow: 36,
      keyHigh: 96,
    };
    this.instruments.push(info);
    return { ...info };
  }

  async samplerListInstruments(): Promise<InstrumentInfo[]> {
    return this.instruments.map((i) => ({ ...i }));
  }

  /** Browser preview: a tiny hand-rolled subtractive synth via WebAudio. */
  async samplerPreviewNote(instrumentId: string, key: number, velocity: number): Promise<void> {
    const inst = this.instruments.find((i) => i.id === instrumentId);
    const ctx = this.ensureCtx();
    const now = ctx.currentTime;
    const freq = 440 * Math.pow(2, (key - 69) / 12);
    const vel = Math.max(1, Math.min(127, velocity)) / 127;
    const bells = inst?.name.toLowerCase().includes("bell");

    const gain = ctx.createGain();
    gain.gain.setValueAtTime(0, now);
    gain.gain.linearRampToValueAtTime(0.22 * vel, now + 0.008);
    gain.gain.exponentialRampToValueAtTime(0.0004, now + (bells ? 1.4 : 0.7));
    const filter = ctx.createBiquadFilter();
    filter.type = "lowpass";
    filter.frequency.setValueAtTime(Math.min(14000, freq * (bells ? 9 : 5)), now);
    filter.frequency.exponentialRampToValueAtTime(Math.max(300, freq * 1.4), now + 0.55);
    filter.Q.value = 1.1;
    gain.connect(ctx.destination);
    filter.connect(gain);

    for (const detune of bells ? [0] : [-7, 6]) {
      const osc = ctx.createOscillator();
      osc.type = bells ? "sine" : "sawtooth";
      osc.frequency.value = freq;
      osc.detune.value = detune;
      osc.connect(filter);
      osc.start(now);
      osc.stop(now + 1.6);
    }
    if (bells) {
      const partial = ctx.createOscillator();
      partial.type = "sine";
      partial.frequency.value = freq * 2.76; // inharmonic shimmer
      const pg = ctx.createGain();
      pg.gain.setValueAtTime(0.08 * vel, now);
      pg.gain.exponentialRampToValueAtTime(0.0003, now + 0.9);
      partial.connect(pg);
      pg.connect(ctx.destination);
      partial.start(now);
      partial.stop(now + 1.0);
    }
  }

  async setTrackInstrument(trackId: string, instrumentId: string | null): Promise<TrackState> {
    const track = this.track(trackId);
    if (!track) throw new Error(`unknown track: ${trackId}`);
    if (track.kind !== "midi") throw new Error(`track ${trackId} is not a midi track`);
    if (instrumentId?.startsWith("plugin:")) {
      const pid = instrumentId.slice("plugin:".length);
      const inst = this.pluginInstances.get(pid);
      if (!inst) throw new Error(`unknown plugin instance: ${pid} (plugin_instantiate first)`);
      inst.trackId = trackId;
    } else if (instrumentId && !this.instruments.some((i) => i.id === instrumentId)) {
      throw new Error(`unknown instrument: ${instrumentId} (load it first)`);
    }
    // A rebind away from a plugin clears that instance's track ref.
    for (const inst of this.pluginInstances.values()) {
      if (inst.trackId === trackId && instrumentId !== `plugin:${inst.id}`) inst.trackId = null;
    }
    track.instrumentId = instrumentId;
    this.resyncAudio();
    return { ...track };
  }

  // ── phase 3: plugins ──

  async pluginScan(): Promise<PluginDescriptor[]> {
    // Simulated dlopen/world walk latency so scan UI states are honest.
    await new Promise((r) => setTimeout(r, 650 + Math.random() * 350));
    this.pluginDescriptors = DEMO_PLUGINS.map((d) => ({ ...d }));
    this.pluginScanned = true;
    return this.pluginDescriptors.map((d) => ({ ...d }));
  }

  async pluginList(): Promise<PluginListResult> {
    return {
      plugins: this.pluginDescriptors.map((d) => ({ ...d })),
      instances: [...this.pluginInstances.values()].map((i) => ({ ...i })),
      scanned: this.pluginScanned,
    };
  }

  async pluginInstantiate(uid: string): Promise<PluginInstanceInfo> {
    // Mirrors the backend PluginRegistry error surface verbatim.
    if (!this.pluginScanned) throw new Error("no plugin scan yet — call plugin_scan first");
    const desc = this.pluginDescriptors.find((d) => d.uid === uid);
    if (!desc) throw new Error(`unknown plugin uid: ${uid}`);
    if (!desc.isInstrument) {
      throw new Error(`${desc.name} is an effect; v1 hosts instruments on midi tracks only`);
    }
    const info: PluginInstanceInfo = {
      id: uuid(),
      uid: desc.uid,
      name: desc.name,
      format: desc.format,
      status: "stub",
      trackId: null,
    };
    this.pluginInstances.set(info.id, info);
    this.pluginParams.set(info.id, demoPluginParams(desc.uid));
    // Demo "P1/P2 host": the DSP comes up shortly after registration.
    setTimeout(() => {
      const inst = this.pluginInstances.get(info.id);
      if (inst && inst.status === "stub") {
        inst.status = "active";
        this.resyncAudio(); // audible immediately if a track is bound
      }
    }, 1100);
    return { ...info };
  }

  async pluginRemove(instanceId: string): Promise<PluginInstanceInfo> {
    const inst = this.pluginInstances.get(instanceId);
    if (!inst) throw new Error(`unknown plugin instance: ${instanceId}`);
    this.pluginInstances.delete(instanceId);
    this.pluginParams.delete(instanceId);
    this.resyncAudio(); // bound tracks fall back to silence/PolySynth
    return { ...inst };
  }

  async pluginGetParams(instanceId: string): Promise<PluginParamInfo[]> {
    if (!this.pluginInstances.has(instanceId)) {
      throw new Error(`unknown plugin instance: ${instanceId}`);
    }
    return (this.pluginParams.get(instanceId) ?? []).map((p) => ({ ...p }));
  }

  async pluginSetParam(
    instanceId: string,
    changes: PluginParamChange[],
  ): Promise<PluginParamInfo[]> {
    if (!this.pluginInstances.has(instanceId)) {
      throw new Error(`unknown plugin instance: ${instanceId}`);
    }
    const params = this.pluginParams.get(instanceId) ?? [];
    for (const ch of changes) {
      const p = params.find((x) => x.id === ch.id);
      if (p) p.value = Math.min(p.max, Math.max(p.min, ch.value));
    }
    this.pluginParams.set(instanceId, params);
    // Audible follow-up, coalesced: re-schedule voices at most ~6×/s while
    // a knob is dragged (full-rate rescheduling would restart notes).
    if (!this.pluginResyncTimer) {
      this.pluginResyncTimer = setTimeout(() => {
        this.pluginResyncTimer = null;
        this.resyncAudio();
      }, 160);
    }
    return params.map((p) => ({ ...p }));
  }

  // ── phase 2: open-kind generation jobs ──

  async sidecarRunJob(
    kind: string,
    params: Record<string, unknown>,
    onEvent: (e: OpenSidecarEvent) => void,
  ): Promise<string> {
    const staged: Record<string, { until: number; stage: string }[]> = {
      aceStepGenerate: [
        { until: 0.08, stage: "loading ACE-Step" },
        { until: 0.2, stage: "encoding prompt" },
        { until: 0.86, stage: "diffusion" },
        { until: 1, stage: "decoding audio" },
      ],
      aceStepRepaint: [
        { until: 0.1, stage: "loading ACE-Step" },
        { until: 0.3, stage: "analyzing region" },
        { until: 0.9, stage: "repainting" },
        { until: 1, stage: "crossfading" },
      ],
      aceStepAudio2Audio: [
        { until: 0.1, stage: "loading ACE-Step" },
        { until: 0.32, stage: "encoding source" },
        { until: 0.9, stage: "restyling" },
        { until: 1, stage: "decoding audio" },
      ],
      elevenLabsMusic: [
        { until: 0.15, stage: "contacting api" },
        { until: 0.85, stage: "composing" },
        { until: 1, stage: "downloading" },
      ],
      amtInfill: [
        { until: 0.2, stage: "loading anticipation" },
        { until: 0.9, stage: "sampling tokens" },
        { until: 1, stage: "merging notes" },
      ],
      stableAudioSfz: [
        { until: 0.1, stage: "loading stable audio" },
        { until: 0.75, stage: "sampling keys" },
        { until: 0.92, stage: "trimming + loops" },
        { until: 1, stage: "writing sfz" },
      ],
    };
    const stages = staged[kind];
    if (!stages) throw new Error(`unknown job kind: ${kind}`);

    const jobId = uuid();
    const job: DemoJob = {
      status: { jobId, kind, state: "queued", progress: 0, stage: "queued" },
      onEvent: onEvent as (e: SidecarEvent) => void,
      timer: null,
      request: params,
    };
    this.jobs.set(jobId, job);

    const emitBoth = (e: OpenSidecarEvent) => {
      onEvent(e);
      if (e.type === "progress") this.emit("sidecar://progress", e);
      else if (e.type === "done")
        this.emit("sidecar://done", e as unknown as AuraEventMap["sidecar://done"]);
      else if (e.type === "error") this.emit("sidecar://error", e);
    };

    const logLines = [
      `worker: ${kind} starting (simulate=demo)`,
      `params: ${JSON.stringify(params).slice(0, 120)}`,
    ];
    setTimeout(() => {
      for (const line of logLines) emitBoth({ type: "log", jobId, line } as OpenSidecarEvent);
    }, 120);

    const durationMs = kind === "amtInfill" ? 4200 : 7000 + Math.random() * 2500;
    const started = this.now();
    job.status.state = "running";
    job.timer = setInterval(() => {
      const p = Math.min(1, (this.now() - started) / durationMs);
      const stage = stages.find((s) => p <= s.until)?.stage ?? stages[stages.length - 1].stage;
      job.status.progress = p;
      job.status.stage = stage;
      emitBoth({ type: "progress", jobId, progress: p, stage });
      if (p >= 1) {
        if (job.timer) clearInterval(job.timer);
        job.timer = null;
        job.status.state = "done";
        const result = this.finishGenJob(kind, params);
        job.status.result = result as unknown as SidecarJobStatus["result"];
        emitBoth({ type: "done", jobId, result: result as GenJobResult & Record<string, unknown> });
      }
    }, 100);
    return jobId;
  }

  /** Build the per-kind result AND apply the control-plane conveniences the
   * real backend performs (auto-import, auto-instrument-load). */
  private finishGenJob(kind: string, params: Record<string, unknown>): GenJobResult {
    if (kind === "amtInfill") {
      const start = Number(params.regionStartTicks ?? 0);
      const end = Number(params.regionEndTicks ?? start + 4 * this.ppq);
      const step = this.ppq / 4;
      const scale = [57, 60, 62, 64, 67, 69, 72, 76];
      const notes: MidiNote[] = [];
      const seed = hashString(JSON.stringify(params));
      for (let t = start, i = 0; t + step <= end; t += step, i++) {
        if (noise(seed, i) < 0.22) continue; // breathing room
        notes.push({
          tick: Math.round(t),
          lengthTicks: Math.round(step * (noise(seed ^ 7, i) > 0.7 ? 1.8 : 0.9)),
          key: scale[Math.floor(noise(seed ^ 13, i) * scale.length)],
          velocity: 72 + Math.floor(noise(seed ^ 29, i) * 44),
          channel: 0,
        });
      }
      return { kind: "amtInfill", ppq: this.ppq, notes, simulated: true };
    }
    if (kind === "stableAudioSfz") {
      const name = String(params.name ?? params.prompt ?? "generated").slice(0, 28);
      const keys = Array.isArray(params.keys) ? (params.keys as number[]) : [36, 48, 60, 72];
      const info: InstrumentInfo = {
        id: uuid(),
        name,
        sfzPath: `${String(params.outputDir ?? "/home/demo/instruments")}/${name.replace(/\s+/g, "-")}.sfz`,
        regionCount: keys.length * Number(params.velocityLayers ?? 1),
        keyLow: Math.min(...keys),
        keyHigh: Math.max(...keys),
      };
      this.instruments.push(info); // == control plane auto-load
      return {
        kind: "stableAudioSfz",
        name: info.name,
        sfzPath: info.sfzPath,
        sampleCount: info.regionCount,
        simulated: true,
      };
    }
    // audio kinds — honor the importToTrackId convenience
    const outputPath = String(params.outputPath ?? "/home/demo/gen/output.wav");
    const durationSeconds =
      Number(params.durationSeconds) > 0 ? Number(params.durationSeconds) : 30;
    if ("importToTrackId" in params) {
      const requested = params.importToTrackId ? String(params.importToTrackId) : null;
      let trackId = requested && this.track(requested) ? requested : null;
      if (!trackId) {
        const prompt = String(params.prompt ?? "generated");
        const track = this.makeTrack({
          name: prompt.slice(0, 22) || "generated",
          color: kind === "elevenLabsMusic" ? "#ffc857" : "#ff4fd8",
        });
        this.tracks.push(track);
        trackId = track.id;
      }
      const at = Number(params.importAtSamples ?? 0);
      const clip = this.makeClip(
        trackId,
        outputPath.split("/").pop() ?? "generated.wav",
        at,
        Math.round(durationSeconds * SR),
      );
      this.characters.set(clip.id, kind === "elevenLabsMusic" ? "pad" : "other");
      this.clips.push(clip);
      this.resyncAudio();
    }
    return { kind, outputPath, durationSeconds, sampleRate: SR, simulated: true };
  }

  // ── wave 1.5: hum-to-song ──

  /** A plausible hummed phrase: A-minor pentatonic, phrase-shaped, ~2 bars. */
  private fakeHumNotes(seed: number, quantizeGrid: number): MidiNote[] {
    const scale = [57, 60, 62, 64, 67, 69, 72, 74];
    const grid = quantizeGrid > 0 ? (this.ppq * 4) / quantizeGrid : this.ppq / 2;
    const notes: MidiNote[] = [];
    let tick = 0;
    let degree = 2 + (seed % 3);
    for (let i = 0; i < 11; i++) {
      const rest = noise(seed ^ 0x51ab, i) < 0.18;
      const lenSteps = noise(seed ^ 0x77, i) > 0.72 ? 2 : 1;
      if (!rest) {
        degree = Math.max(
          0,
          Math.min(scale.length - 1, degree + Math.round((noise(seed, i) - 0.5) * 3)),
        );
        notes.push({
          tick: Math.round(tick),
          lengthTicks: Math.max(1, Math.round(grid * lenSteps * 0.92)),
          key: scale[degree],
          velocity: 84 + Math.floor(noise(seed ^ 0xbeef, i) * 36),
          channel: 0,
        });
      }
      tick += grid * lenSteps;
    }
    return notes;
  }

  async humToSong(
    request: HumToSongRequest,
    onEvent: (e: SidecarEvent) => void,
  ): Promise<string> {
    if (request.clipId && request.path) throw new Error("pass either clipId or path, not both");
    if (!request.clipId && !request.path) {
      throw new Error("pass clipId (a recorded clip) or path (a WAV file)");
    }
    let sourceName = "hum";
    if (request.clipId) {
      const clip = this.clips.find((c) => c.id === request.clipId);
      if (!clip) throw new Error(`unknown audio clip: ${request.clipId}`);
      sourceName = clip.name;
    } else if (request.path) {
      if (!request.path.startsWith("/")) throw new Error(`path must be absolute: ${request.path}`);
      sourceName = request.path.split("/").pop()?.replace(/\.\w+$/, "") ?? "hum";
    }
    if (request.trackId) {
      const t = this.track(request.trackId);
      if (!t) throw new Error(`unknown track: ${request.trackId}`);
      if (t.kind !== "midi") {
        throw new Error(`track ${request.trackId} is kind "${t.kind}" (hummed melodies need a midi track)`);
      }
    }

    const jobId = uuid();
    const job: DemoJob = {
      status: { jobId, kind: "humToMidi", state: "queued", progress: 0, stage: "queued" },
      onEvent,
      timer: null,
      request: request as Record<string, unknown>,
    };
    this.jobs.set(jobId, job);
    const stages = [
      { until: 0.14, stage: "loadingAudio" },
      { until: 0.55, stage: "trackingPitch" },
      { until: 0.8, stage: "segmenting" },
      { until: 1.0, stage: "quantizing" },
    ];
    const emitBoth = (e: SidecarEvent) => {
      onEvent(e);
      if (e.type === "progress") this.emit("sidecar://progress", e);
      else if (e.type === "done") this.emit("sidecar://done", e);
      else if (e.type === "error") this.emit("sidecar://error", e);
    };
    const durationMs = 4200;
    const started = this.now();
    job.status.state = "running";
    job.timer = setInterval(() => {
      const p = Math.min(1, (this.now() - started) / durationMs);
      const stage = stages.find((s) => p <= s.until)?.stage ?? "quantizing";
      job.status.progress = p;
      job.status.stage = stage;
      emitBoth({ type: "progress", jobId, progress: p, stage });
      if (p >= 1) {
        if (job.timer) clearInterval(job.timer);
        job.timer = null;
        job.status.state = "done";
        const seed = hashString(sourceName + jobId);
        const notes = this.fakeHumNotes(seed, request.quantizeGrid ?? 0);
        const end = notes.reduce((m, n) => Math.max(m, n.tick + n.lengthTicks), 0);
        const lengthTicks = Math.max(BAR_TICKS, Math.ceil(end / BAR_TICKS) * BAR_TICKS);
        const result = {
          kind: "humToMidi",
          ppq: this.ppq,
          bpm: request.bpm ?? TEMPO,
          lengthTicks,
          notes,
          simulated: true,
        };
        job.status.result = result as unknown as SidecarJobStatus["result"];
        emitBoth({
          type: "done",
          jobId,
          // humToMidi rides the open sidecar envelope (kind-discriminated blob)
          result: result as unknown as NonNullable<SidecarJobStatus["result"]>,
        });
        // The real control plane applies the clip AFTER done and reports it
        // as a follow-up log line on the same channel.
        setTimeout(() => {
          const { clip, created } = this.applyHumClip(
            notes,
            lengthTicks,
            request.trackId ?? null,
            request.atTicks ?? 0,
            `${sourceName} melody`,
          );
          let line = `hum-to-song: melody clip ${clip.id} (${clip.notes.length} notes) on track ${clip.trackId}${created ? ` (auto-created "${created.name}")` : ""}`;
          if (request.accompany) {
            const accompId = this.runFakeAccompany(clip, onEvent);
            line += `; accompaniment job ${accompId} submitted`;
          }
          onEvent({ type: "log", jobId, line });
        }, 350);
      }
    }, 100);
    return jobId;
  }

  private applyHumClip(
    notes: MidiNote[],
    lengthTicks: number,
    trackId: string | null,
    atTicks: number,
    name: string,
  ): { clip: MidiClip; created: TrackState | null } {
    let created: TrackState | null = null;
    let target = trackId;
    if (!target) {
      created = this.makeTrack({ name, color: "#52e5ff", kind: "midi" });
      this.tracks.push(created);
      target = created.id;
    }
    const clip: MidiClip = {
      id: uuid(),
      trackId: target,
      name,
      timelineStartTicks: atTicks,
      lengthTicks,
      notes: [...notes].sort(byTickKey),
    };
    this.midiClips.push(clip);
    this.resyncAudio();
    return { clip, created };
  }

  /** Chained accompaniment job: events stream over the SAME channel. */
  private runFakeAccompany(melody: MidiClip, onEvent: (e: SidecarEvent) => void): string {
    const jobId = uuid();
    const job: DemoJob = {
      status: { jobId, kind: "amtInfill", state: "running", progress: 0, stage: "queued" },
      onEvent,
      timer: null,
      request: {},
    };
    this.jobs.set(jobId, job);
    const started = this.now();
    const durationMs = 3600;
    job.timer = setInterval(() => {
      const p = Math.min(1, (this.now() - started) / durationMs);
      const stage = p < 0.25 ? "loading anticipation" : p < 0.9 ? "sampling tokens" : "merging notes";
      job.status.progress = p;
      job.status.stage = stage;
      onEvent({ type: "progress", jobId, progress: p, stage });
      if (p >= 1) {
        if (job.timer) clearInterval(job.timer);
        job.timer = null;
        job.status.state = "done";
        // Low chord tones under the melody, half-note pulse.
        const seed = hashString(jobId);
        const roots = [45, 41, 36, 43]; // A F C G
        const notes: MidiNote[] = [];
        for (let t = 0; t + this.ppq * 2 <= melody.lengthTicks; t += this.ppq * 2) {
          const bar = Math.floor(t / BAR_TICKS);
          const root = roots[bar % roots.length];
          notes.push(
            { tick: t, lengthTicks: this.ppq * 2 - 60, key: root, velocity: 92, channel: 0 },
            {
              tick: t,
              lengthTicks: this.ppq * 2 - 60,
              key: root + (noise(seed, bar) > 0.5 ? 7 : 12),
              velocity: 78,
              channel: 0,
            },
          );
        }
        onEvent({
          type: "done",
          jobId,
          result: { kind: "amtInfill", ppq: this.ppq, notes, simulated: true } as unknown as NonNullable<
            SidecarJobStatus["result"]
          >,
        });
        setTimeout(() => {
          const { clip } = this.applyHumClip(notes, melody.lengthTicks, null, melody.timelineStartTicks, "Hum Accompaniment");
          onEvent({
            type: "log",
            jobId,
            line: `hum-to-song: accompaniment clip ${clip.id} (${clip.notes.length} notes) on track ${clip.trackId}`,
          });
        }, 300);
      }
    }, 100);
    return jobId;
  }

  // ── wave 1.5: export ──

  async exportCapabilities(): Promise<ExportCapabilities> {
    return {
      formats: ["wav", "mp3", "flac", "ogg", "m4a"],
      ffmpeg: true,
      ffmpegPath: "/usr/bin/ffmpeg",
      hint: null,
    };
  }

  async exportJobStatus(jobId: string): Promise<ExportJobStatus> {
    const st = this.exportJobs.get(jobId);
    if (!st) throw new Error(`unknown export job: ${jobId}`);
    return { ...st };
  }

  async exportSong(request: ExportRequest): Promise<string> {
    if (!request.path.startsWith("/")) throw new Error(`path must be absolute: ${request.path}`);
    const format = (
      request.format ?? request.path.split(".").pop() ?? "wav"
    ).toLowerCase();
    if (!["wav", "mp3", "flac", "ogg", "m4a", "aac", "mp4"].includes(format)) {
      throw new Error(`unsupported export format: ${format} (wav|mp3|flac|ogg|m4a)`);
    }
    const region = request.region ?? "full";
    if (region === "loop" && this.loopEnd <= this.loopStart) {
      throw new Error("no loop region set — set one with transport_set_loop first");
    }
    if (this.clips.length === 0 && !this.midiClips.some((c) => c.notes.length > 0)) {
      throw new Error("nothing to export — the project has no audio clips or midi notes");
    }
    const bitDepth = request.bitDepth ?? 24;
    if (![16, 24, 32].includes(bitDepth)) {
      throw new Error(`unsupported bit depth: ${bitDepth} (16|24|32)`);
    }
    const tail = request.tailSeconds ?? (region === "loop" ? 0 : 1);
    let frames: number;
    if (region === "loop") {
      frames = this.loopEnd - this.loopStart + Math.round(tail * SR);
    } else {
      let end = 0;
      for (const c of this.clips) end = Math.max(end, c.timelineStartSamples + c.lengthSamples);
      for (const m of this.midiClips) {
        end = Math.max(end, this.ticksToSamples(m.timelineStartTicks + m.lengthTicks));
      }
      frames = Math.round(end + tail * SR);
    }
    const rate = request.sampleRate ?? SR;
    const durationSeconds = frames / SR;

    const jobId = uuid();
    const status: ExportJobStatus = {
      jobId,
      kind: "exportSong",
      state: "queued",
      progress: 0,
      stage: "queued",
      result: null,
      error: null,
    };
    this.exportJobs.set(jobId, status);
    this.emit("export://progress", { jobId, progress: 0, stage: "queued" });

    const needsEncode = format !== "wav" || rate !== SR;
    const renderTop = needsEncode ? 0.75 : 0.9;
    const durationMs = 2600 + Math.random() * 900;
    const started = this.now();
    const tick = setInterval(() => {
      const p = Math.min(1, (this.now() - started) / durationMs);
      let stage: string;
      let progress: number;
      if (p < 0.05) {
        stage = "building graph";
        progress = 0.02 + p;
      } else if (p < 0.8) {
        stage = "rendering";
        progress = 0.05 + ((p - 0.05) / 0.75) * (renderTop - 0.05);
      } else if (p < 0.88 || !needsEncode) {
        stage = "writing wav";
        progress = Math.min(1, renderTop + ((p - 0.8) / 0.2) * (1 - renderTop));
      } else {
        stage = "encoding";
        progress = 0.85 + ((p - 0.88) / 0.12) * 0.15;
      }
      status.state = "running";
      status.progress = Math.min(1, progress);
      status.stage = stage;
      this.emit("export://progress", { jobId, progress: status.progress, stage });
      if (p >= 1) {
        clearInterval(tick);
        const bytesPerSample = format === "wav" ? bitDepth / 8 : 0.18; // lossy ≈ estimate
        const result: ExportResult = {
          path: request.path,
          format: format === "aac" || format === "mp4" ? "m4a" : format,
          region,
          sampleRate: rate,
          bitDepth,
          frames,
          durationSeconds,
          sizeBytes: Math.round(frames * 2 * bytesPerSample) + 44,
        };
        status.state = "done";
        status.progress = 1;
        status.stage = "done";
        status.result = result;
        this.emit("export://done", { ...result, jobId });
      }
    }, 90);
    return jobId;
  }

  // ── wave 1.5: import + split stems ──

  async importAudioClipSplitStems(
    request: ImportClipRequest,
    onEvent: (e: SidecarEvent) => void,
  ): Promise<ImportSplitReply> {
    const clip = await this.importAudioClip(request);
    const jobId = await this.runFakeJob("stemSplit", { inputPath: request.path }, (e) => {
      onEvent(e);
      if (e.type === "done") {
        // Real-backend protocol: `done` first, then the stems land one by
        // one, each announced by a follow-up `log` line.
        setTimeout(() => {
          for (const line of this.materializeDemoStems(clip)) {
            onEvent({ type: "log", jobId: e.jobId, line });
          }
        }, 250);
      }
    });
    return { clip, jobId };
  }

  /** SPLIT STEMS on a clip ALREADY on the timeline (Task 11 addendum): no
   * re-import, no duplicate clip — mirrors the real backend's
   * `split_stems_for_clip`, reusing the same demo stem-materialization as
   * `importAudioClipSplitStems` above, just without the leading import. */
  async splitStemsForClip(clipId: string, onEvent: (e: SidecarEvent) => void): Promise<string> {
    const source = this.clips.find((c) => c.id === clipId);
    if (!source) throw new Error(`unknown clip: ${clipId}`);
    return this.runFakeJob("stemSplit", { inputPath: source.sourcePath }, (e) => {
      onEvent(e);
      if (e.type === "done") {
        // Real-backend protocol: `done` first, then the stems land one by
        // one, each announced by a follow-up `log` line.
        setTimeout(() => {
          for (const line of this.materializeDemoStems(source)) {
            onEvent({ type: "log", jobId: e.jobId, line });
          }
        }, 250);
      }
    });
  }

  private materializeDemoStems(source: Clip): string[] {
    const stems: { name: StemName; label: string; color: string; character: ClipCharacter }[] = [
      { name: "drums", label: "Drums", color: "#ffc857", character: "drums" },
      { name: "bass", label: "Bass", color: "#ff4fd8", character: "bass" },
      { name: "vocals", label: "Vocals", color: "#52e5ff", character: "vocals" },
      { name: "other", label: "Other", color: "#9d7bff", character: "pad" },
    ];
    let at = this.tracks.findIndex((t) => t.id === source.trackId);
    const lines: string[] = [];
    for (const stem of stems) {
      const track = this.makeTrack({ name: `${stem.label} · ${source.name}`, color: stem.color });
      this.tracks.splice(at >= 0 ? ++at : this.tracks.length, 0, track);
      const clip = this.makeClip(track.id, `${source.name} — ${stem.name}`, source.timelineStartSamples, source.lengthSamples);
      clip.sourcePath = `stems/${source.id}/${stem.name}.wav`;
      this.characters.set(clip.id, stem.character);
      this.clips.push(clip);
      lines.push(`auto-import stem \`${stem.name}\`: clip ${clip.id} on track ${track.name} (${track.id})`);
    }
    this.resyncAudio();
    return lines;
  }

  // ── wave 1.5: loop-jam ──

  private ljStatus(): LoopJamStatus {
    return {
      state: this.ljState,
      jobId: this.ljJobId,
      trackId: this.ljTrackId,
      loopStartSamples: Math.round(this.loopStart),
      loopEndSamples: Math.round(this.loopEnd),
      generation: this.ljGeneration,
      lastError: this.ljLastError,
    };
  }

  private ljEmit() {
    this.emit("loopjam://state", this.ljStatus());
  }

  private ljSchedule(ms: number, fn: () => void) {
    this.ljTimers.push(setTimeout(fn, ms));
  }

  private ljClearTimers() {
    for (const t of this.ljTimers) clearTimeout(t);
    this.ljTimers = [];
  }

  async loopjamStatus(): Promise<LoopJamStatus> {
    return this.ljStatus();
  }

  async loopjamEvolve(options: EvolveOptions = {}): Promise<LoopJamStatus> {
    if (this.loopEnd <= this.loopStart) {
      throw new Error("no loop region set — set one with transport_set_loop first");
    }
    if (this.ljState === "generating" || this.ljState === "ready") {
      throw new Error("an evolve cycle is already in flight — cancel it first");
    }
    let target = options.trackId ?? null;
    if (target && !this.track(target)) throw new Error(`unknown track: ${target}`);
    if (!target) {
      const hit = this.clips.find(
        (c) =>
          c.timelineStartSamples < this.loopEnd &&
          c.timelineStartSamples + c.lengthSamples > this.loopStart &&
          this.track(c.trackId)?.kind !== "midi",
      );
      if (!hit) throw new Error("no audio clip inside the loop region to evolve");
      target = hit.trackId;
    }
    this.ljClearTimers();
    this.ljState = "generating";
    this.ljJobId = uuid();
    this.ljTrackId = target;
    this.ljLastError = null;
    this.ljEmit();

    const seed = options.seed ?? Date.now();
    this.ljSchedule(3400 + Math.random() * 900, () => {
      if (this.ljState !== "generating") return;
      this.ljState = "ready";
      this.ljEmit();
      // Apply at the next loop wrap while rolling; ~1.2 s later when parked.
      const pos = this.position();
      const applyIn =
        this.tState !== "stopped" && this.loopEnabled && pos < this.loopEnd
          ? ((this.loopEnd - pos) / SR) * 1000
          : 1200;
      this.ljSchedule(applyIn, () => {
        if (this.ljState !== "ready") return;
        const clip = this.clips.find(
          (c) =>
            c.trackId === this.ljTrackId &&
            c.timelineStartSamples < this.loopEnd &&
            c.timelineStartSamples + c.lengthSamples > this.loopStart,
        );
        if (clip) {
          // Repaint = new audio in place: reseed the synth + fresh tiles.
          this.clipSeeds.set(clip.id, hashString(`${clip.id}:${seed}:${this.ljGeneration}`));
          this.clipBuffers.delete(clip.id);
          const cycle: ClipCharacter[] = ["other", "pad", "bass"];
          this.characters.set(clip.id, cycle[this.ljGeneration % cycle.length]);
          this.resyncAudio();
        }
        this.ljGeneration++;
        this.ljState = "applied";
        this.ljJobId = null;
        this.ljEmit();
      });
    });
    return this.ljStatus();
  }

  async loopjamCancel(): Promise<LoopJamStatus> {
    if (this.ljState === "generating" || this.ljState === "ready") {
      this.ljClearTimers();
      this.ljState = "idle";
      this.ljJobId = null;
      this.ljEmit();
    }
    return this.ljStatus();
  }

  // ── wave 1.5: Zyn patches ──

  async zynListPatches(): Promise<ZynPatch[]> {
    await new Promise((r) => setTimeout(r, 180 + Math.random() * 200));
    return this.zynPatches.map((p) => ({ ...p }));
  }

  async zynLoadPatch(instanceId: string, path: string): Promise<void> {
    const inst = this.pluginInstances.get(instanceId);
    if (!inst) throw new Error(`unknown plugin instance: ${instanceId}`);
    if (!inst.uid.toLowerCase().includes("zyn")) {
      throw new Error(`${inst.name} is not a ZynAddSubFX instance`);
    }
    const patch = this.zynPatches.find((p) => p.path === path);
    if (!patch) throw new Error(`patch not found: ${path}`);
    await new Promise((r) => setTimeout(r, 250));
    this.zynLoaded.set(instanceId, patch);
    // Audible in the demo synth: bank shapes the voice's filter/detune.
    const params = this.pluginParams.get(instanceId) ?? [];
    const cutoff = params.find((p) => p.name.toLowerCase().includes("cutoff"));
    const detune = params.find((p) => p.name.toLowerCase().includes("detune"));
    const byBank: Record<string, [number, number]> = {
      Bass: [700, 3],
      Pads: [1100, 14],
      Choir_and_Voice: [1600, 9],
      Arpeggios: [3800, 6],
      Plucked: [5600, 2],
      Synth_Leads: [8600, 11],
    };
    const [hz, ct] = byBank[patch.bank] ?? [3400, 6];
    if (cutoff) cutoff.value = Math.min(cutoff.max, Math.max(cutoff.min, hz));
    if (detune) detune.value = Math.min(detune.max, Math.max(detune.min, ct));
    this.resyncAudio();
  }

  /** Patch currently loaded into a demo Zyn instance (UI affordance). */
  zynLoadedPatch(instanceId: string): ZynPatch | undefined {
    return this.zynLoaded.get(instanceId);
  }

  // ── phase 2: mcp ──

  async mcpGetStatus(): Promise<McpStatus> {
    return {
      running: true,
      port: this.mcpPolicy.port ?? 41717,
      tokenFingerprint: "d3a0f4c8",
      policy: { ...this.mcpPolicy, toolOverrides: { ...this.mcpPolicy.toolOverrides } },
      pending: [...this.mcpPending.values()].map(({ apply: _a, ...p }) => ({ ...p })),
    };
  }

  async mcpSetPolicy(policy: McpPolicy): Promise<McpPolicy> {
    this.mcpPolicy = { toolOverrides: {}, port: 41717, ...policy };
    return { ...this.mcpPolicy };
  }

  async mcpConfirmPending(confirmationId: string, approve: boolean): Promise<void> {
    const pending = this.mcpPending.get(confirmationId);
    if (!pending) throw new Error(`unknown confirmation: ${confirmationId}`);
    this.mcpPending.delete(confirmationId);
    if (approve) {
      pending.apply?.();
      this.refreshTrackAudio();
    }
  }

  private requestConfirm(tool: string, summary: string, apply?: () => void) {
    if (this.mcpPolicy.mode !== "confirmDestructive") return;
    const pending: PendingConfirmation & { apply?: () => void } = {
      id: uuid(),
      tool,
      summary,
      requestedAt: new Date().toISOString(),
      apply,
    };
    this.mcpPending.set(pending.id, pending);
    const { apply: _a, ...wire } = pending;
    this.emit("mcp://confirm-requested", { ...wire });
    // real server: 60 s timeout ⇒ deny
    setTimeout(() => this.mcpPending.delete(pending.id), 60_000);
  }

  /** A scripted "agent session" so the confirm flow demos itself. */
  private scheduleMcpScript() {
    if (this.mcpScriptStarted) return;
    this.mcpScriptStarted = true;
    setTimeout(() => {
      const bass = this.tracks.find((t) => t.name === "Synth Bass");
      this.requestConfirm(
        "set_track_mix",
        `Set "${bass?.name ?? "Synth Bass"}" gain to -4.2 dB, pan 12% L`,
        () => {
          if (bass) {
            bass.gainDb = -4.2;
            bass.pan = -0.12;
          }
        },
      );
    }, 15_000);
    setTimeout(() => {
      this.requestConfirm(
        "run_sidecar_job",
        'aceStepGenerate: "rain-slick chrome arps, 118 bpm" (30 s → new track)',
      );
    }, 75_000);
  }

  // The browser demo has no filesystem: the eight built-ins are the whole
  // catalogue there.
  listUserThemes() {
    return Promise.resolve([] as UserThemeFile[]);
  }
  writeUserTheme() {
    return Promise.reject(new Error("user themes need the desktop app"));
  }
  userThemesDir() {
    return Promise.resolve("(desktop app only)");
  }

  // ── events ──

  on<K extends AuraEventName>(event: K, cb: (payload: AuraEventMap[K]) => void): Unsubscribe {
    let subs = this.listeners.get(event);
    if (!subs) {
      subs = new Set();
      this.listeners.set(event, subs);
    }
    const wrapped = cb as (payload: unknown) => void;
    subs.add(wrapped);
    return () => subs.delete(wrapped);
  }
}
