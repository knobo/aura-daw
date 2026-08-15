/**
 * Hand-written TypeScript mirrors of docs/ipc-schemas/*.schema.json.
 * These are the wire shapes for the frozen Tauri command surface —
 * keep field names camelCase, exactly as on the wire.
 */

import type { SectionRow } from "../sectionTable";

// ── audio-device.schema.json ────────────────────────────────────────────────

export interface AudioDevice {
  id: string;
  name: string;
  isDefault: boolean;
  maxChannels: number;
  defaultSampleRate: number;
}

// ── midi-input (midi-input-ports slice 1, not yet schema'd) ─────────────────
// Hardware MIDI input port list/select/activity. App-config only — no
// document coupling (mirrors the audio device selects above).

export interface MidiPortInfo {
  id: string;
  name: string;
}

export interface MidiInputStatus {
  selected: MidiPortInfo | null;
  eventsSeen: number;
  lastEventAgeMs: number | null;
  lastStatusBytes: number[];
  /** Live monitoring (slice 1b): incoming notes audible via the preview
   * voice when true. Always false when nothing is selected. */
  monitor: boolean;
  /** Track incoming notes are routed to (slice 2), or null. */
  targetTrackId: string | null;
  /** Notes are reaching a track's instrument right now. */
  routing: boolean;
  droppedEvents: number;
  capturing: boolean;
}

// ── midi-output: clock/sync + per-track/per-clip routing ───────────────────
// App-config only, same carve-out as the input side above — never stored in
// the project. Any number of ports can be open at once (`outputs`); a
// track- or clip-scoped route (`routes`) sends that track's/clip's notes to
// one port+channel. A clip route always wins over its track's route.

export interface MidiOutputPortStatus {
  port: MidiPortInfo;
  clockEnabled: boolean;
  running: boolean;
  pulsesSent: number;
  resyncs: number;
  notesSent: number;
}

export interface MidiRouteStatus {
  scope: "track" | "clip";
  id: string;
  portId: string;
  channel: number;
  /** cpal input-device name this track records its audio return from. Clip
   * routes never have one; missing/null = MIDI-only. */
  returnDevice?: string | null;
}

export interface MidiOutputStatus {
  outputs: MidiOutputPortStatus[];
  routes: MidiRouteStatus[];
}

// ── transport-state.schema.json ─────────────────────────────────────────────

export type TransportMode = "stopped" | "playing" | "recording";

export interface TransportState {
  state: TransportMode;
  positionSamples: number;
  sampleRate: number;
  tempoBpm: number;
  loopEnabled: boolean;
  loopStartSamples: number;
  loopEndSamples: number;
  /** Last audible sample of the material (clip ends + final note-off); 0 = nothing to play. */
  songEndSamples: number;
  /** Stop the transport on reaching songEndSamples (ignored while looping/recording). */
  stopAtEnd: boolean;
  /** Samples of count-in still to play before a take arms. 0 when idle. */
  countInLeftSamples?: number;
}

// ── track-state.schema.json ─────────────────────────────────────────────────

export type TrackKind = "audio" | "midi" | "automation" | "bus";

export interface TrackState {
  id: string;
  name: string;
  kind: TrackKind;
  /** dB relative to unity; -160 encodes -inf. UI range -60..+12. */
  gainDb: number;
  /** -1 hard left .. +1 hard right. */
  pan: number;
  muted: boolean;
  soloed: boolean;
  armed: boolean;
  /** "#rrggbb" lowercase. */
  color: string;
  /** Sampler instrument bound to this (midi) track — additive phase-2 field. */
  instrumentId?: string | null;
}

// ── clip.schema.json ────────────────────────────────────────────────────────

export interface Clip {
  id: string;
  trackId: string;
  name: string;
  /** Relative to the .aura dir, POSIX separators. */
  sourcePath: string;
  sourceChannels: number;
  sourceSampleRate: number;
  sourceLengthSamples: number;
  timelineStartSamples: number;
  offsetSamples: number;
  lengthSamples: number;
  gainDb: number;
  fadeInSamples: number;
  fadeOutSamples: number;
}

