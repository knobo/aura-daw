/**
 * Backend adapter. Every frozen IPC command from src-tauri/src/lib.rs is
 * wrapped in one typed method. When the page runs outside Tauri (plain
 * `vite dev` in a browser) the adapter transparently swaps to the demo
 * engine in ./demo.ts, which synthesizes transport, meters, waveform tiles
 * and sidecar jobs so the full UI stays live.
 */

import { invoke, Channel } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  AudioDevice,
  AuraClipsPayload,
  AuraEventMap,
  AuraEventName,
  AutomationClip,
  AutomationLane,
  AutomationMode,
  Binding,
  Clip,
  ChordSpan,
  ClipPlacement,
  ComposerGenerateReply,
  ComposerGenerateRequest,
  ComposerSuggestion,
  Curve,
  InsertSlot,
  SendSlot,
  ModulationSnapshot,
  EvolveOptions,
  ExportCapabilities,
  ExportJobStatus,
  ExportRequest,
  ExtractMelodyReply,
  ExtractMelodyRequest,
  HarmonyView,
  HistoryOverview,
  HistoryStep,
  HistoryVersionDetail,
  HumToSongRequest,
  ImportClipRequest,
  ImportSplitReply,
  InstrumentInfo,
  KeySpan,
  LibraryEntry,
  LoopJamStatus,
  McpPolicy,
  McpStatus,
  MeterEvent,
  MeterFrame,
  MidiClip,
  MidiInputStatus,
  MidiNote,
  MidiOutputStatus,
  MidiPortInfo,
  LaunchBinding,
  LaunchMap,
  LaunchSnapshot,
  OpenSidecarEvent,
  PlayerInfo,
  PlayerSource,
  PluginCatalog,
  PluginCatalogPatch,
  PluginDescriptor,
  PluginInstanceInfo,
  PluginListResult,
  PluginParamChange,
  PluginParamInfo,
  PluginScanStatus,
  PaletteView,
  PasteRequest,
  PasteResult,
  PitchFrameBatch,
  PitchScoreReport,
  PitchTrackView,
  Project,
  ProjectSnapshot,
  SidecarEvent,
  SidecarJobStatus,
  StemSplitRequest,
  TempoEvent,
  TempoMapState,
  TrackState,
  TranscribeRequest,
  TransportState,
  UndoToOutcome,
  UserThemeFile,
  WaveformTileRequest,
  ZynPatch,
} from "./types/ipc";

export type Unsubscribe = () => void;

/** Native file drag over the window (Tauri webview drag-drop events). */
export interface FileDropEvent {
  type: "enter" | "over" | "drop" | "leave";
  /** Absolute paths (enter/drop only). */
  paths: string[];
  /** Window-local position in CSS px, when the webview reports one. */
  x: number;
  y: number;
}

export interface Backend {
  readonly mode: "tauri" | "demo";

  // transport
  transportPlay(): Promise<TransportState>;
  transportStop(): Promise<TransportState>;
  transportSeek(positionSamples: number): Promise<TransportState>;
  /** Set the loop region (samples) and its enabled flag. */
  transportSetLoop(
    enabled: boolean,
    startSamples: number,
    endSamples: number,
  ): Promise<TransportState>;
  /** Stop the transport when the playhead reaches the end of the material. */
  transportSetStopAtEnd(enabled: boolean): Promise<TransportState>;
  setMetronome?(enabled: boolean, gain: number, countInBars: number): Promise<void>;
  getTransportState(): Promise<TransportState>;

  // devices
  listInputDevices(): Promise<AudioDevice[]>;
  listOutputDevices(): Promise<AudioDevice[]>;
  selectInputDevice(id: string): Promise<void>;
  selectOutputDevice(id: string): Promise<void>;

  // recording
  startRecording(trackIds?: string[] | null): Promise<void>;
  stopRecording(): Promise<Clip[]>;

  // meters (Tauri v2 Channel<MeterFrame>)
  subscribeMeters(onFrame: (frame: MeterFrame) => void): Promise<Unsubscribe>;

  // pitch coach — the mic opens on an explicit listen toggle or an open
  // panel, never on track arm (owner ruling R6). Mode changes come back as
  // `pitch://state`; none of these four set frontend state optimistically.
  pitchListenStart(): Promise<void>;
  pitchListenStop(): Promise<void>;
  /** The returned `Unsubscribe` also tells the engine — see the note on
   * `pitch_unsubscribe`; a muted channel is invisible to Rust. */
  subscribePitch(onBatch: (batch: PitchFrameBatch) => void): Promise<Unsubscribe>;
  setRehearseHold(enabled: boolean): Promise<void>;
  /** MIDI track holding the target melody; `null` clears it. */
  pitchSetReference(trackId: string | null): Promise<void>;
  /**
   * Score a recorded take against a MIDI track's melody. Analyses and
   * caches the take's pitch curve on a miss, so this is the only call the
   * report needs — roughly 2 s of control-thread work for a 3-minute take
   * the first time, and effectively free after.
   *
   * Never cached frontend-side either: the report is recomputed so it stays
   * correct after the reference melody or the tolerance changes.
   */
  pitchScore(clipId: string, referenceTrackId: string, toleranceCents: number): Promise<PitchScoreReport>;
  /** The take's stored pitch curve on the timeline, thinned to at most
   * `maxPoints`. Analyses on a miss, same as `pitchScore`. */
  pitchTrack(clipId: string, maxPoints?: number): Promise<PitchTrackView>;
  /** (Re)analyse a clip's audio and write its pitch track; returns the
   * frame count. The only way to give a take recorded before the live fold
   * existed a curve. */
  pitchAnalyzeClip(clipId: string): Promise<number>;
  /** Extract monophonic MIDI melody from an audio clip. */
  pitchExtractMelody(request: ExtractMelodyRequest): Promise<ExtractMelodyReply>;

  // waveform tiles — raw AWTF binary
  getWaveformTile(req: WaveformTileRequest): Promise<ArrayBuffer>;

  // tracks
  addTrack(init?: Partial<TrackState>): Promise<TrackState>;
  removeTrack(trackId: string): Promise<void>;
  getTracks(): Promise<TrackState[]>;
  setTrackGain(trackId: string, gainDb: number): Promise<void>;
  setTrackPan(trackId: string, pan: number): Promise<void>;
  setTrackMute(trackId: string, muted: boolean): Promise<void>;
  setTrackSolo(trackId: string, soloed: boolean): Promise<void>;
  setTrackArm(trackId: string, armed: boolean): Promise<void>;
  setTrackAutomationMode(trackId: string, mode: AutomationMode): Promise<void>;
  /** Rename a track. Trimmed backend-side; an empty name is rejected. */
  setTrackName(trackId: string, name: string): Promise<TrackState>;
  /** Put a track in a lane group, or take it out (`null`). Does NOT keep
   * the group contiguous — no UI path calls this; prefer `arrangeLanes`
   * for anything user-facing, which normalizes runs by construction. */
  setTrackGroup(trackId: string, group: string | null): Promise<TrackState>;
  /** Whole lane arrangement — display order + group per lane — in ONE
   * undoable transaction. `lanes` must name every track exactly once, in
   * the new order; the backend rejects a partial list rather than dropping
   * rows. Returns the tracks in their new order. */
  arrangeLanes(lanes: { trackId: string; group: string | null }[]): Promise<TrackState[]>;

