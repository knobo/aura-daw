/**
 * Lightweight toast queue for completion/failure notices (export done, import
 * landed, evolve applied…). Rendered by components/Toasts.svelte; auto-expires.
 */

export type ToastKind = "success" | "error" | "info";

export interface Toast {
  id: number;
  kind: ToastKind;
  /** One-line headline ("EXPORT COMPLETE"). */
  title: string;
  /** Detail lines (path, duration, size…). */
  lines: string[];
  createdAt: number;
}

const TTL_MS: Record<ToastKind, number> = {
  success: 9000,
  info: 6000,
  error: 14000,
};

class ToastStore {
  list = $state<Toast[]>([]);
  private nextId = 1;

  push(kind: ToastKind, title: string, ...lines: string[]): number {
    const id = this.nextId++;
    this.list = [...this.list, { id, kind, title, lines, createdAt: Date.now() }];
    setTimeout(() => this.dismiss(id), TTL_MS[kind]);
    return id;
  }

  success(title: string, ...lines: string[]) {
    return this.push("success", title, ...lines);
  }
  error(title: string, ...lines: string[]) {
    return this.push("error", title, ...lines);
  }
  info(title: string, ...lines: string[]) {
    return this.push("info", title, ...lines);
  }

  dismiss(id: number) {
    this.list = this.list.filter((t) => t.id !== id);
  }
}

export const toasts = new ToastStore();
