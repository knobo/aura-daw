/**
 * Sampler instrument bank mirror: list, load, per-track binding and note
 * audition (sampler_preview_note — the "can hear it" hook).
 */

import { backend } from "../tauri";
import { project } from "./project.svelte";
import type { InstrumentInfo } from "../types/ipc";

class InstrumentsStore {
  list = $state<InstrumentInfo[]>([]);
  loading = $state(false);
  error = $state<string | null>(null);
  /** Instrument row whose keys are being auditioned (UI highlight). */
  previewingId = $state<string | null>(null);
  /** MIDI key currently held by previewDown, if any. */
  previewingKey = $state<number | null>(null);

  byId(id: string | null | undefined): InstrumentInfo | undefined {
    return id ? this.list.find((i) => i.id === id) : undefined;
  }

  async refresh() {
    this.loading = true;
    try {
      this.list = await backend.samplerListInstruments();
      this.error = null;
    } catch (err) {
      this.error = String(err);
    } finally {
      this.loading = false;
    }
  }

  async load(sfzPath: string, name: string | null = null): Promise<InstrumentInfo | null> {
    try {
      const info = await backend.samplerLoadInstrument(sfzPath, name);
      if (!this.list.some((i) => i.id === info.id)) this.list = [...this.list, info];
      this.error = null;
      return info;
    } catch (err) {
      this.error = String(err);
      return null;
    }
  }

  async assign(trackId: string, instrumentId: string | null) {
    try {
      const track = await backend.setTrackInstrument(trackId, instrumentId);
      project.patchTrackLocal(trackId, { instrumentId: track.instrumentId ?? instrumentId });
      this.error = null;
    } catch (err) {
      this.error = String(err);
    }
  }

  async preview(instrumentId: string, key: number, velocity = 100) {
    this.previewingId = instrumentId;
    try {
      await backend.samplerPreviewNote(instrumentId, key, velocity);
    } catch (err) {
      console.warn("[aura] sampler_preview_note failed:", err);
    } finally {
      setTimeout(() => {
        if (this.previewingId === instrumentId) this.previewingId = null;
      }, 350);
    }
  }

  /** Note-on for hold-to-play. Sounds until `previewUp`. */
  async previewDown(instrumentId: string, key: number, velocity = 100) {
    if (this.previewingKey != null) await this.previewUp();
    this.previewingId = instrumentId;
    this.previewingKey = key;
    try {
      await backend.samplerPreviewNoteOn(instrumentId, key, velocity);
    } catch (err) {
      console.warn("[aura] sampler_preview_note_on failed:", err);
      this.previewingId = null;
      this.previewingKey = null;
    }
  }

  async previewUp() {
    const key = this.previewingKey;
    this.previewingId = null;
    this.previewingKey = null;
    if (key == null) return;
    try {
      await backend.samplerPreviewNoteOff(key);
    } catch (err) {
      console.warn("[aura] sampler_preview_note_off failed:", err);
    }
  }
}

export const instruments = new InstrumentsStore();