  // project
  /** Create `<parentDir>/<name>.aura`; parentDir defaults to the home dir. */
  createProject(name: string, parentDir?: string | null): Promise<Project>;
  openProject(path: string): Promise<Project>;
  saveProject(): Promise<void>;
  /** First save of an unsaved session: create `<parentDir>/<name>.aura` and persist current content. */
  saveProjectAs(name: string, parentDir: string): Promise<Project>;
  /** Demo mode also serves the current project from here; live mode asks Tauri. */
  getProject(): Promise<Project>;

  // sidecars — submit returns the job UUID; events stream over the Channel
  sidecarSplitStems(
    request: StemSplitRequest,
    onEvent: (e: SidecarEvent) => void,
  ): Promise<string>;
  sidecarTranscribe(
    request: TranscribeRequest,
    onEvent: (e: SidecarEvent) => void,
  ): Promise<string>;
  sidecarJobStatus(jobId: string): Promise<SidecarJobStatus>;
  sidecarCancelJob(jobId: string): Promise<void>;
  sidecarListJobs(): Promise<SidecarJobStatus[]>;

  // ── phase 2 ──

  /** Cold-start control-plane snapshot (tracks + clips + midi + tempo). */
  getProjectState(): Promise<ProjectSnapshot>;
  importAudioClip(request: ImportClipRequest): Promise<Clip>;

  /** Persist an audio clip's timeline position through the channel (Plan E
   * Task 4). Optional — real-engine only, same convention as
   * `seedDemoProject?`; the demo backend keeps clip placement local-only. */
  moveClip?(clipId: string, timelineStartSamples: number): Promise<void>;

  /** Batch placement move — the group-drag counterpart of `moveClip`. Sent
   * ONCE at gesture end, inside a `gestureBegin`/`gestureEnd` boundary, so
   * the whole drag is one undo step. Optional — real-engine only, same
   * convention as `moveClip?`. */
  moveClips?(placements: ClipPlacement[]): Promise<void>;

  /** Build the application/x-aura-clips payload for these clips — a pure
   * backend READ. Selection stays frontend-side; the backend is told WHICH
   * clips, never "what is selected". */
  clipsCopy?(audioClipIds: string[], midiClipIds: string[]): Promise<AuraClipsPayload>;
  /** Paste a payload at `atSamples` as ONE transaction (one undo step). */
  clipsPaste?(request: PasteRequest): Promise<PasteResult>;
  /** OS clipboard text slot (cross-instance copy/paste). */
  osClipboardWriteText?(text: string): Promise<void>;
  osClipboardReadText?(): Promise<string>;
  /** Remove an audio clip from its track. */
  removeClip(clipId: string): Promise<void>;
  /** Open a clip's source audio file in the OS's default application for its
   * file type ("open in external editor" — double-click an audio clip on
   * the timeline). Optional — real backend only, same convention as
   * `libraryAudition?`: the demo backend has no on-disk source to open. */
  openClipInExternalEditor?(clipId: string): Promise<void>;

  /** All automation lanes (points inline — per-project, not per-frame).
   * Optional: the demo backend has no automation. */
  automationGet?(): Promise<AutomationLane[]>;
  /** Upsert one lane; an empty `points` array deletes it. One invoke carries
   * the lane's FULL point set (D-03: an edit gesture, never one invoke per
   * point). Returns the updated lane list. */
  automationSet?(lane: AutomationLane): Promise<AutomationLane[]>;

  /** Full modulation document (curves + bindings + automation clips).
   * Optional: the demo backend has no modulation. */
  modulationGet?(): Promise<ModulationSnapshot>;
  /** Upsert one curve; returns the updated snapshot (D-03). */
  modulationSetCurve?(curve: Curve): Promise<ModulationSnapshot>;
  /** Upsert one binding (`delete: true` removes it); returns the snapshot. */
  modulationSetBinding?(binding: Binding, del?: boolean): Promise<ModulationSnapshot>;
  /** Upsert one automation clip (`delete: true` removes it); returns the snapshot. */
  automationClipSet?(clip: AutomationClip, del?: boolean): Promise<ModulationSnapshot>;

  /** Open a gesture boundary (Plan E Task 14, round-2 §4.4's CLAP-style
   * primitive) — call on `pointerdown` of a fader/pan control. Matching
   * mid-gesture commits (e.g. `setTrackGain`/`setTrackPan`) coalesce
   * backend-side into one history-bound batch; `gestureEnd` closes the
   * boundary. Optional — real-engine only, same convention as `moveClip?`;
   * the demo backend has no history to fold into. */
  gestureBegin?(label: string): Promise<string>;
  /** Close the open gesture boundary (see `gestureBegin`) — call on
   * `pointerup`/`pointercancel`. Pass the id `gestureBegin` returned;
   * a mismatch is a no-op so a late end cannot close a different
   * gesture. Omitting the id keeps the old "close whatever is open"
   * contract. Safe to call even when nothing is open. */
  gestureEnd?(id?: string): Promise<void>;

  /**
   * Seed an empty session with the built-in demo song so the first press of
   * play makes sound (real engine only — the demo backend ships with content).
   */
  seedDemoProject?(): Promise<ProjectSnapshot>;

  /** Undo the most recent history step (Plan E Task 17). `label` is the
   * undone step's own label, or `null` when there was nothing to undo (an
   * empty history is not an error). Optional — real-engine only, same
   * convention as `moveClip?`/`gestureBegin?`; the demo backend has no op
   * log to walk back. */
  undo?(): Promise<HistoryStep>;
  /** Redo the most recently undone step (see `undo`). */
  redo?(): Promise<HistoryStep>;
  /** Read-only retained-version browser; real engine only. */
  historyOverview?(): Promise<HistoryOverview>;
  /** Materialize one retained revision and return a summary, never state. */
  historyVersion?(rev: number): Promise<HistoryVersionDetail | null>;
  /** Walk the undo ancestry back to `targetRev`; real engine only. */
  historyUndoTo?(targetRev: number, expectedEpoch: number, expectedHeadRev: number | null): Promise<UndoToOutcome>;

