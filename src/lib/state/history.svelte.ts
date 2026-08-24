import { backend } from "../tauri";
import { projectops } from "./projectops.svelte";
import type { HistoryOverview, HistoryVersionDetail } from "../types/ipc";

class HistoryBrowser {
  private pending: Promise<void> | null = null;
  private selectionRequest = 0;
  overview: HistoryOverview | null = $state(null);
  detail: HistoryVersionDetail | null = $state(null);
  selectedRev: number | null = $state(null);
  loading = $state(false);
  error: string | null = $state(null);

  get available() {
    return Boolean(backend.historyOverview && backend.historyVersion);
  }

  load(): Promise<void> {
    const call = backend.historyOverview;
    if (!call) return Promise.resolve();
    if (this.pending) return this.pending;
    this.pending = this.loadOnce(() => call.call(backend)).finally(() => {
      this.pending = null;
    });
    return this.pending;
  }

  async refresh(): Promise<void> {
    if (this.pending) await this.pending;
    return this.load();
  }

  private async loadOnce(call: () => Promise<HistoryOverview>) {
    this.loading = true;
    this.error = null;
    try {
      this.overview = await call();
      const next = this.overview.versions[0]?.rev ?? null;
      if (this.selectedRev == null || !this.overview.versions.some((v) => v.rev === this.selectedRev)) {
        this.selectedRev = next;
      }
      if (this.selectedRev != null) await this.select(this.selectedRev);
      else this.detail = null;
    } catch (err) {
      this.error = String(err);
    } finally {
      this.loading = false;
    }
  }

  async select(rev: number) {
    if (!backend.historyVersion) return;
    this.selectedRev = rev;
    this.error = null;
    const request = ++this.selectionRequest;
    try {
      const detail = await backend.historyVersion(rev);
      if (request !== this.selectionRequest || this.selectedRev !== rev) return;
      this.detail = detail;
      if (!detail) this.error = `Revision ${rev} is no longer retained.`;
    } catch (err) {
      if (request !== this.selectionRequest || this.selectedRev !== rev) return;
      this.detail = null;
      this.error = String(err);
    }
  }

  /** The selected row, or null. */
  get selected() {
    return this.overview?.versions.find((v) => v.rev === this.selectedRev) ?? null;
  }

  /**
   * `Undo to here` is offered for a selected revision that is on the undo
   * ancestry and is not already the head (walking to the head is a no-op).
   * `onUndoPath` is only a hint — the backend re-validates under its own
   * lock and can still refuse, so `undoToSelected` below still has to
   * handle a real rejection, not treat this as authority.
   */
  get canUndoToSelected() {
    const s = this.selected;
    return Boolean(s?.onUndoPath && this.overview && s.rev !== this.overview.headRev);
  }

  async undoToSelected() {
    const o = this.overview;
    const rev = this.selectedRev;
    if (!o || rev == null || !this.canUndoToSelected) return;
    this.error = null;
    let failure: string | null = null;
    try {
      await projectops.undoTo(rev, o.epoch, o.headRev);
    } catch (err) {
      failure = String(err);
    }
    // ORDER MATTERS: `loadOnce` clears `error` on its way in, so a refused
    // walk's reason is assigned AFTER the refresh, not in a `finally` before
    // it — otherwise the panel refreshes the message away in the same tick.
    await this.refresh();
    if (failure) this.error = failure;
  }
}

export const historyBrowser = new HistoryBrowser();
