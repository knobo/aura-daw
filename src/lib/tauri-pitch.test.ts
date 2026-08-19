/**
 * The Tauri backend's pitch subscription, at the IPC seam.
 *
 * This is the test that actually guards the leak: the bus calling its
 * unsubscribe proves nothing, because the old closure only muted
 * `channel.onmessage` and the engine never heard about it. A Tauri
 * `Channel` whose JS side has stopped listening still accepts sends, so
 * `send_batch` kept returning true and the sink was never retired — one
 * more batch per 60 Hz tick for every open of the Pitch Coach.
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

const invoke = vi.fn((_cmd: string, _args?: unknown) => Promise.resolve());

class FakeChannel<T> {
  static next = 1;
  id = FakeChannel.next++;
  onmessage: (v: T) => void = () => {};
}

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invoke(cmd, args),
  Channel: FakeChannel,
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: () => Promise.resolve(() => {}),
}));

// `backend` picks its implementation at import time from this global.
(globalThis as { window?: unknown }).window = { __TAURI_INTERNALS__: {} };
const { backend } = await import("./tauri");

describe("TauriBackend pitch subscription", () => {
  beforeEach(() => invoke.mockClear());

  it("is the Tauri backend, not the demo one", () => {
    expect(backend.mode).toBe("tauri");
  });

  it("retires the subscription in the ENGINE, not just locally", async () => {
    const off = await backend.subscribePitch(() => {});
    const subscribe = invoke.mock.calls.find((c) => c[0] === "pitch_subscribe");
    expect(subscribe).toBeDefined();
    const channel = (subscribe?.[1] as { channel: FakeChannel<unknown> }).channel;

    off();
    const unsubscribe = invoke.mock.calls.find((c) => c[0] === "pitch_unsubscribe");
    expect(unsubscribe, "unsubscribing must reach Rust").toBeDefined();
    // The engine keys sinks by channel id; the wrong id retires someone else.
    expect(unsubscribe?.[1]).toEqual({ channelId: channel.id });
  });

  it("mutes the local handler too, so a late in-flight batch is dropped", async () => {
    const onBatch = vi.fn();
    const off = await backend.subscribePitch(onBatch);
    const channel = (
      invoke.mock.calls.find((c) => c[0] === "pitch_subscribe")?.[1] as {
        channel: FakeChannel<unknown>;
      }
    ).channel;
    off();
    channel.onmessage({ frames: [], deviceRate: 0, listening: false, rehearseHold: false });
    expect(onBatch).not.toHaveBeenCalled();
  });

  it("routes automation mode through the existing batched mix command", async () => {
    await backend.setTrackAutomationMode("t-1", "touch");
    expect(invoke).toHaveBeenCalledWith("set_track_mix", {
      changes: [{ trackId: "t-1", automationMode: "touch" }],
    });
    expect(invoke).not.toHaveBeenCalledWith("set_track_automation_mode", expect.anything());
  });
});