  // midi (all musical positions are integer ticks at the project ppq)
  setTempoMap(
    ppq: number | null,
    events: TempoEvent[],
    meter?: MeterEvent[] | null,
  ): Promise<TempoMapState>;
  midiAddClip(
    trackId: string,
    name: string | null,
    timelineStartTicks: number,
    lengthTicks: number,
  ): Promise<MidiClip>;
  midiSetNotes(clipId: string, notes: MidiNote[]): Promise<MidiClip>;
  midiSetClipBounds(
    clipId: string,
    timelineStartTicks: number,
    lengthTicks: number,
    contentLengthTicks: number | null,
  ): Promise<MidiClip>;
  midiSetClipPlacement(
    clipId: string,
    transposeSemitones: number | null,
    velocityOffset: number | null,
  ): Promise<MidiClip>;
  midiRenameClip(clipId: string, name: string): Promise<MidiClip>;
  /** Remove a MIDI clip from its track. */
  midiRemoveClip(clipId: string): Promise<void>;
  midiGetClips(): Promise<MidiClip[]>;
  midiImportFile(path: string, trackId?: string | null, atTicks?: number | null): Promise<MidiClip[]>;
  midiExportFile(path: string, clipIds?: string[] | null): Promise<string>;

  // ── the Composer (Plan H1) ──
  // Optional so the demo backend can omit one without a type break; call
  // through `backend.harmonyGet?.(...)`.
  /** The harmony document plus everything the panel draws (the circle, the
   *  per-chord analysis, the menus) — all backend-derived. */
  harmonyGet?(atTicks?: number): Promise<HarmonyView>;
  /** Replace the harmony document. Batch-shaped: one call is one op and one
   *  undo entry, whatever the gesture touched. */
  harmonySet?(keys: KeySpan[], chords: ChordSpan[], atTicks?: number): Promise<HarmonyView>;
  /** Which notes may I play at this tick, and why. */
  composerPalette?(tick: number): Promise<PaletteView>;
  /** Ranked next chords, each carrying its own reasoning. */
  composerSuggest?(atTicks?: number, limit?: number): Promise<ComposerSuggestion[]>;
  /** Write parts as ordinary MIDI clips — one transaction, one undo. */
  composerGenerate?(request: ComposerGenerateRequest): Promise<ComposerGenerateReply>;

  // sampler
  samplerLoadInstrument(sfzPath: string, name?: string | null): Promise<InstrumentInfo>;
  samplerListInstruments(): Promise<InstrumentInfo[]>;
  samplerPreviewNote(instrumentId: string, key: number, velocity: number): Promise<void>;
  /** Held audition: sounds until `samplerPreviewNoteOff`. */
  samplerPreviewNoteOn(instrumentId: string, key: number, velocity: number): Promise<void>;
  samplerPreviewNoteOff(key: number): Promise<void>;
  setTrackInstrument(trackId: string, instrumentId: string | null): Promise<TrackState>;

  // open-kind generation jobs (sidecar-job-v2)
  sidecarRunJob(
    kind: string,
    params: Record<string, unknown>,
    onEvent: (e: OpenSidecarEvent) => void,
  ): Promise<string>;

  // ── phase 3: plugins (frozen surface — PHASE3-PLAN §0) ──

  /** Scan LV2 + CLAP search paths; results are cached backend-side. */
  pluginScan(): Promise<PluginDescriptor[]>;
  pluginList(): Promise<PluginListResult>;
  pluginInstantiate(uid: string): Promise<PluginInstanceInfo>;
  pluginRemove(instanceId: string): Promise<PluginInstanceInfo>;
  pluginGetParams(instanceId: string): Promise<PluginParamInfo[]>;
  /** Batched (D-03): one invoke carries N parameter changes. */
  pluginSetParam(instanceId: string, changes: PluginParamChange[]): Promise<PluginParamInfo[]>;
  /**
   * Play a one-shot note (C3 unless the caller picks another key) through a
   * live plugin instance's bound midi track. Not a project edit.
   */
  pluginPreviewNote(instanceId: string, key: number, velocity: number): Promise<void>;
  pluginPreviewNoteOn(instanceId: string, key: number, velocity: number): Promise<void>;
  pluginPreviewNoteOff(): Promise<void>;
  /** Open the native plugin-owned floating editor for a hosted instance. */
  pluginShowGui?(instanceId: string): Promise<void>;
  /** INTERFACE pref: keep plugin editor windows above AURA. */
  pluginSetGuiOnTop?(enabled: boolean): Promise<void>;

  // ── plugin catalog (machine-global, persistent — additive) ──

  /** Read the full persistent catalog (scan cache + favorites/recents/tags/
   * pinned params). Optional: an older engine build may not have it yet. */
  pluginCatalogGet?(): Promise<PluginCatalog>;
  /** Apply one patch and persist; returns the FULL merged catalog so the
   * frontend never guesses at the result. */
  pluginCatalogUpdate?(patch: PluginCatalogPatch): Promise<PluginCatalog>;
  /** "cache from N ago, M bundles changed" without actually rescanning. */
  pluginScanStatus?(): Promise<PluginScanStatus>;

  // ── insert-FX slots (Plan G1) ──

  /** Add an effect as an insert slot on a track. */
  insertAdd?(trackId: string, uid: string, index?: number): Promise<InsertSlot>;
  /** Remove one insert slot and its plugin instance. */
  insertRemove?(trackId: string, slotId: string): Promise<void>;
  /** Reorder one insert slot on a track. */
  insertReorder?(trackId: string, slotId: string, toIndex: number): Promise<void>;
  /** Set bypass on one insert slot. */
  insertSetBypass?(trackId: string, slotId: string, bypassed: boolean): Promise<void>;

  // ── sends / bus returns (Plan G2) ──

  /** Add a send from `trackId` into the bus `dest`. Unity, post-fader. */
  sendAdd?(trackId: string, dest: string): Promise<SendSlot>;
  /** Remove one send edge. */
  sendRemove?(trackId: string, sendId: string): Promise<void>;
  /** Set a send's amount in dB. A mix change — safe per knob frame. */
  sendSetAmount?(trackId: string, sendId: string, amountDb: number): Promise<void>;
  /** Move a send's tap between post-fader (false) and pre-fader (true). */
  sendSetPreFader?(trackId: string, sendId: string, preFader: boolean): Promise<void>;
  /** Point a track's output at a bus, or back at the master (`null`). A
   * MOVE, not a copy: the track stops reaching the master. */
  trackSetOutput?(trackId: string, output: string | null): Promise<void>;

  // ── wave 1.5 ──