// ── meter-frame.schema.json ─────────────────────────────────────────────────

export interface TrackMeter {
  /** Track UUID, or the literal "master". */
  trackId: string;
  /** Linear peak, full scale = 1.0 (may exceed on clip). */
  peakL: number;
  peakR: number;
  rmsL: number;
  rmsR: number;
  clipped: boolean;
}

export interface MeterFrame {
  seq: number;
  positionSamples: number;
  tracks: TrackMeter[];
  master: TrackMeter;
}

// ── recording-command.schema.json ───────────────────────────────────────────

export interface RecordingState {
  recording: boolean;
  trackIds: string[];
  startedAtSamples?: number;
  xruns?: number;
  /** Present on the stop notification: the take's AUDIO clips only. */
  clips?: Clip[];
  /** Stop notification, MIDI take only: the clip `MidiClipAdd` registered
   * in the take's transaction. `null`/absent when the take recorded no
   * MIDI. It is an id, not the clip — the store re-pulls, since the clip
   * arrives through no other channel. */
  midiClipId?: string | null;
}

// ── project.schema.json ─────────────────────────────────────────────────────

/** Who committed a transaction (control::op::Actor, externally-tagged serde
 * enum with rename_all="camelCase"): a unit variant serializes as a bare
 * string, the struct variant as `{ agent: { tool } }`. */
export type Actor = "user" | "engine" | "system" | { agent: { tool: string } };

/**
 * `create_project`/`open_project`/`save_project_as`'s return value AND the
 * `project://changed` app-event payload — both are `audio::types::Project`
 * (src-tauri/src/audio/types.rs), which the backend has NOT bumped past
 * schemaVersion 1 despite the MIDI side landing schemaVersion 3 in
 * project.json (see project-v3.schema.json): `tracks`/`clips` stay flat and
 * audio-only here — MIDI clips are a separate surface (`MidiClip`,
 * `ProjectSnapshot.midiClips`), never folded into this type. `rev`/`label`/
 * `actor` are additive fields control::mod's `execute()` inserts into the
 * `project://changed` payload only (control/mod.rs:611-622) — every OTHER
 * emit site (audio::mod, control::import, control::loopjam) sends the bare
 * v1 struct without them, so treat all three as optional even on that
 * event.
 */
export interface Project {
  schemaVersion: 1;
  name: string;
  path?: string;
  createdAt?: string;
  modifiedAt?: string;
  sampleRate: number;
  tempoBpm: number;
  timeSignature?: [number, number];
  tracks: TrackState[];
  clips: Clip[];
  transport?: TransportState;
  /** `project://changed` only (control::mod's execute() path): the
   * committed transaction's revision counter (control/session.rs). */
  rev?: number;
  /** `project://changed` only: the committed transaction's human label. */
  label?: string;
  /** `project://changed` only: who committed the transaction. */
  actor?: Actor;
}

// ── waveform-tile.schema.json ───────────────────────────────────────────────

export interface WaveformTileRequest {
  clipId: string;
  /** Bin width in source samples = 2^(8+lod). */
  lod: number;
  /** Tile n covers bins [n*1024, (n+1)*1024). */
  tileIndex: number;
}

export const AWTF_MAGIC = 0x41575446;
export const AWTF_BINS_PER_TILE = 1024;

/** Parsed AWTF binary tile (response of get_waveform_tile). */
export interface WaveformTile {
  channels: number;
  lod: number;
  tileIndex: number;
  bins: number;
  /** channels × bins × (min, max) i16 pairs, interleaved per channel block. */
  data: Int16Array;
}

// ── sidecar-job.schema.json ─────────────────────────────────────────────────

export type StemName = "vocals" | "drums" | "bass" | "other";

