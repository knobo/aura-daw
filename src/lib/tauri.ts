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
  ImportClipRequest,
  InstrumentInfo,
  McpPolicy,
  McpStatus,
  MeterFrame,
  MidiClip,
  MidiNote,
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
} from "./types/ipc";

export type Unsubscribe = () => void;

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
  createProject(name: string): Promise<Project>;
  openProject(path: string): Promise<Project>;
  saveProject(): Promise<void>;
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

  /**
   * Seed an empty session with the built-in demo song so the first press of
   * play makes sound (real engine only — the demo backend ships with content).
   */
  seedDemoProject?(): Promise<ProjectSnapshot>;

  // midi (all musical positions are integer ticks at the project ppq)
  setTempoMap(ppq: number | null, events: TempoEvent[]): Promise<TempoMapState>;
  midiAddClip(
    trackId: string,
    name: string | null,
    timelineStartTicks: number,
    lengthTicks: number,
  ): Promise<MidiClip>;
  midiSetNotes(clipId: string, notes: MidiNote[]): Promise<MidiClip>;
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

  // mcp
  mcpGetStatus(): Promise<McpStatus>;
  mcpSetPolicy(policy: McpPolicy): Promise<McpPolicy>;
  mcpConfirmPending(confirmationId: string, approve: boolean): Promise<void>;

  // app events (frozen names)
  on<K extends AuraEventName>(
    event: K,
    cb: (payload: AuraEventMap[K]) => void,
  ): Unsubscribe;

  /**
   * Demo-only hint so synthetic waveforms match a clip's musical role
   * (drums look spiky, pads swell...). No-op against the real engine.
   */
  hintClipCharacter?(clipId: string, character: string): void;

  /**
   * Demo-only: register a frontend-created clip (stem results) with the
   * synthetic engine so its meters play back. No-op against the real engine.
   */
  registerClip?(clip: Clip): void;
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

  createProject(name: string) {
    return invoke<Project>("create_project", { name });
  }
  openProject(path: string) {
    return invoke<Project>("open_project", { path });
  }
  async saveProject() {
    await invoke("save_project");
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
