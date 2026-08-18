import { backend } from "../tauri";
import type { HistoryOverview, HistoryVersionDetail } from "../types/ipc";

class HistoryBrowser {
  private pending: Promise<void> | null = null;
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
    try {
      this.detail = await backend.historyVersion(rev);
      if (!this.detail) this.error = `Revision ${rev} is no longer retained.`;
    } catch (err) {
      this.detail = null;
      this.error = String(err);
    }
  }
}

export const historyBrowser = new HistoryBrowser();