export interface StemSplitRequest {
  inputPath: string;
  model?: string | null;
  stems?: StemName[] | null;
  outputDir?: string | null;
}

export interface TranscribeRequest {
  inputPath: string;
  model?: string | null;
  language?: string | null;
  withPitch?: boolean | null;
}

export interface SidecarProgressEvent {
  type: "progress";
  jobId: string;
  progress: number;
  stage: string;
}

export interface SidecarLogEvent {
  type: "log";
  jobId: string;
  line: string;
}

export interface StemSplitResult {
  kind: "stemSplit";
  stems: Partial<Record<StemName, string>>;
  elapsedSeconds?: number;
}

export interface TranscribeSegmentWord {
  word: string;
  startSeconds: number;
  endSeconds: number;
  pitchHz?: number | null;
}

export interface TranscribeSegment {
  startSeconds: number;
  endSeconds: number;
  text: string;
  words?: TranscribeSegmentWord[];
}

export interface TranscribeResult {
  kind: "transcribe";
  language: string;
  segments: TranscribeSegment[];
  transcriptPath?: string;
}

export type SidecarResult = StemSplitResult | TranscribeResult;

export interface SidecarDoneEvent {
  type: "done";
  jobId: string;
  result: SidecarResult;
}

export interface SidecarErrorEvent {
  type: "error";
  jobId: string;
  message: string;
}

export type SidecarEvent =
  | SidecarProgressEvent
  | SidecarLogEvent
  | SidecarDoneEvent
  | SidecarErrorEvent;

export type SidecarJobState = "queued" | "running" | "done" | "error" | "cancelled";

export interface SidecarJobStatus {
  jobId: string;
  /** Open kind string since phase 2 ("stemSplit", "aceStepGenerate", …). */
  kind: string;
  state: SidecarJobState;
  progress: number;
  stage: string;
  result?: SidecarResult | null;
  error?: string | null;
}

// ── midi-clip.schema.json + project-v2 (phase 2) ───────────────────────────

export interface TempoEvent {
  tick: number;
  bpm: number;
}

/** Persisted time signature (round-2 §3.3/O-10), v3+. */
export interface MeterEvent {
  tick: number;
  num: number;
  den: number;
}

/** v3 integer-period tempo event (round-2 §3.3) — the backend's real
 * storage; `TempoEvent.bpm` above stays a derived display value. */
export interface TempoPeriodEvent {
  tick: number;
  periodStart: number;
  periodEnd: number;
}

export interface TempoMapState {
  ppq: number;
  events: TempoEvent[];
  /** Additive v3 fields (round-2 §3.6) — see `../sectionTable.ts` for the
   * ONE bijection implementation that consumes `sectionTable`. */
  meterMap: MeterEvent[];
  periodEvents: TempoPeriodEvent[];
  sectionTable: SectionRow[];
  sectionTableRuleVersion: number;
}

export interface MidiNote {
  /** Backend-minted stable identity (ADR 0001) — round-trip it untouched on
   * edits; STRIP it on copies (note-ops copyNote), or the keep-rule re-mints
   * both the copy and the original. Absent on frontend-drawn notes. */
  noteId?: number;
  /** Onset, ticks relative to the clip start. */
  tick: number;
  lengthTicks: number;
  /** MIDI key, 60 = C4. */
  key: number;
  /** 1..127 */
  velocity: number;
  channel?: number;
}

export interface MidiClip {
  id: string;
  trackId: string;
  name: string;
  timelineStartTicks: number;
  lengthTicks: number;
  /** Content (loop/native) length in ticks; absent = same as lengthTicks. */
  contentLengthTicks?: number;
  /** Content identity (ADR 0004). Clip envelopes are keyed by this. */
  contentId?: string;
  /** Sorted by (tick, key). */
  notes: MidiNote[];
}

/** One clip's new placement in a `move_clips` batch (Track C). Audio is
 * placed in SAMPLES, MIDI in TICKS — two units that must never be
 * confusable, hence the `kind` tag. `lengthTicks`/`contentLengthTicks` are
 * additive and OPTIONAL: absent means "unchanged", never "clear". */
