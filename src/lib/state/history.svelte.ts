import { backend } from "../tauri";
import type { HistoryOverview, HistoryVersionDetail } from "../types/ipc";

class HistoryBrowser {
  overview: HistoryOverview | null = $state(null);
  detail: HistoryVersionDetail | null = $state(null);
  selectedRev: number | null = $state(null);
  loading = $state(false);
  error: string | null = $state(null);

  get available() {
    return Boolean(backend.historyOverview && backend.historyVersion);
  }

  async load() {
    if (!backend.historyOverview) return;
    this.loading = true;
    this.error = null;
    try {
      this.overview = await backend.historyOverview();
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
