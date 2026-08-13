/**
 * Project operations: save / open / new, driven by the TransportBar project
 * menu and the Ctrl+S/O/N shortcuts. Owns the name dialog ("new" creates a
 * blank project, "saveAs" materializes an unsaved session — the backend
 * refuses saveAs once a project is open), the unsaved-changes confirmation,
 * and the store re-pull after the open project changes. Desktop (Tauri) only;
 * demo mode gets an informational toast.
 */

import { backend } from "../tauri";
import { midi } from "./midi.svelte";
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

  async save() {
    if (this.demoOnly()) return;
    if (!project.projectDir) {
      this.dialog = {
        mode: "saveAs",
        name: project.name,
        parentDir: await this.defaultParentDir(),
      };
      return;
    }
    try {
      await backend.saveProject();
      toasts.success("PROJECT SAVED", project.projectDir);
    } catch (err) {
      toasts.error("SAVE FAILED", String(err));
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
      await this.save();
      // Unsaved session: save() turned into a Save As dialog instead —
      // let the user finish that; they can redo the action afterwards.
      if (!project.projectDir) return;
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

  /** Re-pull the full snapshot; `project://changed` alone leaves projectDir stale. */
  private async adopt() {
    await project.reload();
    await midi.init();
  }
}

export const projectops = new ProjectOpsStore();
