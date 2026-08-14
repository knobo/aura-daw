/**
 * Native directory picker, behind a replaceable seam so components stay
 * testable outside Tauri. Mirrors `projectops.pickDirectory`'s idiom (a
 * replaceable member, not a mutable export) — that one stays where it is;
 * this module exists for callers that are not the project flow.
 */
export const directoryPicker = {
  async pick(title: string): Promise<string | null> {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const picked = await open({ directory: true, title });
    return typeof picked === "string" ? picked : null;
  },
};
