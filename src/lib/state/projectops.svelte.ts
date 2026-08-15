/**
 * Project operations: save / open / new, driven by the TransportBar project
 * menu and the Ctrl+S/O/N shortcuts. Owns the name dialog ("new" creates a
 * blank project, "saveAs" materializes an unsaved session — the backend
 * refuses saveAs once a project is open), the unsaved-changes confirmation,
 * and the store re-pull after the open project changes. Desktop (Tauri) only;
 * demo mode gets an informational toast.
 */

import { backend } from "../tauri";
import { automation } from "./automation.svelte";
import { modulation } from "./modulation.svelte";
import { clipEditLoop } from "./clip-edit-loop.svelte";
import { midi } from "./midi.svelte";
import { plugins } from "./plugins.svelte";
import { project } from "./project.svelte";
import { toasts } from "./toasts.svelte";

export interface ProjectDialogState {
  mode: "new" | "saveAs";
  name: string;
  parentDir: string;
}

class ProjectOpsStore {
  /** Name/location dialog (null = closed). */
  dialog = $state<ProjectDialogState | null>(null);
  /** Pending action awaiting the unsaved-changes confirmation. */
  confirm = $state<"new" | "open" | null>(null);
  busy = $state(false);

  /** Seam for tests / non-Tauri: native directory picker. */
  pickDirectory: () => Promise<string | null> = async () => {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const picked = await open({
      directory: true,
      title: "Open AURA project (.aura folder)",
    });
    return typeof picked === "string" ? picked : null;
  };

  /** Seam for tests / non-Tauri: default parent dir for new projects. */
  defaultParentDir: () => Promise<string> = async () => {
    const { homeDir, join } = await import("@tauri-apps/api/path");
    return await join(await homeDir(), "Music", "AURA");
  };

  get available(): boolean {
    return backend.mode === "tauri";
  }

  /** True (and toasts) when running in the browser demo. */
  private demoOnly(): boolean {
    if (this.available) return false;
    toasts.info("DESKTOP ONLY", "Project save/open is available in the desktop app");
    return true;
  }

  private hasContent(): boolean {
    return project.tracks.length > 0 || midi.clips.length > 0;
  }

  /**
   * Returns true only on an actually-completed save — false both on
   * failure and when there was no open project to save (diverted to the
   * Save As dialog instead). Finding 3: `proceedConfirmed` needs this to
   * tell a real save from a failed one before it lets a pending New/Open
   * destroy the unsaved session.
   */
  async save(): Promise<boolean> {
    if (this.demoOnly()) return false;
    if (!project.projectDir) {
      this.dialog = {
        mode: "saveAs",
        name: project.name,
        parentDir: await this.defaultParentDir(),
      };
      return false;
    }
    try {
      await backend.saveProject();
      toasts.success("PROJECT SAVED", project.projectDir);
      return true;
    } catch (err) {
      toasts.error("SAVE FAILED", String(err));
      return false;
    }
  }

  requestNew() {
    if (this.demoOnly()) return;
    if (this.hasContent()) this.confirm = "new";
    else void this.openNewDialog();
  }

  requestOpen() {
    if (this.demoOnly()) return;
    if (this.hasContent()) this.confirm = "open";
    else void this.openViaPicker();
  }

  /** Confirmation answer: optionally save first, then run the pending action. */
  async proceedConfirmed(saveFirst: boolean) {
    const action = this.confirm;
    this.confirm = null;
    if (saveFirst) {
      const saved = await this.save();
      // Finding 3: save() may have FAILED (already toasted "SAVE FAILED"
      // above) or diverted to the Save As dialog (unsaved session) — either
      // way there is no guarantee the session is safely on disk, so the
      // pending New/Open must not proceed and destroy it. Let the user
      // retry the save (or finish the dialog) and redo the action after.
      if (!saved) return;
    }
    if (action === "new") await this.openNewDialog();
    else if (action === "open") await this.openViaPicker();
  }

  cancelConfirm() {
    this.confirm = null;
  }

  async openNewDialog() {
    this.dialog = {
      mode: "new",
      name: "Untitled",
      parentDir: await this.defaultParentDir(),
    };
  }

  async submitDialog() {
    const d = this.dialog;
    if (!d || this.busy) return;
    this.busy = true;
    try {
      if (d.mode === "new") await backend.createProject(d.name, d.parentDir);
      else await backend.saveProjectAs(d.name, d.parentDir);
      this.dialog = null;
      await this.adopt();
      toasts.success(
        d.mode === "new" ? "PROJECT CREATED" : "PROJECT SAVED",
        `${d.parentDir}/${d.name}.aura`,
      );
    } catch (err) {
      // Dialog stays open so the user can fix the name/location.
      toasts.error(d.mode === "new" ? "CREATE FAILED" : "SAVE FAILED", String(err));
    } finally {
      this.busy = false;
    }
  }

  async openViaPicker() {
    const path = await this.pickDirectory();
    if (!path) return;
    try {
      await backend.openProject(path);
      await this.adopt();
      toasts.success("PROJECT OPENED", path);
    } catch (err) {
      toasts.error("OPEN FAILED", String(err));
    }
  }

  /**
   * Undo / redo the last history step (Plan E Task 17). Thin: the backend
   * owns the stacks — this store only invokes and reports. The optional-
   * method pattern (Task 4's precedent) keeps the demo backend, which has
   * no op log, out of it entirely.
   *
   * After the commit lands the backend emits `project://changed`, but that
   * payload does not carry midi — and an undo can restore a MIDI clip — so
   * this re-pulls the same way `adopt` does, minus the loop-edit reset
   * (no project swap happened here).
   */
  async undo() {
    await this.step("undo");
  }

  async redo() {
    await this.step("redo");
  }

  private async step(dir: "undo" | "redo") {
    const call = dir === "undo" ? backend.undo : backend.redo;
    if (!call) return; // demo backend: no op log to walk
    try {
      const step = await call.call(backend);
      if (!step?.label) return; // nothing to undo/redo — silent, not an error
      await this.repull();
      toasts.info(dir === "undo" ? "UNDO" : "REDO", step.label);
    } catch (err) {
      toasts.error(dir === "undo" ? "UNDO FAILED" : "REDO FAILED", String(err));
    }
  }

  /**
   * Re-pull every store an op can address. `project://changed` carries only
   * the `Project` shape, so midi, automation and the plugin registry each
   * need their own pull — M-3 (Plan E whole-branch review): this used to
   * stop after project + midi, leaving automation lanes and an open plugin
   * param panel showing pre-undo values until something else refreshed them.
   */
  private async repull() {
    await project.reload();
    await midi.init();
    await automation.reload();
    await modulation.reload();
    // reloadOpenParams BEFORE refresh: refresh()'s applyInstances closes the
    // param panel when the instance registry no longer lists the open
    // instance, which would otherwise race the re-pull it's meant to serve.
    await plugins.reloadOpenParams();
    await plugins.refresh();
  }

  /** Re-pull the full snapshot; `project://changed` alone leaves projectDir stale. */
  private async adopt() {
    // Finding 8: drop any loop-while-editing state from the OLD project
    // BEFORE pulling the new one in — a non-replaying reset (never
    // `exit()`, which would replay the old snapshot's loop/solo writes onto
    // whatever project just got adopted).
    clipEditLoop.reset();
    await this.repull();
  }
}

export const projectops = new ProjectOpsStore();