  /**
   * Hum/voice recording → MIDI melody clip (optional AMT accompaniment).
   * Returns the job id; progress/done/error + follow-up apply logs stream
   * over the channel (same envelope as the other sidecar jobs).
   */
  humToSong(
    request: HumToSongRequest,
    onEvent: (e: SidecarEvent) => void,
  ): Promise<string>;

  /** Offline bounce; returns the export job id (export://* events follow). */
  exportSong(request: ExportRequest): Promise<string>;
  exportJobStatus(jobId: string): Promise<ExportJobStatus>;
  /** Which formats are exportable right now (ffmpeg-gated ones included only
   * when ffmpeg is on PATH). */
  exportCapabilities(): Promise<ExportCapabilities>;

  /** Import an audio file AND submit a Demucs stem split on the imported
   * copy in one step; stems auto-land as new tracks on the job's done. */
  importAudioClipSplitStems(
    request: ImportClipRequest,
    onEvent: (e: SidecarEvent) => void,
  ): Promise<ImportSplitReply>;

  /** SPLIT STEMS on a clip ALREADY on the timeline (Task 11 addendum): no
   * re-import, no duplicate clip — the backend resolves `clipId`, submits the
   * Demucs job on its existing project-copy audio, and auto-imports the four
   * stems as new tracks aligned to the clip's current timeline position.
   * Returns the job id; progress/done/error stream over `onEvent`, same
   * envelope as the other sidecar jobs. */
  splitStemsForClip(clipId: string, onEvent: (e: SidecarEvent) => void): Promise<string>;

  // loop-jam (ACE-Step repaint of the loop region, applied at wrap)
  loopjamEvolve(options?: EvolveOptions): Promise<LoopJamStatus>;
  loopjamCancel(): Promise<LoopJamStatus>;
  loopjamStatus(): Promise<LoopJamStatus>;

  // ZynAddSubFX bank patches
  zynListPatches(): Promise<ZynPatch[]>;
  zynLoadPatch(instanceId: string, path: string): Promise<void>;

  /**
   * Native file drags over the window (real app only — the webview swallows
   * HTML5 drag events when Tauri's drag-drop handling is on). Demo mode uses
   * plain browser DnD instead and leaves this undefined.
   */
  onFileDrop?(cb: (e: FileDropEvent) => void): Unsubscribe;

  // ── hardware MIDI input (midi-input-ports slice 1) ──
  // Optional: real backend only (no hardware to simulate in demo mode),
  // same carve-out as hintClipCharacter/registerClip above. UI callers must
  // guard with `backend.midiListInputPorts?.(...)`.
  midiListInputPorts?(): Promise<MidiPortInfo[]>;
  /** `monitor` defaults to true (audible) when omitted and a port is
   * selected; irrelevant when `portId` is null (closing the connection). */
  midiSelectInputPort?(portId: string | null, monitor?: boolean): Promise<void>;
  midiInputStatus?(): Promise<MidiInputStatus>;
  /** Route incoming notes to this track's instrument (slice 2), or null to
   * clear the route back to the slice-1 preview tone. */
  midiSelectInputTrack?(trackId: string | null): Promise<void>;

  // ── hardware MIDI output: clock/sync + per-track/per-clip routing ──
  midiListOutputPorts?(): Promise<MidiPortInfo[]>;
  /** Open an additional output port — does not affect any other port
   * already open. Any number of ports may be open simultaneously. */
  midiOpenOutputPort?(portId: string): Promise<void>;
  midiCloseOutputPort?(portId: string): Promise<void>;
  midiSetOutputClockEnabled?(portId: string, enabled: boolean): Promise<void>;
  /** Route this MIDI track's notes to a port+channel, or `portId: null` to
   * clear the route. A clip's own route (below) always wins over its
   * track's. */
  midiSetTrackRoute?(trackId: string, portId: string | null, channel?: number | null): Promise<void>;
  /** Pick (or `deviceId: null` to clear) the audio-return input on a
   * routed MIDI track. The track must already have a MIDI-out route. */
  midiSetTrackReturn?(trackId: string, deviceId: string | null): Promise<void>;
  /** Route one MIDI clip's notes, overriding its track's route, or
   * `portId: null` to fall back to the track's routing. */
  midiSetClipRoute?(clipId: string, portId: string | null, channel?: number | null): Promise<void>;
  midiSetTrackVirtualOutput?(trackId: string, enabled: boolean): Promise<void>;
  midiSetClipVirtualOutput?(clipId: string, enabled: boolean): Promise<void>;
  midiOutputStatus?(): Promise<MidiOutputStatus>;

  // ── MIDI launch map (note → region/clip) ──
  launchGet?(): Promise<LaunchSnapshot>;
  /** Upsert `binding`, or delete `id` when `binding` is null. */
  launchSet?(binding: LaunchBinding | null, id?: string | null, mapId?: string | null): Promise<LaunchSnapshot>;
  launchSetDrive?(clipId: string, on: boolean, mapId?: string | null): Promise<LaunchSnapshot>;
  launchSetDriveFocus?(clipId: string | null): Promise<void>;
  launchSetMap?(id: string, map: LaunchMap | null): Promise<LaunchSnapshot>;
  launchFire?(id: string, bypass?: boolean): Promise<void>;
  /** Cut whatever is on the launch overlay. Idempotent — safe to call with
   * nothing launched, which is what lets stop-all call it unconditionally. */
  launchStop?(): Promise<void>;
  launchLearnArm?(id: string | null): Promise<void>;
  launchLearnTake?(): Promise<{ note: number; channel: number } | null>;

  // ── players (Plan V — V2, Task 9; Task 13 adds fire/stop/add) ──
  /** Every player in the document. Read-only; fix round 1, Critical 2
   * uses it to resolve a `LaunchTarget::Player` back to its clip, and
   * Task 13's `players.svelte.ts` uses it to mirror the whole registry. */
  playersGet?(): Promise<PlayerInfo[]>;
  /** Fire a player's own playhead from 0 (frozen name, Task 9). */
  playerFire?(playerId: string): Promise<void>;
  /** Cut a player's playhead (frozen name, Task 9) — gate mode's release. */
  playerStop?(playerId: string): Promise<void>;
  /** Create a player — undoable, journaled (frozen name, Task 9). */
  playerAdd?(source: PlayerSource, raw?: boolean, name?: string | null): Promise<PlayerInfo>;

  // ── library & browser (Track E, additive; desktop only) ──
  /** List ONE directory level of the sample library. */
  libraryScan?(dir: string): Promise<LibraryEntry[]>;
  /** Absolute path of the default library dir, created on demand. */
  libraryDefaultRoot?(): Promise<string>;
  /** One-shot audition of a library file. Document-decoupled: no op, no
   * document field, no dirty flag. */
  libraryAudition?(path: string, maxSeconds?: number | null): Promise<void>;
  /** Release whatever the audition path is sounding. */
  libraryAuditionStop?(): Promise<void>;

