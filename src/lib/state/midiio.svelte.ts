/**
 * MIDI I/O store (slice 2): hardware port lists (in + out), the input
 * routing target track, the output clock toggle and note-out target track,
 * and a status poll mirroring both hubs' app-config state (ruling 1/10:
 * config, not document state — no op, no undo). Thin renderer throughout:
 * every setter is a command emission, nothing computed here feeds a
 * document mutation.
 */

import { backend } from "../tauri";
import type { MidiPortInfo } from "../types/ipc";

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
  outPortId = $state("");
  clockEnabled = $state(false);
  clockRunning = $state(false);
  noteTrackId = $state<string | null>(null);

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
        this.outPortId = s.selected?.id ?? "";
        this.clockEnabled = s.clockEnabled;
        this.clockRunning = s.running;
        this.noteTrackId = s.noteTrackId;
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

  async selectOutPort(id: string): Promise<void> {
    this.outPortId = id;
    await backend.midiSelectOutputPort?.(id || null);
  }

  async setClockEnabled(on: boolean): Promise<void> {
    this.clockEnabled = on;
    await backend.midiSetClockEnabled?.(on);
  }

  async setOutputTrack(trackId: string | null): Promise<void> {
    this.noteTrackId = trackId;
    await backend.midiSelectOutputTrack?.(trackId);
  }
}

export const midiIo = new MidiIoStore();
