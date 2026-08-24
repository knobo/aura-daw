/**
 * Audition targets: the pure half of "what would this row sound like".
 * The store (state/audition.svelte.ts) does the I/O; everything decidable
 * from data alone is decided here, so it can be tested without a backend.
 */
import { describe, expect, it } from "vitest";
import {
  AUDITION_KEY,
  resolveDescriptorTarget,
  resolvePluginInstanceTarget,
} from "./audition-target";
import type { PluginInstanceInfo, TrackState } from "../types/ipc";

const track = (id: string, instrumentId: string | null): TrackState =>
  ({
    id,
    name: id,
    kind: "midi",
    gainDb: 0,
    pan: 0,
    muted: false,
    soloed: false,
    armed: false,
    automationMode: "read",
    color: "#888888",
    instrumentId,
  }) as unknown as TrackState;

const inst = (id: string, uid: string, status = "active"): PluginInstanceInfo =>
  ({ id, uid, name: uid, format: "lv2", status }) as unknown as PluginInstanceInfo;

describe("resolvePluginInstanceTarget", () => {
  it("targets C3 on an instance bound to a midi track", () => {
    const t = [track("t1", "plugin:p1")];
    expect(resolvePluginInstanceTarget("p1", t)).toEqual({
      kind: "pluginInstance",
      instanceId: "p1",
      key: AUDITION_KEY,
    });
  });

  it("refuses an instance that is on no midi track", () => {
    // plugin_preview_note routes through the instance's bound midi track;
    // an insert-only or unplaced instance has no note input at all.
    const t = [track("t1", null)];
    const got = resolvePluginInstanceTarget("p1", t);
    expect(got.kind).toBe("silent");
    if (got.kind === "silent") expect(got.reason).toMatch(/not on a midi track/i);
  });
});

describe("resolveDescriptorTarget", () => {
  it("borrows a live, track-bound instance of the same plugin", () => {
    const instances = [inst("p1", "urn:zyn")];
    const tracks = [track("t1", "plugin:p1")];
    expect(resolveDescriptorTarget("urn:zyn", instances, tracks)).toEqual({
      kind: "pluginInstance",
      instanceId: "p1",
      key: AUDITION_KEY,
    });
  });

  it("ignores an instance of the same plugin that is not track-bound", () => {
    const instances = [inst("p1", "urn:zyn")];
    const tracks = [track("t1", null)];
    expect(resolveDescriptorTarget("urn:zyn", instances, tracks).kind).toBe("silent");
  });

  it("ignores a non-active instance", () => {
    const instances = [inst("p1", "urn:zyn", "crashed")];
    const tracks = [track("t1", "plugin:p1")];
    expect(resolveDescriptorTarget("urn:zyn", instances, tracks).kind).toBe("silent");
  });

  it("refuses a catalog plugin with nothing live, and never proposes an edit", () => {
    // R-3: instantiating to preview would commit Op::PluginAdd. Silence is
    // the correct answer until the backend preview slot exists.
    const got = resolveDescriptorTarget("urn:nothing", [], []);
    expect(got.kind).toBe("silent");
    if (got.kind === "silent") expect(got.reason).toMatch(/no live instance/i);
  });

  it("prefers the first active bound instance when several match", () => {
    const instances = [inst("p1", "urn:zyn", "stub"), inst("p2", "urn:zyn")];
    const tracks = [track("t1", "plugin:p1"), track("t2", "plugin:p2")];
    const got = resolveDescriptorTarget("urn:zyn", instances, tracks);
    expect(got).toEqual({ kind: "pluginInstance", instanceId: "p2", key: AUDITION_KEY });
  });
});