  // mcp
  mcpGetStatus(): Promise<McpStatus>;
  mcpSetPolicy(policy: McpPolicy): Promise<McpPolicy>;
  mcpConfirmPending(confirmationId: string, approve: boolean): Promise<void>;

  // themes
  /** User themes from `config_dir()/aura/themes/*.json`, contents unparsed. */
  listUserThemes(): Promise<UserThemeFile[]>;
  /** Write a theme file; returns its absolute path. */
  writeUserTheme(id: string, json: string): Promise<string>;
  /** Absolute path of the themes folder, for the preferences dialog. */
  userThemesDir(): Promise<string>;

  // app events (frozen names)
  on<K extends AuraEventName>(
    event: K,
    cb: (payload: AuraEventMap[K]) => void,
  ): Unsubscribe;
}

export const isTauri: boolean =
  typeof window !== "undefined" &&
  ("__TAURI_INTERNALS__" in window || "__TAURI__" in window);

// ── Live implementation ─────────────────────────────────────────────────────

class TauriBackend implements Backend {
  readonly mode = "tauri" as const;

  transportPlay() {
    return invoke<TransportState>("transport_play");
  }
  transportStop() {
    return invoke<TransportState>("transport_stop");
  }
  transportSeek(positionSamples: number) {
    return invoke<TransportState>("transport_seek", { positionSamples });
  }
  transportSetLoop(enabled: boolean, startSamples: number, endSamples: number) {
    return invoke<TransportState>("transport_set_loop", {
      enabled,
      startSamples,
      endSamples,
    });
  }
  transportSetStopAtEnd(enabled: boolean) {
    return invoke<TransportState>("transport_set_stop_at_end", { enabled });
  }
  async setMetronome(enabled: boolean, gain: number, countInBars: number) {
    await invoke("set_metronome", { enabled, gain, countInBars });
  }
  getTransportState() {
    return invoke<TransportState>("get_transport_state");
  }

  listInputDevices() {
    return invoke<AudioDevice[]>("list_input_devices");
  }
  listOutputDevices() {
    return invoke<AudioDevice[]>("list_output_devices");
  }
  async selectInputDevice(id: string) {
    // Rust signature: select_input_device(device_id) → camelCase deviceId.
    await invoke("select_input_device", { deviceId: id });
  }
  async selectOutputDevice(id: string) {
    await invoke("select_output_device", { deviceId: id });
  }

  async startRecording(trackIds: string[] | null = null) {
    await invoke("start_recording", { trackIds });
  }
  stopRecording() {
    return invoke<Clip[]>("stop_recording");
  }

  async subscribeMeters(onFrame: (frame: MeterFrame) => void) {
    const channel = new Channel<MeterFrame>();
    channel.onmessage = onFrame;
    await invoke("subscribe_meters", { channel });
    return () => {
      channel.onmessage = () => {};
    };
  }

  async pitchListenStart() {
    await invoke("pitch_listen_start");
  }
  async pitchListenStop() {
    await invoke("pitch_listen_stop");
  }
  async subscribePitch(onBatch: (batch: PitchFrameBatch) => void) {
    const channel = new Channel<PitchFrameBatch>();
    channel.onmessage = onBatch;
    await invoke("pitch_subscribe", { channel });
    // NOT the meter shape. `subscribeMeters` only mutes its end, which is
    // safe there because it is subscribed once for the app's lifetime; the
    // pitch panel subscribes on every open, and a muted channel still
    // accepts sends, so the engine has to be told or one sink per visit
    // accumulates for the rest of the session.
    return () => {
      channel.onmessage = () => {};
      void invoke("pitch_unsubscribe", { channelId: channel.id }).catch((err) =>
        console.warn("[aura] pitch_unsubscribe failed:", err),
      );
    };
  }
  async setRehearseHold(enabled: boolean) {
    await invoke("set_rehearse_hold", { enabled });
  }
  async pitchSetReference(trackId: string | null) {
    await invoke("pitch_set_reference", { trackId });
  }
  async pitchScore(clipId: string, referenceTrackId: string, toleranceCents: number) {
    return await invoke<PitchScoreReport>("pitch_score", { clipId, referenceTrackId, toleranceCents });
  }
  async pitchTrack(clipId: string, maxPoints?: number) {
    return await invoke<PitchTrackView>("pitch_track", { clipId, maxPoints: maxPoints ?? null });
  }
  async pitchAnalyzeClip(clipId: string) {
    return await invoke<number>("pitch_analyze_clip", { clipId });
  }
  async pitchExtractMelody(request: ExtractMelodyRequest) {
    return await invoke<ExtractMelodyReply>("pitch_extract_melody", { request });
  }

  async getWaveformTile(req: WaveformTileRequest): Promise<ArrayBuffer> {
    const res = await invoke<ArrayBuffer | Uint8Array>("get_waveform_tile", {
      ...req,
    });
    if (res instanceof ArrayBuffer) return res;
    return res.slice().buffer as ArrayBuffer;
  }

  addTrack(init: Partial<TrackState> = {}) {
    return invoke<TrackState>("add_track", { ...init });
  }
  async removeTrack(trackId: string) {
    await invoke("remove_track", { trackId });
  }
  getTracks() {
    return invoke<TrackState[]>("get_tracks");
  }
  async setTrackGain(trackId: string, gainDb: number) {
    await invoke("set_track_gain", { trackId, gainDb });
  }
  async setTrackPan(trackId: string, pan: number) {
    await invoke("set_track_pan", { trackId, pan });
  }
  async setTrackMute(trackId: string, muted: boolean) {
    await invoke("set_track_mute", { trackId, muted });
  }
  async setTrackSolo(trackId: string, soloed: boolean) {
    await invoke("set_track_solo", { trackId, soloed });
  }
  async setTrackArm(trackId: string, armed: boolean) {
    await invoke("set_track_arm", { trackId, armed });
  }
  async setTrackAutomationMode(trackId: string, mode: AutomationMode) {
    await invoke<TrackState[]>("set_track_mix", {
      changes: [{ trackId, automationMode: mode }],
    });
  }
  setTrackName(trackId: string, name: string) {
    return invoke<TrackState>("set_track_name", { trackId, name });
  }
  setTrackGroup(trackId: string, group: string | null) {
    return invoke<TrackState>("set_track_group", { trackId, group });
  }
  arrangeLanes(lanes: { trackId: string; group: string | null }[]) {
    return invoke<TrackState[]>("arrange_lanes", { lanes });
  }

