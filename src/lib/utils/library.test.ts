/**
 * Pure helpers behind the library panel. No DOM, no stores — the vitest
 * environment is "node" (see vitest.config.ts).
 */
import { describe, expect, it } from "vitest";
import { parentDir } from "./library";

describe("parentDir", () => {
  it("walks up a POSIX path", () => {
    expect(parentDir("/home/u/Music/AURA/Library/drums")).toBe("/home/u/Music/AURA/Library");
    expect(parentDir("/home/u/Music/AURA/Library/drums/")).toBe("/home/u/Music/AURA/Library");
  });

  it("walks up a Windows path", () => {
    expect(parentDir("C:\\Users\\u\\Samples\\drums")).toBe("C:\\Users\\u\\Samples");
  });

  it("returns null at a filesystem root, so the UI can hide the up button", () => {
    expect(parentDir("/")).toBeNull();
    expect(parentDir("C:\\")).toBeNull();
    expect(parentDir("")).toBeNull();
  });
});