export type ClipPlacement =
  | { kind: "audio"; clipId: string; timelineStartSamples: number }
  | {
      kind: "midi";
      clipId: string;
      timelineStartTicks: number;
      lengthTicks?: number;
      contentLengthTicks?: number;
    };

// ── application/x-aura-clips (Track C) ──────────────────────────────────────

export interface SkippedClip {
  name: string;
  reason: string;
}

export type ClipboardClip =
  | {
      kind: "audio";
      name: string;
      sourceTrackId: string;
      sourceTrackName: string;
      offsetFromAnchorSamples: number;
      lengthSamples: number;
      sourcePath: string;
      sourceAbsPath: string | null;
      sourceChannels: number;
      sourceSampleRate: number;
      sourceLengthSamples: number;
      offsetSamples: number;
      gainDb: number;
      fadeInSamples: number;
      fadeOutSamples: number;
    }
  | {
      kind: "midi";
      name: string;
      sourceTrackId: string;
      sourceTrackName: string;
      offsetFromAnchorTicks: number;
      lengthTicks: number;
      contentLengthTicks: number | null;
      notes: MidiNote[];
    };

export interface AuraClipsPayload {
  mime: string;
  schemaVersion: number;
  anchorSamples: number;
  anchorTicks: number;
  ppq: number;
  sourceProjectDir: string | null;
  clips: ClipboardClip[];
}

export interface PasteRequest {
  payload: AuraClipsPayload;
  atSamples: number;
  toNewTracks: boolean;
}

export interface PasteResult {
  audioClips: Clip[];
  midiClips: MidiClip[];
  createdTracks: TrackState[];
  skipped: SkippedClip[];
}

/** Result of get_project_state (control plane cold-start snapshot). */
export interface ProjectSnapshot {
  projectName?: string | null;
  projectDir?: string | null;
  transport: TransportState;
  tracks: TrackState[];
  clips: Clip[];
  midiClips: MidiClip[];
  ppq: number;
  tempoEvents: TempoEvent[];
  /** Additive v3 fields (round-2 §3.6) — available from cold start, not
   * just after a set_tempo_map edit. */
  meterMap: MeterEvent[];
  periodEvents: TempoPeriodEvent[];
  sectionTable: SectionRow[];
  sectionTableRuleVersion: number;
}

// ── sidecar-job-v2.schema.json (phase 2, open job kinds) ───────────────────

export type GenJobKind =
  | "aceStepGenerate"
  | "aceStepRepaint"
  | "aceStepAudio2Audio"
  | "elevenLabsMusic"
  | "amtInfill"
  | "stableAudioSfz";

export interface AudioGenerationResult {
  kind: string;
  outputPath: string;
  durationSeconds?: number;
  sampleRate?: number;
  simulated?: boolean;
}

export interface AmtInfillResult {
  kind: "amtInfill";
  ppq?: number;
  notes: MidiNote[];
  simulated?: boolean;
}

export interface SfzInstrumentResult {
  kind: "stableAudioSfz";
  name?: string;
  sfzPath: string;
  sampleCount?: number;
  simulated?: boolean;
}

export type GenJobResult = AudioGenerationResult | AmtInfillResult | SfzInstrumentResult;

/** Done event payloads for phase-2 kinds reuse the v1 event envelope with an
 * open result blob; observers must render unknown kinds generically. */
export type OpenSidecarEvent =
  | SidecarProgressEvent
  | SidecarLogEvent
  | { type: "done"; jobId: string; result: GenJobResult & Record<string, unknown> }
  | SidecarErrorEvent;

// ── sampler-instrument.schema.json (phase 2) ───────────────────────────────

export interface InstrumentInfo {
  id: string;
  name: string;
  /** Absolute path of the source .sfz. */
  sfzPath: string;
  regionCount: number;
  keyLow?: number;
  keyHigh?: number;
}