  createProject(name: string, parentDir: string | null = null) {
    // Rust signature: create_project(parent_dir, name). Default the parent
    // to a writable per-user location so "new project" works out of the box.
    return invoke<Project>("create_project", {
      parentDir: parentDir ?? "/tmp/aura-projects",
      name,
    });
  }
  openProject(path: string) {
    return invoke<Project>("open_project", { path });
  }
  async saveProject() {
    await invoke("save_project");
  }
  saveProjectAs(name: string, parentDir: string) {
    return invoke<Project>("save_project_as", { parentDir, name });
  }
  async getProject(): Promise<Project> {
    // No dedicated command in the frozen surface; assemble from parts.
    const [tracks, transport] = await Promise.all([
      this.getTracks(),
      this.getTransportState(),
    ]);
    return {
      schemaVersion: 1,
      name: "Untitled",
      sampleRate: transport.sampleRate,
      tempoBpm: transport.tempoBpm,
      tracks,
      clips: [],
      transport,
    };
  }

  private submitSidecar(
    command: string,
    request: StemSplitRequest | TranscribeRequest,
    onEvent: (e: SidecarEvent) => void,
  ) {
    const channel = new Channel<SidecarEvent>();
    channel.onmessage = onEvent;
    return invoke<string>(command, { request, onEvent: channel });
  }

  sidecarSplitStems(request: StemSplitRequest, onEvent: (e: SidecarEvent) => void) {
    return this.submitSidecar("sidecar_split_stems", request, onEvent);
  }
  sidecarTranscribe(request: TranscribeRequest, onEvent: (e: SidecarEvent) => void) {
    return this.submitSidecar("sidecar_transcribe", request, onEvent);
  }
  sidecarJobStatus(jobId: string) {
    return invoke<SidecarJobStatus>("sidecar_job_status", { jobId });
  }
  async sidecarCancelJob(jobId: string) {
    await invoke("sidecar_cancel_job", { jobId });
  }
  sidecarListJobs() {
    return invoke<SidecarJobStatus[]>("sidecar_list_jobs");
  }

  // ── phase 2 ──

  getProjectState() {
    return invoke<ProjectSnapshot>("get_project_state");
  }
  importAudioClip(request: ImportClipRequest) {
    return invoke<Clip>("import_audio_clip", { request });
  }
  moveClip(clipId: string, timelineStartSamples: number) {
    return invoke<void>("move_clip", { clipId, timelineStartSamples });
  }
  moveClips(placements: ClipPlacement[]) {
    return invoke<void>("move_clips", { placements });
  }
  clipsCopy(audioClipIds: string[], midiClipIds: string[]) {
    return invoke<AuraClipsPayload>("clips_copy", { audioClipIds, midiClipIds });
  }
  clipsPaste(request: PasteRequest) {
    return invoke<PasteResult>("clips_paste", { request });
  }
  osClipboardWriteText(text: string) {
    return invoke<void>("os_clipboard_write_text", { text });
  }
  osClipboardReadText() {
    return invoke<string>("os_clipboard_read_text");
  }
  async removeClip(clipId: string) {
    await invoke("remove_clip", { clipId });
  }
  async openClipInExternalEditor(clipId: string) {
    await invoke("open_clip_in_external_editor", { clipId });
  }
  automationGet() {
    return invoke<AutomationLane[]>("automation_get");
  }
  automationSet(lane: AutomationLane) {
    return invoke<AutomationLane[]>("automation_set", { lane });
  }
  modulationGet() {
    return invoke<ModulationSnapshot>("modulation_get");
  }
  modulationSetCurve(curve: Curve) {
    return invoke<ModulationSnapshot>("modulation_set_curve", { curve });
  }
  modulationSetBinding(binding: Binding, del?: boolean) {
    return invoke<ModulationSnapshot>("modulation_set_binding", { binding, delete: del });
  }
  automationClipSet(clip: AutomationClip, del?: boolean) {
    return invoke<ModulationSnapshot>("automation_clip_set", { clip, delete: del });
  }
  undo() {
    return invoke<HistoryStep>("undo");
  }
  redo() {
    return invoke<HistoryStep>("redo");
  }
  historyOverview() {
    return invoke<HistoryOverview>("history_overview");
  }
  historyUndoTo(targetRev: number, expectedEpoch: number, expectedHeadRev: number | null) {
    return invoke<UndoToOutcome>("history_undo_to", { targetRev, expectedEpoch, expectedHeadRev });
  }
  historyVersion(rev: number) {
    return invoke<HistoryVersionDetail | null>("history_version", { rev });
  }
  async gestureBegin(label: string) {
    return invoke<string>("gesture_begin", { label });
  }
  async gestureEnd(id?: string) {
    if (id == null) return;
    await invoke("gesture_end", { id });
  }
  seedDemoProject() {
    return invoke<ProjectSnapshot>("seed_demo_project");
  }

  setTempoMap(ppq: number | null, events: TempoEvent[], meter?: MeterEvent[] | null) {
    return invoke<TempoMapState>("set_tempo_map", { ppq, events, meter: meter ?? null });
  }
  midiAddClip(trackId: string, name: string | null, timelineStartTicks: number, lengthTicks: number) {
    return invoke<MidiClip>("midi_add_clip", { trackId, name, timelineStartTicks, lengthTicks });
  }
  midiSetNotes(clipId: string, notes: MidiNote[]) {
    return invoke<MidiClip>("midi_set_notes", { clipId, notes });
  }
  midiSetClipBounds(
    clipId: string,
    timelineStartTicks: number,
    lengthTicks: number,
    contentLengthTicks: number | null,
  ) {
    return invoke<MidiClip>("midi_set_clip_bounds", {
      clipId,
      timelineStartTicks,
      lengthTicks,
      contentLengthTicks,
    });
  }
  midiSetClipPlacement(
    clipId: string,
    transposeSemitones: number | null,
    velocityOffset: number | null,
  ) {
    return invoke<MidiClip>("midi_set_clip_placement", {
      clipId,
      transposeSemitones,
      velocityOffset,
    });
  }
  midiRenameClip(clipId: string, name: string) {
    return invoke<MidiClip>("midi_rename_clip", { clipId, name });
  }
  async midiRemoveClip(clipId: string) {
    await invoke("midi_remove_clip", { clipId });
  }
  midiGetClips() {
    return invoke<MidiClip[]>("midi_get_clips");
  }
  midiImportFile(path: string, trackId: string | null = null, atTicks: number | null = null) {
    return invoke<MidiClip[]>("midi_import_file", { path, trackId, atTicks });
  }
  midiExportFile(path: string, clipIds: string[] | null = null) {
    return invoke<string>("midi_export_file", { path, clipIds });
  }

