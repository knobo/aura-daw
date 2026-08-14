/**
 * Library & browser panel state (Track E).
 *
 * Chrome only (ADR 0006): every listing comes from `library_scan`, every
 * mutation goes through an existing command via an existing store. The store
 * owns navigation, a per-folder cache, and which file is auditioning.
 *
 * `up()` walking above a configured folder deliberately returns to the ROOTS
 * list rather than to the filesystem parent — the configured folders are what
 * the user chose to see, and browsing out of them sideways is a file manager's
 * job, not a DAW's.
 */

import { backend } from "../tauri";
import { prefs } from "../prefs/prefs.svelte";
import { project } from "./project.svelte";
import { midi } from "./midi.svelte";
import { toasts } from "./toasts.svelte";
import { parentDir, type LibraryDragPayload } from "../utils/library";
import type { LibraryEntry } from "../types/ipc";

export type LibraryRoot = "samples" | "clips" | "presets";

class LibraryStore {
  /** Which of the three roots the panel shows. */
  root = $state<LibraryRoot>("samples");
  /** Absolute path of the backend's default library dir (null until init). */
  defaultRoot = $state<string | null>(null);
  /** Folder being listed; null = the configured-folders list. */
  cwd = $state<string | null>(null);
  entries = $state<LibraryEntry[]>([]);
  loading = $state(false);
  error = $state<string | null>(null);
  /** Path of the file currently auditioning ("" = nothing). */
  auditioning = $state("");

  /** path → listing. In memory, per session; `refresh()` busts one entry. */
  private cache = new Map<string, LibraryEntry[]>();

  /** False in demo mode (no filesystem) — the panel shows a desktop-only note. */
  get available(): boolean {
    return !!backend.libraryScan;
  }

  /** The default library dir first, then the user's folders (no duplicates). */
  get folders(): string[] {
    const configured = prefs.values.librarySampleFolders;
    if (!this.defaultRoot) return [...configured];
    return [this.defaultRoot, ...configured.filter((p) => p !== this.defaultRoot)];
  }

  /** Resolve the default root once. Safe to call on every panel open. */
  async init(): Promise<void> {
    if (this.defaultRoot !== null || !backend.libraryDefaultRoot) return;
    try {
      this.defaultRoot = await backend.libraryDefaultRoot();
      this.error = null;
    } catch (err) {
      this.error = String(err);
    }
  }

  /** List `path` (cached), making it the current folder. */
  async open(path: string): Promise<void> {
    this.cwd = path;
    const hit = this.cache.get(path);
    if (hit) {
      this.entries = hit;
      this.error = null;
      return;
    }
    if (!backend.libraryScan) return;
    this.loading = true;
    try {
      const listing = await backend.libraryScan(path);
      this.cache.set(path, listing);
      this.entries = listing;
      this.error = null;
    } catch (err) {
      this.entries = [];
      this.error = String(err);
    } finally {
      this.loading = false;
    }
  }

  /** Up one level, or back to the roots list when already at a configured one. */
  async up(): Promise<void> {
    const here = this.cwd;
    if (!here) return;
    const parent = this.folders.includes(here) ? null : parentDir(here);
    if (!parent) {
      this.cwd = null;
      this.entries = [];
      return;
    }
    await this.open(parent);
  }

  /** Re-list the current folder, ignoring (and replacing) the cache. */
  async refresh(): Promise<void> {
    const here = this.cwd;
    if (!here) return;
    this.cache.delete(here);
    await this.open(here);
  }

  /** Audition a file — no document coupling; the previous one is cancelled. */
  async audition(path: string): Promise<void> {
    if (!backend.libraryAudition) return;
    this.auditioning = path;
    try {
      await backend.libraryAudition(path, null);
      this.error = null;
    } catch (err) {
      this.auditioning = "";
      this.error = String(err);
    }
  }

  async stopAudition(): Promise<void> {
    this.auditioning = "";
    try {
      await backend.libraryAuditionStop?.();
    } catch {
      // Stopping a preview that is not running is not worth a message.
    }
  }

  /**
   * A library row was dropped on a track. EVERY branch calls an existing,
   * channel-routed command — this store never mutates the document itself, so
   * every drop is undoable for free and adds no op-log exposure.
   */
  async dropOnTrack(
    payload: LibraryDragPayload,
    trackId: string,
    atSamples: number,
  ): Promise<void> {
    const track = project.trackById(trackId);
    if (!track) return;

    switch (payload.kind) {
      case "sampleFile": {
        if (track.kind !== "audio") {
          toasts.info("NEEDS AN AUDIO TRACK", `${payload.name} is audio — drop it on an audio track`);
          return;
        }
        try {
          await backend.importAudioClip({ path: payload.path, trackId, atSamples });
          await project.reload();
        } catch (err) {
          toasts.error("IMPORT FAILED", String(err));
        }
        return;
      }
      case "projectAudioClip": {
        if (track.kind !== "audio") {
          toasts.info("NEEDS AN AUDIO TRACK", "audio clips drop on audio tracks");
          return;
        }
        const clip = project.clips.find((c) => c.id === payload.clipId);
        if (!clip || !project.projectDir) return;
        try {
          // `sourcePath` is relative to the .aura dir; join it with the
          // platform separator rather than string-concatenating.
          const { join } = await import("@tauri-apps/api/path");
          const abs = await join(project.projectDir, clip.sourcePath);
          await backend.importAudioClip({ path: abs, trackId, atSamples });
          await project.reload();
        } catch (err) {
          toasts.error("CLIP COPY FAILED", String(err));
        }
        return;
      }

      case "projectMidiClip": {
        if (track.kind !== "midi") {
          toasts.info("NEEDS A MIDI TRACK", "MIDI clips drop on MIDI tracks");
          return;
        }
        // One gesture = one undo entry, even though the stamp is up to three
        // commands (add + bounds + notes). `fold_ops` keeps non-Set ops in
        // the same batch, and a closed gesture becomes one HistoryEntry.
        project.beginGesture("drag midi clip");
        try {
          await midi.stampClipTo(payload.clipId, trackId, midi.samplesToTicks(atSamples));
        } finally {
          project.endGesture();
        }
        return;
      }

      default:
        return; // remaining kinds land in Task 8
    }
  }

  /** Back to a fresh store — tests only; the app never resets the panel. */
  reset(): void {
    this.root = "samples";
    this.defaultRoot = null;
    this.cwd = null;
    this.entries = [];
    this.loading = false;
    this.error = null;
    this.auditioning = "";
    this.cache.clear();
  }
}

export const library = new LibraryStore();
