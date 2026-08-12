/**
 * Hand-written TypeScript mirrors of docs/ipc-schemas/*.schema.json.
 * These are the wire shapes for the frozen Tauri command surface —
 * keep field names camelCase, exactly as on the wire.
 */

// ── audio-device.schema.json ────────────────────────────────────────────────

export interface AudioDevice {
  id: string;
  name: string;
  isDefault: boolean;
  maxChannels: number;
  defaultSampleRate: number;
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
}

// ── track-state.schema.json ─────────────────────────────────────────────────

export type TrackKind = "audio" | "midi" | "bus";

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
  /** Present on the stop notification. */
  clips?: Clip[];
}

// ── project.schema.json ─────────────────────────────────────────────────────

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

export interface TempoMapState {
  ppq: number;
  events: TempoEvent[];
}

export interface MidiNote {
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
  /** Sorted by (tick, key). */
  notes: MidiNote[];
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
  /** Absolute path to a wav/flac file. */
  path: string;
  trackId?: string | null;
  atSamples?: number | null;
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
}

export type AuraEventName = keyof AuraEventMap;