  // ── the Composer (Plan H1) ──
  harmonyGet(atTicks?: number) {
    return invoke<HarmonyView>("harmony_get", { atTicks: atTicks ?? null });
  }
  harmonySet(keys: KeySpan[], chords: ChordSpan[], atTicks?: number) {
    return invoke<HarmonyView>("harmony_set", { keys, chords, atTicks: atTicks ?? null });
  }
  composerPalette(tick: number) {
    return invoke<PaletteView>("composer_palette", { tick });
  }
  composerSuggest(atTicks?: number, limit?: number) {
    return invoke<ComposerSuggestion[]>("composer_suggest", {
      atTicks: atTicks ?? null,
      limit: limit ?? null,
    });
  }
  composerGenerate(request: ComposerGenerateRequest) {
    return invoke<ComposerGenerateReply>("composer_generate", { request });
  }

  samplerLoadInstrument(sfzPath: string, name: string | null = null) {
    return invoke<InstrumentInfo>("sampler_load_instrument", { sfzPath, name });
  }
  samplerListInstruments() {
    return invoke<InstrumentInfo[]>("sampler_list_instruments");
  }
  async samplerPreviewNote(instrumentId: string, key: number, velocity: number) {
    await invoke("sampler_preview_note", { instrumentId, key, velocity });
  }
  async samplerPreviewNoteOn(instrumentId: string, key: number, velocity: number) {
    await invoke("sampler_preview_note_on", { instrumentId, key, velocity });
  }
  async samplerPreviewNoteOff(key: number) {
    await invoke("sampler_preview_note_off", { key });
  }
  setTrackInstrument(trackId: string, instrumentId: string | null) {
    return invoke<TrackState>("set_track_instrument", { trackId, instrumentId });
  }

  sidecarRunJob(
    kind: string,
    params: Record<string, unknown>,
    onEvent: (e: OpenSidecarEvent) => void,
  ) {
    const channel = new Channel<OpenSidecarEvent>();
    channel.onmessage = onEvent;
    return invoke<string>("sidecar_run_job", { kind, params, onEvent: channel });
  }

  // ── phase 3: plugins ──

  pluginScan() {
    return invoke<PluginDescriptor[]>("plugin_scan");
  }
  pluginList() {
    return invoke<PluginListResult>("plugin_list");
  }
  pluginInstantiate(uid: string) {
    return invoke<PluginInstanceInfo>("plugin_instantiate", { uid });
  }
  pluginRemove(instanceId: string) {
    return invoke<PluginInstanceInfo>("plugin_remove", { instanceId });
  }
  pluginGetParams(instanceId: string) {
    return invoke<PluginParamInfo[]>("plugin_get_params", { instanceId });
  }
  pluginSetParam(instanceId: string, changes: PluginParamChange[]) {
    return invoke<PluginParamInfo[]>("plugin_set_param", { instanceId, changes });
  }
  async pluginPreviewNote(instanceId: string, key: number, velocity: number) {
    await invoke("plugin_preview_note", { instanceId, key, velocity });
  }
  async pluginPreviewNoteOn(instanceId: string, key: number, velocity: number) {
    await invoke("plugin_preview_note_on", { instanceId, key, velocity });
  }
  async pluginPreviewNoteOff() {
    await invoke("plugin_preview_note_off");
  }
  async pluginShowGui(instanceId: string) {
    await invoke("plugin_show_gui", { instanceId });
  }
  async pluginSetGuiOnTop(enabled: boolean) {
    await invoke("plugin_set_gui_on_top", { enabled });
  }

  // ── plugin catalog ──

  pluginCatalogGet() {
    return invoke<PluginCatalog>("plugin_catalog_get");
  }
  pluginCatalogUpdate(patch: PluginCatalogPatch) {
    return invoke<PluginCatalog>("plugin_catalog_update", { patch });
  }
  pluginScanStatus() {
    return invoke<PluginScanStatus>("plugin_scan_status");
  }

  // ── insert-FX slots ──

  insertAdd(trackId: string, uid: string, index?: number) {
    return invoke<InsertSlot>("insert_add", { trackId, uid, index });
  }
  insertRemove(trackId: string, slotId: string) {
    return invoke<void>("insert_remove", { trackId, slotId });
  }
  insertReorder(trackId: string, slotId: string, toIndex: number) {
    return invoke<void>("insert_reorder", { trackId, slotId, toIndex });
  }
  insertSetBypass(trackId: string, slotId: string, bypassed: boolean) {
    return invoke<void>("insert_set_bypass", { trackId, slotId, bypassed });
  }

  // ── sends / bus returns ──

  sendAdd(trackId: string, dest: string) {
    return invoke<SendSlot>("send_add", { trackId, dest });
  }
  sendRemove(trackId: string, sendId: string) {
    return invoke<void>("send_remove", { trackId, sendId });
  }
  sendSetAmount(trackId: string, sendId: string, amountDb: number) {
    return invoke<void>("send_set_amount", { trackId, sendId, amountDb });
  }
  sendSetPreFader(trackId: string, sendId: string, preFader: boolean) {
    return invoke<void>("send_set_pre_fader", { trackId, sendId, preFader });
  }
  trackSetOutput(trackId: string, output: string | null) {
    return invoke<void>("track_set_output", { trackId, output });
  }

  // ── wave 1.5 ──

  humToSong(request: HumToSongRequest, onEvent: (e: SidecarEvent) => void) {
    const channel = new Channel<SidecarEvent>();
    channel.onmessage = onEvent;
    return invoke<string>("hum_to_song", { request, onEvent: channel });
  }

  exportSong(request: ExportRequest) {
    return invoke<string>("export_song", { request });
  }
  exportJobStatus(jobId: string) {
    return invoke<ExportJobStatus>("export_job_status", { jobId });
  }
  exportCapabilities() {
    return invoke<ExportCapabilities>("export_capabilities");
  }

  importAudioClipSplitStems(request: ImportClipRequest, onEvent: (e: SidecarEvent) => void) {
    const channel = new Channel<SidecarEvent>();
    channel.onmessage = onEvent;
    return invoke<ImportSplitReply>("import_audio_clip_split_stems", {
      request,
      onEvent: channel,
    });
  }
  splitStemsForClip(clipId: string, onEvent: (e: SidecarEvent) => void) {
    const channel = new Channel<SidecarEvent>();
    channel.onmessage = onEvent;
    return invoke<string>("split_stems_for_clip", { clipId, onEvent: channel });
  }

