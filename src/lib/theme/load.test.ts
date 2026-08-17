import { beforeEach, describe, expect, it, vi } from "vitest";

const listUserThemes = vi.fn();
vi.mock("../tauri", () => ({ backend: { listUserThemes: () => listUserThemes() } }));

const errors: { title: string; lines: string[] }[] = [];
vi.mock("../state/toasts.svelte", () => ({
  toasts: {
    error: (title: string, ...lines: string[]) => void errors.push({ title, lines }),
    info: (title: string, ...lines: string[]) => void errors.push({ title, lines }),
  },
}));

const { theme } = await import("./theme.svelte");
const { loadUserThemes } = await import("./load");

beforeEach(() => {
  errors.length = 0;
  listUserThemes.mockReset();
  theme.setUserThemes([]);
});

describe("loadUserThemes", () => {
  it("installs the themes it could parse", async () => {
    listUserThemes.mockResolvedValue([
      { id: "sunny", raw: JSON.stringify({ name: "Sunny", tokens: { bg1: "#ffffff" } }) },
    ]);
    await loadUserThemes();
    expect(theme.userThemes().map((t) => t.id)).toEqual(["sunny"]);
    expect(theme.userThemes()[0].tokens.bg1).toBe("#ffffff");
  });

  it("keeps the good files when one is broken, and names the broken one", async () => {
    listUserThemes.mockResolvedValue([
      { id: "good", raw: JSON.stringify({ name: "Good" }) },
      { id: "broken", raw: "{not json" },
    ]);
    await loadUserThemes();
    expect(theme.userThemes().map((t) => t.id)).toEqual(["good"]);
    expect(errors).toHaveLength(1);
    expect(errors[0].lines.join(" ")).toMatch(/broken\.json/);
  });

  it("reports dropped keys without dropping the theme", async () => {
    listUserThemes.mockResolvedValue([
      { id: "typo", raw: JSON.stringify({ name: "Typo", tokens: { cyan: "not-a-colour" } }) },
    ]);
    await loadUserThemes();
    expect(theme.userThemes().map((t) => t.id)).toEqual(["typo"]);
    expect(errors).toHaveLength(1);
    expect(errors[0].lines.join(" ")).toMatch(/cyan/);
  });

  it("says nothing when every file is fine", async () => {
    listUserThemes.mockResolvedValue([{ id: "fine", raw: JSON.stringify({ name: "Fine" }) }]);
    await loadUserThemes();
    expect(errors).toEqual([]);
  });

  it("survives a backend that rejects", async () => {
    listUserThemes.mockRejectedValue(new Error("no such command"));
    await expect(loadUserThemes()).resolves.toBeUndefined();
    expect(theme.userThemes()).toEqual([]);
  });
});
