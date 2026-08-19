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
import { toasts } from "./toasts.svelte";
import type { MidiOutputPortStatus, MidiPortInfo, MidiRouteStatus } from "../types/ipc";

const POLL_MS_DEFAULT = 500;
/** How often the PORT LISTS are re-enumerated. Deliberately much slower than
 * the status poll: each pass builds and drops a `midir` client (an ALSA-seq
 * client create/destroy), where a status read is a few atomics. Two seconds is
 * below what anyone notices when plugging a device in, and cheap. */
const PORTS_POLL_MS = 2000;

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

  isVirtualTrack(trackId: string): boolean {
    return this.trackRoute(trackId)?.portId === `aura-virtual:track:${trackId}`;
  }

  isVirtualClip(clipId: string): boolean {
    return this.clipOverride(clipId)?.portId === `aura-virtual:clip:${clipId}`;
  }

  async setTrackVirtualOutput(trackId: string, enabled: boolean): Promise<void> {
    try {
      await backend.midiSetTrackVirtualOutput?.(trackId, enabled);
    } catch (err) {
      toasts.error("MIDI OUTPUT FAILED", String(err));
    } finally {
      const status = await backend.midiOutputStatus?.().catch(() => undefined);
      if (status) {
        this.outputs = status.outputs;
        this.routes = status.routes;
      }
    }
  }

  async setClipVirtualOutput(clipId: string, enabled: boolean): Promise<void> {
    try {
      await backend.midiSetClipVirtualOutput?.(clipId, enabled);
    } catch (err) {
      toasts.error("MIDI OUTPUT FAILED", String(err));
    } finally {
      const status = await backend.midiOutputStatus?.().catch(() => undefined);
      if (status) {
        this.outputs = status.outputs;
        this.routes = status.routes;
      }
    }
  }

  /** Both port lists, tolerant of a backend without the optional midi
   * methods (demo mode has no hardware to simulate).
   *
   * A failed or empty enumeration leaves the previous list in place instead of
   * blanking it: this now runs on a timer (see `startPolling`), and one
   * unlucky read must not make every device vanish from the picker mid-click.
   * A device that is genuinely gone still disappears — `list_*_ports` returns
   * the others, so the list shrinks rather than emptying. */
  async loadPorts(): Promise<void> {
    const [inPorts, outPorts] = await Promise.all([
      backend.midiListInputPorts?.().catch(() => []) ?? Promise.resolve([]),
      backend.midiListOutputPorts?.().catch(() => []) ?? Promise.resolve([]),
    ]);
    if (inPorts.length > 0 || this.inPorts.length === 0) this.inPorts = inPorts;
    if (outPorts.length > 0 || this.outPorts.length === 0) this.outPorts = outPorts;
  }

  /** Poll both status endpoints at `intervalMs` (500 ms default) and mirror
   * them into the store, and re-enumerate the PORT LISTS on a slower beat.
   * Returns the stopper.
   *
   * The lists used to be read once when the panel mounted, so a device started
   * after AURA — Carla, Hydrogen, a USB keyboard — never appeared in the picker
   * and the documented workaround was to restart the app. The backend
   * re-enumerates on every call already (`list_output_ports` builds and drops a
   * throwaway `midir` client), so this only ever needed asking again.
   *
   * Slower than the status poll on purpose: enumeration creates and destroys an
   * ALSA-seq client per call, where a status read is a few atomics. */
  startPolling(intervalMs: number = POLL_MS_DEFAULT): () => void {
    const portsId = setInterval(() => {
      void this.loadPorts().catch(() => {});
    }, Math.max(intervalMs, PORTS_POLL_MS));
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
    return () => {
      clearInterval(id);
      clearInterval(portsId);
    };
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

  /** `channel` null (the default) = each note keeps its own MIDI channel. */
  async setTrackRoute(
    trackId: string,
    portId: string | null,
    channel: number | null = null,
  ): Promise<void> {
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

  /** `channel` null (the default) = each note keeps its own MIDI channel. */
  async setClipRoute(
    clipId: string,
    portId: string | null,
    channel: number | null = null,
  ): Promise<void> {
    this.routes = this.routes.filter((r) => !(r.scope === "clip" && r.id === clipId));
    if (portId) this.routes = [...this.routes, { scope: "clip", id: clipId, portId, channel }];
    await backend.midiSetClipRoute?.(clipId, portId, channel);
  }
}

export const midiIo = new MidiIoStore();