  loopjamEvolve(options: EvolveOptions = {}) {
    return invoke<LoopJamStatus>("loopjam_evolve", { options });
  }
  loopjamCancel() {
    return invoke<LoopJamStatus>("loopjam_cancel");
  }
  loopjamStatus() {
    return invoke<LoopJamStatus>("loopjam_status");
  }

  zynListPatches() {
    return invoke<ZynPatch[]>("zyn_list_patches");
  }
  async zynLoadPatch(instanceId: string, path: string) {
    await invoke("zyn_load_patch", { instanceId, path });
  }

  onFileDrop(cb: (e: FileDropEvent) => void): Unsubscribe {
    // Webview drag-drop events (Tauri v2). Physical → CSS px for hit tests.
    const scale = () => window.devicePixelRatio || 1;
    const norm = (
      type: FileDropEvent["type"],
      payload: { paths?: string[]; position?: { x: number; y: number } },
    ): FileDropEvent => ({
      type,
      paths: payload.paths ?? [],
      x: (payload.position?.x ?? 0) / scale(),
      y: (payload.position?.y ?? 0) / scale(),
    });
    const subs = [
      listen<{ paths: string[]; position: { x: number; y: number } }>(
        "tauri://drag-enter",
        (e) => cb(norm("enter", e.payload)),
      ),
      listen<{ position: { x: number; y: number } }>("tauri://drag-over", (e) =>
        cb(norm("over", e.payload)),
      ),
      listen<{ paths: string[]; position: { x: number; y: number } }>(
        "tauri://drag-drop",
        (e) => cb(norm("drop", e.payload)),
      ),
      listen("tauri://drag-leave", () => cb(norm("leave", {}))),
    ];
    return () => {
      for (const p of subs) p.then((un) => un()).catch(() => {});
    };
  }

  midiListInputPorts() {
    return invoke<MidiPortInfo[]>("midi_list_input_ports");
  }
  async midiSelectInputPort(portId: string | null, monitor?: boolean) {
    await invoke("midi_select_input_port", { portId, monitor: monitor ?? null });
  }
  midiInputStatus() {
    return invoke<MidiInputStatus>("midi_input_status");
  }
  async midiSelectInputTrack(trackId: string | null) {
    await invoke("midi_select_input_track", { trackId });
  }

  midiListOutputPorts() {
    return invoke<MidiPortInfo[]>("midi_list_output_ports");
  }
  async midiOpenOutputPort(portId: string) {
    await invoke("midi_open_output_port", { portId });
  }
  async midiCloseOutputPort(portId: string) {
    await invoke("midi_close_output_port", { portId });
  }
  async midiSetOutputClockEnabled(portId: string, enabled: boolean) {
    await invoke("midi_set_output_clock_enabled", { portId, enabled });
  }
  async midiSetTrackRoute(trackId: string, portId: string | null, channel?: number | null) {
    await invoke("midi_set_track_route", { trackId, portId, channel: channel ?? null });
  }
  async midiSetTrackReturn(trackId: string, deviceId: string | null) {
    await invoke("midi_set_track_return", { trackId, deviceId });
  }
  async midiSetClipRoute(clipId: string, portId: string | null, channel?: number | null) {
    await invoke("midi_set_clip_route", { clipId, portId, channel: channel ?? null });
  }
  async midiSetTrackVirtualOutput(trackId: string, enabled: boolean) {
    await invoke("midi_set_track_virtual_output", { trackId, enabled });
  }
  async midiSetClipVirtualOutput(clipId: string, enabled: boolean) {
    await invoke("midi_set_clip_virtual_output", { clipId, enabled });
  }
  midiOutputStatus() {
    return invoke<MidiOutputStatus>("midi_output_status");
  }

  launchGet() {
    return invoke<LaunchSnapshot>("launch_get");
  }
  launchSet(binding: LaunchBinding | null, id?: string | null, mapId?: string | null) {
    return invoke<LaunchSnapshot>("launch_set", { binding, id: id ?? null, mapId: mapId ?? null });
  }
  launchSetDrive(clipId: string, on: boolean, mapId?: string | null) {
    return invoke<LaunchSnapshot>("launch_set_drive", { clipId, on, mapId: mapId ?? null });
  }
  async launchSetDriveFocus(clipId: string | null) {
    await invoke("launch_set_drive_focus", { clipId });
  }
  launchSetMap(id: string, map: LaunchMap | null) {
    return invoke<LaunchSnapshot>("launch_set_map", { id, map });
  }
  async launchFire(id: string, bypass?: boolean) {
    await invoke("launch_fire", { id, bypass: bypass ?? false });
  }
  async launchStop() {
    await invoke("launch_stop");
  }
  async launchLearnArm(id: string | null) {
    await invoke("launch_learn_arm", { id });
  }
  launchLearnTake() {
    return invoke<{ note: number; channel: number } | null>("launch_learn_take");
  }

  playersGet() {
    return invoke<PlayerInfo[]>("players_get");
  }
  async playerFire(playerId: string) {
    await invoke("player_fire", { playerId });
  }
  async playerStop(playerId: string) {
    await invoke("player_stop", { playerId });
  }
  playerAdd(source: PlayerSource, raw = false, name: string | null = null) {
    return invoke<PlayerInfo>("player_add", { source, raw, name });
  }

  libraryScan(dir: string) {
    return invoke<LibraryEntry[]>("library_scan", { dir });
  }
  libraryDefaultRoot() {
    return invoke<string>("library_default_root");
  }
  async libraryAudition(path: string, maxSeconds: number | null = null) {
    await invoke("library_audition", { path, maxSeconds });
  }
  async libraryAuditionStop() {
    await invoke("library_audition_stop");
  }

  mcpGetStatus() {
    return invoke<McpStatus>("mcp_get_status");
  }
  mcpSetPolicy(policy: McpPolicy) {
    return invoke<McpPolicy>("mcp_set_policy", { policy });
  }
  async mcpConfirmPending(confirmationId: string, approve: boolean) {
    await invoke("mcp_confirm_pending", { confirmationId, approve });
  }

  listUserThemes() {
    return invoke<UserThemeFile[]>("list_user_themes");
  }
  writeUserTheme(id: string, json: string) {
    return invoke<string>("write_user_theme", { id, json });
  }
  userThemesDir() {
    return invoke<string>("user_themes_dir");
  }

  on<K extends AuraEventName>(event: K, cb: (payload: AuraEventMap[K]) => void) {
    const p = listen<AuraEventMap[K]>(event, (e) => cb(e.payload));
    return () => {
      p.then((unlisten) => unlisten()).catch(() => {});
    };
  }
}

// ── Selection ───────────────────────────────────────────────────────────────

import { DemoBackend } from "./demo";

export const backend: Backend = isTauri ? new TauriBackend() : new DemoBackend();
