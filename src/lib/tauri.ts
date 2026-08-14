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
  AuraEventMap,
  AuraEventName,
  Clip,
  EvolveOptions,
  ExportCapabilities,
  ExportJobStatus,
  ExportRequest,
  HistoryStep,
  HumToSongRequest,
  ImportClipRequest,
  ImportSplitReply,
  InstrumentInfo,
  LoopJamStatus,
  McpPolicy,
  McpStatus,
  MeterFrame,
  MidiClip,
  MidiInputStatus,
  MidiNote,
  MidiPortInfo,
  OpenSidecarEvent,
  PluginDescriptor,
  PluginInstanceInfo,
  PluginListResult,
  PluginParamChange,
  PluginParamInfo,
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

  /** Open a gesture boundary (Plan E Task 14, round-2 §4.4's CLAP-style
   * primitive) — call on `pointerdown` of a fader/pan control. Matching
   * mid-gesture commits (e.g. `setTrackGain`/`setTrackPan`) coalesce
   * backend-side into one history-bound batch; `gestureEnd` closes the
   * boundary. Optional — real-engine only, same convention as `moveClip?`;
   * the demo backend has no history to fold into. */
  gestureBegin?(label: string): Promise<void>;
  /** Close the open gesture boundary (see `gestureBegin`) — call on
   * `pointerup`/`pointercancel`. Safe to call even when nothing is open
   * (a stray event without a matching `gestureBegin`); the backend treats
   * it as a no-op. */
  gestureEnd?(): Promise<void>;

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

  // midi (all musical positions are integer ticks at the project ppq)
  setTempoMap(ppq: number | null, events: TempoEvent[]): Promise<TempoMapState>;
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
  midiGetClips(): Promise<MidiClip[]>;
  midiImportFile(path: string, trackId?: string | null, atTicks?: number | null): Promise<MidiClip[]>;
  midiExportFile(path: string, clipIds?: string[] | null): Promise<string>;

  // sampler
  samplerLoadInstrument(sfzPath: string, name?: string | null): Promise<InstrumentInfo>;
  samplerListInstruments(): Promise<InstrumentInfo[]>;
  samplerPreviewNote(instrumentId: string, key: number, velocity: number): Promise<void>;
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

  // mcp
  mcpGetStatus(): Promise<McpStatus>;
  mcpSetPolicy(policy: McpPolicy): Promise<McpPolicy>;
  mcpConfirmPending(confirmationId: string, approve: boolean): Promise<void>;

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
  undo() {
    return invoke<HistoryStep>("undo");
  }
  redo() {
    return invoke<HistoryStep>("redo");
  }
  async gestureBegin(label: string) {
    await invoke("gesture_begin", { label });
  }
  async gestureEnd() {
    await invoke("gesture_end");
  }
  seedDemoProject() {
    return invoke<ProjectSnapshot>("seed_demo_project");
  }

  setTempoMap(ppq: number | null, events: TempoEvent[]) {
    return invoke<TempoMapState>("set_tempo_map", { ppq, events });
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
  midiGetClips() {
    return invoke<MidiClip[]>("midi_get_clips");
  }
  midiImportFile(path: string, trackId: string | null = null, atTicks: number | null = null) {
    return invoke<MidiClip[]>("midi_import_file", { path, trackId, atTicks });
  }
  midiExportFile(path: string, clipIds: string[] | null = null) {
    return invoke<string>("midi_export_file", { path, clipIds });
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

  mcpGetStatus() {
    return invoke<McpStatus>("mcp_get_status");
  }
  mcpSetPolicy(policy: McpPolicy) {
    return invoke<McpPolicy>("mcp_set_policy", { policy });
  }
  async mcpConfirmPending(confirmationId: string, approve: boolean) {
    await invoke("mcp_confirm_pending", { confirmationId, approve });
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
