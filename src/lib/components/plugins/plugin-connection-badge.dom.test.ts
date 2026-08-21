/**
 * Unbound instruments must be bindable from the connection badge — the
 * same chip the rack, params panel and Zyn patch browser already show.
 *
 * See Global Constraints in
 * docs/superpowers/plans/2026-08-18-dom-test-environment.md.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import type { PluginInstanceInfo, TrackState } from "../../types/ipc";

const setTrackInstrument = vi.fn(async (trackId: string, instrumentId: string | null) => ({
  id: trackId,
  name: "Bass",
  kind: "midi" as const,
  gainDb: 0,
  pan: 0,
  muted: false,
  soloed: false,
  armed: false,
  automationMode: "read" as const,
  color: "#38bdf8",
  instrumentId,
}));

vi.mock("../../tauri", () => ({
  backend: {
    mode: "demo",
    on: () => () => {},
    setTrackInstrument: (...args: [string, string | null]) => setTrackInstrument(...args),
  },
}));

const { default: PluginConnectionBadge } = await import("./PluginConnectionBadge.svelte");
const { plugins } = await import("../../state/plugins.svelte");
const { project } = await import("../../state/project.svelte");

function inst(): PluginInstanceInfo {
  return {
    id: "i1",
    uid: "clap:surge",
    name: "Surge XT",
    format: "clap",
    status: "active",
  };
}

function midi(id: string, name: string, extra: Partial<TrackState> = {}): TrackState {
  return {
    id,
    name,
    kind: "midi",
    gainDb: 0,
    pan: 0,
    muted: false,
    soloed: false,
    armed: false,
    automationMode: "read",
    color: "#38bdf8",
    ...extra,
  };
}

beforeEach(() => {
  setTrackInstrument.mockClear();
  plugins.instances = [inst()];
  project.tracks = [midi("t1", "Bass")];
});

afterEach(() => {
  cleanup();
  plugins.instances = [];
  project.tracks = [];
});

describe("PluginConnectionBadge bind", () => {
  it("offers a bind-to-track control when the instance is unbound", () => {
    render(PluginConnectionBadge, { props: { instanceId: "i1" } });
    expect(screen.getByRole("combobox", { name: /bind to a midi track/i })).toBeTruthy();
  });

  it("binds the instance to the chosen midi track", async () => {
    render(PluginConnectionBadge, { props: { instanceId: "i1" } });
    await fireEvent.change(screen.getByRole("combobox", { name: /bind to a midi track/i }), {
      target: { value: "t1" },
    });
    await waitFor(() =>
      expect(setTrackInstrument).toHaveBeenCalledWith("t1", "plugin:i1"),
    );
  });

  it("shows the bound track instead of a bind control once connected", () => {
    project.tracks = [midi("t1", "Bass", { instrumentId: "plugin:i1" })];
    render(PluginConnectionBadge, { props: { instanceId: "i1" } });
    expect(screen.getByText(/connection: Bass/i)).toBeTruthy();
    expect(screen.queryByRole("combobox", { name: /bind to a midi track/i })).toBeNull();
  });
});