// ── library & browser (Track E, src-tauri/src/library.rs) ──────────────────

export type LibraryEntryKind = "dir" | "audio";

/** One row of a `library_scan` listing. Filesystem metadata only — duration
 * and format are NOT probed during a scan (plan ruling 2). */
export interface LibraryEntry {
  /** File or directory name (no path). */
  name: string;
  /** Absolute path. */
  path: string;
  kind: LibraryEntryKind;
  /** Lowercased extension without the dot; empty for directories. */
  ext: string;
  sizeBytes: number;
  /** Modification time, ms since the Unix epoch (0 when unknown). */
  modifiedMs: number;
}

// ── plugin-descriptor.schema.json + plugin-state.schema.json (phase 3) ─────

export type PluginFormat = "clap" | "lv2";

/** One installed plugin as discovered by plugin_scan. */
export interface PluginDescriptor {
  /** Stable identity: "clap:<bundle>#<id>" or "lv2:<uri>". */
  uid: string;
  format: PluginFormat;
  name: string;
  vendor?: string;
  version?: string;
  /** Bundle file (CLAP) / bundle dir (LV2); machine-local, informational. */
  path?: string;
  isInstrument: boolean;
  audioInputs: number;
  audioOutputs: number;
  hasNoteInput: boolean;
  categories?: string[];
}

export type PluginInstanceStatus = "stub" | "active" | "crashed";

/** A live plugin instance registered in the host. */
export interface PluginInstanceInfo {
  /** Instance uuid; a midi track binds it via instrumentId = "plugin:<id>". */
  id: string;
  uid: string;
  name: string;
  format: PluginFormat;
  status: PluginInstanceStatus;
  trackId?: string | null;
}

/** One plugin parameter (plugin-state.schema.json#/$defs/param). */
export interface PluginParamInfo {
  /** Stable per-plugin id (CLAP param id / LV2 control-port index). */
  id: number;
  name: string;
  min: number;
  max: number;
  default: number;
  value: number;
  /** Discrete step count; 0 = continuous. */
  steps?: number;
}

/** One element of plugin_set_param's batched changes array. */
export interface PluginParamChange {
  id: number;
  value: number;
}

/** Result of plugin_list. */
export interface PluginListResult {
  /** Last scan result (empty until plugin_scan ran). */
  plugins: PluginDescriptor[];
  instances: PluginInstanceInfo[];
  scanned: boolean;
}

// ── automation (plugins::automation) ────────────────────────────────────────

/** One automation breakpoint in MUSICAL time (ticks @ project ppq). Between
 * points the value ramps linearly; before the first / after the last it
 * holds. Mirrors `plugins::automation::AutomationPoint`. */
export interface AutomationPoint {
  tick: number;
  value: number;
}

/** One parameter's automation curve. `targetNode` is `"track:<trackId>"` for
 * built-in track params (today: gain, `paramId` 0) or a plugin instance id
 * for plugin params. Mirrors `plugins::automation::AutomationLane`. */
export interface AutomationLane {
  id: string;
  targetNode: string;
  paramId: number;
  points: AutomationPoint[];
}

// ── modulation (modulation::model / commands) ──────────────────────────────

export type TrackParam = "gain" | "pan" | "mute" | "send0" | "send1";
export type BindingMode = "absolute" | "add" | "multiply";
export type Domain = "normalized" | "native";

export type Source =
  | { kind: "curve"; curveId: string }
  | { kind: "automationTrack"; trackId: string }
  | { kind: "clipEnvelope"; contentId: string; curveId: string }
  | { kind: "lfo"; modulatorId: string }
  | { kind: "macro"; macroId: string }
  | { kind: "midiCc"; modulatorId: string }
  | { kind: "envFollower"; modulatorId: string };

