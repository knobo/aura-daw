/**
 * MIDI I/O store: hardware port lists (in + out), the input routing target
 * track, and hardware MIDI-out state — any number of simultaneously open
 * output ports, each with its own clock toggle, plus a routing table
 * (`routes`) mapping MIDI tracks or individual clips to a port+channel. A
 * clip's own route always wins over its track's. All of it mirrors the
 * backend's app-config state (ruling 1/10: config, not document state — no
 * op, no undo). Thin renderer throughout: every setter is a command
 * emission, nothing computed here feeds a document mutation.
 */

import { backend } from "../tauri";
import type { MidiOutputPortStatus, MidiPortInfo, MidiRouteStatus } from "../types/ipc";

const POLL_MS_DEFAULT = 500;

class MidiIoStore {
  inPorts = $state<MidiPortInfo[]>([]);
  inPortId = $state("");
  monitor = $state(true); // default ON, matches midi_input's backend default
  active = $state(false);
  targetTrackId = $state<string | null>(null);
  routing = $state(false);
  capturing = $state(false);

  outPorts = $state<MidiPortInfo[]>([]);
  /** Currently open output ports, each with its own clock/activity status. */
  outputs = $state<MidiOutputPortStatus[]>([]);
  /** The current track- and clip-scoped routing table. */
  routes = $state<MidiRouteStatus[]>([]);

  /** The route that would win for a given clip: its own override if one
   * exists, otherwise its track's route (or undefined if neither). */
  routeForClip(clipId: string, trackId: string): MidiRouteStatus | undefined {
    return (
      this.routes.find((r) => r.scope === "clip" && r.id === clipId) ??
      this.routes.find((r) => r.scope === "track" && r.id === trackId)
    );
  }

  /** This clip's OWN override, ignoring any track-level fallback — the
   * clip picker needs to distinguish "inheriting from the track" from "no
   * route at all" (the fallback above collapses that distinction). */
  clipOverride(clipId: string): MidiRouteStatus | undefined {
    return this.routes.find((r) => r.scope === "clip" && r.id === clipId);
  }

  trackRoute(trackId: string): MidiRouteStatus | undefined {
    return this.routes.find((r) => r.scope === "track" && r.id === trackId);
  }

  /** Both port lists, tolerant of a backend without the optional midi
   * methods (demo mode has no hardware to simulate). */
  async loadPorts(): Promise<void> {
    const [inPorts, outPorts] = await Promise.all([
      backend.midiListInputPorts?.().catch(() => []) ?? Promise.resolve([]),
      backend.midiListOutputPorts?.().catch(() => []) ?? Promise.resolve([]),
    ]);
    this.inPorts = inPorts;
    this.outPorts = outPorts;
  }

  /** Poll both status endpoints at `intervalMs` (500 ms default) and mirror
   * them into the store. Returns the stopper. */
  startPolling(intervalMs: number = POLL_MS_DEFAULT): () => void {
    const id = setInterval(() => {
      backend.midiInputStatus?.().then((s) => {
        this.inPortId = s.selected?.id ?? "";
        this.active = s.lastEventAgeMs != null && s.lastEventAgeMs < 300;
        if (s.selected) this.monitor = s.monitor;
        this.targetTrackId = s.targetTrackId;
        this.routing = s.routing;
        this.capturing = s.capturing;
      }).catch(() => {});
      backend.midiOutputStatus?.().then((s) => {
        this.outputs = s.outputs;
        this.routes = s.routes;
      }).catch(() => {});
    }, intervalMs);
    return () => clearInterval(id);
  }

  async selectInPort(id: string): Promise<void> {
    this.inPortId = id;
    await backend.midiSelectInputPort?.(id || null, this.monitor);
  }

  async setMonitor(on: boolean): Promise<void> {
    this.monitor = on;
    if (this.inPortId) await backend.midiSelectInputPort?.(this.inPortId, on);
  }

  async setInputTrack(trackId: string | null): Promise<void> {
    this.targetTrackId = trackId;
    await backend.midiSelectInputTrack?.(trackId);
  }

  async openOutPort(id: string): Promise<void> {
    await backend.midiOpenOutputPort?.(id);
  }

  async closeOutPort(id: string): Promise<void> {
    this.outputs = this.outputs.filter((o) => o.port.id !== id);
    await backend.midiCloseOutputPort?.(id);
  }

  async setClockEnabled(portId: string, on: boolean): Promise<void> {
    this.outputs = this.outputs.map((o) => (o.port.id === portId ? { ...o, clockEnabled: on } : o));
    await backend.midiSetOutputClockEnabled?.(portId, on);
  }

  async setTrackRoute(trackId: string, portId: string | null, channel = 0): Promise<void> {
    const prev = this.trackRoute(trackId);
    this.routes = this.routes.filter((r) => !(r.scope === "track" && r.id === trackId));
    if (portId) {
      this.routes = [
        ...this.routes,
        { scope: "track", id: trackId, portId, channel, returnDevice: prev?.returnDevice ?? null },
      ];
    }
    await backend.midiSetTrackRoute?.(trackId, portId, channel);
  }

  async setTrackReturn(trackId: string, deviceId: string | null): Promise<void> {
    this.routes = this.routes.map((r) =>
      r.scope === "track" && r.id === trackId ? { ...r, returnDevice: deviceId } : r,
    );
    await backend.midiSetTrackReturn?.(trackId, deviceId);
  }

  async setClipRoute(clipId: string, portId: string | null, channel = 0): Promise<void> {
    this.routes = this.routes.filter((r) => !(r.scope === "clip" && r.id === clipId));
    if (portId) this.routes = [...this.routes, { scope: "clip", id: clipId, portId, channel }];
    await backend.midiSetClipRoute?.(clipId, portId, channel);
  }
}

export const midiIo = new MidiIoStore();
