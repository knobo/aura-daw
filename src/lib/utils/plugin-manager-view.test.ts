import { describe, expect, it } from "vitest";
import { initialManagerMode } from "./plugin-manager-view";

describe("initialManagerMode (winner spec §5)", () => {
  it("opens split when the project has instances and the user has not chosen", () => {
    expect(initialManagerMode(undefined, 3)).toBe("split");
  });

  it("opens on browse when nothing is instantiated yet", () => {
    expect(initialManagerMode(undefined, 0)).toBe("browse");
  });

  it("keeps a stored choice even when the instance count would pick the other", () => {
    expect(initialManagerMode("browse", 4)).toBe("browse");
    expect(initialManagerMode("rack", 0)).toBe("rack");
    expect(initialManagerMode("split", 0)).toBe("split");
  });
});