export type TargetRef =
  | { kind: "trackParam"; trackId: string; param: TrackParam }
  | { kind: "pluginParam"; instanceId: string; paramId: number }
  | { kind: "selfTrackParam"; param: TrackParam }
  | { kind: "selfInstrumentParam"; paramId: number }
  | { kind: "macro"; macroId: string }
  | { kind: "port"; nodeId: string; portId: string };

export interface Range {
  min: number;
  max: number;
}

export interface RangeSnapshot {
  min: number;
  max: number;
}

export interface Curve {
  id: string;
  name: string;
  lengthTicks?: number | null;
  points: AutomationPoint[];
}

export interface Binding {
  id: string;
  source: Source;
  target: TargetRef;
  mode: BindingMode;
  depth: number;
  range?: Range;
  domain?: Domain;
  rangeSnapshot?: RangeSnapshot | null;
  enabled?: boolean;
}

export interface AutomationClip {
  id: string;
  trackId: string;
  curveId: string;
  timelineStartTicks: number;
  lengthTicks: number;
  contentLengthTicks?: number | null;
}

/** Full modulation document returned by every modulation_* command (D-03). */
export interface ModulationSnapshot {
  curves: Curve[];
  bindings: Binding[];
  automationClips: AutomationClip[];
}

// ── mcp-policy.schema.json (phase 2) ───────────────────────────────────────

export type McpPolicyMode = "readOnly" | "confirmDestructive" | "full";
export type McpToolRule = "allow" | "confirm" | "deny";

export interface McpPolicy {
  mode: McpPolicyMode;
  toolOverrides?: Record<string, McpToolRule>;
  port?: number;
}

export interface PendingConfirmation {
  id: string;
  tool: string;
  /** Human-readable one-liner for the confirm dialog. */
  summary: string;
  requestedAt?: string;
}

export interface McpStatus {
  running: boolean;
  port: number;
  /** First 8 hex chars of the session token — NEVER the token itself. */
  tokenFingerprint: string;
  policy: McpPolicy;
  pending: PendingConfirmation[];
}

// ── control plane (phase 2) ────────────────────────────────────────────────

export interface ImportClipRequest {
  /** Absolute path to an audio file (wav/mp3/flac/ogg/aac/m4a — wave 1B). */
  path: string;
  trackId?: string | null;
  atSamples?: number | null;
}

// ── wave 1.5: hum-to-song (control/hum.rs) ─────────────────────────────────

/** Argument of `hum_to_song`. Exactly one of clipId/path selects the input. */
export interface HumToSongRequest {
  /** Id of an existing recorded/imported AUDIO clip in the open project. */
  clipId?: string | null;
  /** Absolute path to a WAV recording (alternative to clipId). */
  path?: string | null;
  /** Target MIDI track for the melody clip; null = auto-create one. */
  trackId?: string | null;
  /** Timeline placement of the melody clip in ticks (default 0). */
  atTicks?: number | null;
  /** Grid snap for note onsets: 4 = quarters, 8 = eighths… 0/null = off. */
  quantizeGrid?: number | null;
  /** Tempo override for seconds→ticks; default: the project tempo. */
  bpm?: number | null;
  /** Chain an amtInfill accompaniment over the melody span. */
  accompany?: boolean | null;
}

// ── wave 1.5: export (control/export.rs) ───────────────────────────────────

export interface ExportRequest {
  /** Absolute destination file path. */
  path: string;
  /** "wav" | "mp3" | "flac" | "ogg" | "m4a"; default inferred from path. */
  format?: string | null;
  /** "full" (default) or "loop" (the transport loop region). */
  region?: string | null;
  /** WAV bit depth: 16 | 24 | 32 (32 = IEEE float). Default 24. */
  bitDepth?: number | null;
  /** Output sample rate; differing from the engine rate requires ffmpeg. */
  sampleRate?: number | null;
  /** Master gain on the summed stereo bus, dB. Default 0. */
  masterGainDb?: number | null;
  /** Release tail after the region end, seconds (default 1 full / 0 loop). */
  tailSeconds?: number | null;
}

/** Result payload of a finished export (export://done, minus jobId). */
export interface ExportResult {
  path: string;
  format: string;
  region: string;
  sampleRate: number;
  bitDepth: number;
  frames: number;
  durationSeconds: number;
  sizeBytes: number;
}

export interface ExportJobStatus {
  jobId: string;
  /** Always "exportSong". */
  kind: string;
  state: "queued" | "running" | "done" | "error";
  progress: number;
  stage: string;
  result?: ExportResult | null;
  error?: string | null;
}

export interface ExportCapabilities {
  /** Formats exportable right now ("wav" always; the rest with ffmpeg). */
  formats: string[];
  ffmpeg: boolean;
  ffmpegPath?: string | null;
  /** Present when ffmpeg is missing: how to get the extra formats. */
  hint?: string | null;
}

export interface ExportProgressEvent {
  jobId: string;
  progress: number;
  stage: string;
}

export type ExportDoneEvent = ExportResult & { jobId: string };

export interface ExportErrorEvent {
  jobId: string;
  message: string;
}

// ── wave 1.5: import + split stems (control/import.rs) ─────────────────────

/** Reply of import_audio_clip_split_stems: the imported clip + Demucs job. */
export interface ImportSplitReply {
  clip: Clip;
  jobId: string;
  createdTrack?: TrackState | null;
}

/** Extensions the universal import decode path accepts. */
export const IMPORT_AUDIO_EXTS = ["wav", "mp3", "flac", "ogg", "aac", "m4a", "mp4"] as const;

// ── wave 1.5: loop-jam (control/loopjam.rs) ────────────────────────────────

export interface EvolveOptions {
  /** Target track; null = first audio track with clip audio in the region. */
  trackId?: string | null;
  /** Style prompt forwarded to the repaint worker. */
  prompt?: string | null;
  /** Generation seed; null = derived from the generation counter. */
  seed?: number | null;
  /** Force/deny simulate mode; null = follow AURA_SIDECAR_SIMULATE. */
  simulate?: boolean | null;
}

/** Snapshot of the loop-jam state machine (the loopjam://state payload). */
export interface LoopJamStatus {
  state: "idle" | "generating" | "ready" | "applied";
  jobId?: string | null;
  trackId?: string | null;
  loopStartSamples: number;
  loopEndSamples: number;
  /** Completed hot-swaps since construction. */
  generation: number;
  lastError?: string | null;
}

// ── wave 1.5: Zyn patch bank (plugins/patches.rs) ──────────────────────────

export interface ZynPatch {
  /** Bank directory name, e.g. "Arpeggios". */
  bank: string;
  /** Patch display name, e.g. "Arpeggio 1". */
  name: string;
  /** Program number inside the bank. */
  program: number;
  /** Absolute path of the .xiz file. */
  path: string;
}

/**
 * Reply of the additive `undo` / `redo` commands (Plan E Task 17). `label`
 * is the ORIGINAL label of the step that moved — what a UI shows in
 * "Undo <label>" — or `null` when the stack was empty (not an error). The
 * depths come back in the same round trip so menu items can be
 * enabled/disabled without a second call.
 */
export interface HistoryStep {
  label: string | null;
  undoDepth: number;
  redoDepth: number;
}

// ── app events (frozen names, §3.4) ─────────────────────────────────────────

export interface AuraEventMap {
  "transport://state": TransportState;
  "recording://state": RecordingState;
  "sidecar://progress": SidecarProgressEvent;
  "sidecar://done": SidecarDoneEvent;
  "sidecar://error": SidecarErrorEvent;
  "project://changed": Project;
  "mcp://confirm-requested": PendingConfirmation;
  "export://progress": ExportProgressEvent;
  "export://done": ExportDoneEvent;
  "export://error": ExportErrorEvent;
  "loopjam://state": LoopJamStatus;
}

export type AuraEventName = keyof AuraEventMap;
