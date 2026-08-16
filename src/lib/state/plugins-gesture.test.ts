/**
 * I-8's frontend half: a knob drag brackets its rAF-batched
 * `plugin_set_param` invokes in `gesture_begin`/`gesture_end`, and the
 * TRAILING batch must land BEFORE the gesture closes — a batch that
 * reaches the backend after `gesture_end` gets its own undo entry and its
 * own project.json write, which is exactly what the gesture exists to
 * collapse.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

let resolveSet: (() => void) | null = null;
const pluginSetParam = vi.fn(
  (_instanceId: string, _changes: { id: number; value: number }[]) =>
    new Promise<void>((res) => {
      resolveSet = res;
    }),
);
const gestureBegin = vi.fn(() => Promise.resolve("gid-plugin"));
const gestureEnd = vi.fn((_id?: string) => Promise.resolve());
const calls: string[] = [];

vi.mock("../tauri", () => ({
  backend: {
    mode: "tauri",
    on: () => () => {},
    pluginGetParams: vi.fn(() => Promise.resolve([])),
    pluginList: vi.fn(() => Promise.resolve({ plugins: [], scanned: true, instances: [] })),
    pluginSetParam: (...a: unknown[]) => {
      calls.push("setParam");
      return pluginSetParam(...(a as [string, { id: number; value: number }[]]));
    },
    gestureBegin: (...a: unknown[]) => {
      calls.push("gestureBegin");
      return gestureBegin(...(a as []));
    },
    gestureEnd: (id?: string) => {
      calls.push("gestureEnd");
      return gestureEnd(id);
    },
  },
}));

const { plugins } = await import("./plugins.svelte");

beforeEach(() => {
  vi.clearAllMocks();
  calls.length = 0;
  resolveSet = null;
  plugins.openInstanceId = "inst-1";
  plugins.params = [
    { id: 7, name: "cutoff", min: 0, max: 1, default: 0, value: 0, steps: 0 },
  ];
  vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
    return setTimeout(() => cb(0), 0) as unknown as number;
  });
  vi.stubGlobal("cancelAnimationFrame", (id: number) => clearTimeout(id));
});

describe("plugin knob gesture", () => {
  it("closes the gesture only after the trailing param batch has landed", async () => {
    plugins.beginParamGesture();
    expect(gestureBegin).toHaveBeenCalledWith("plugin param drag");

    plugins.setParam(7, 0.25);
    plugins.setParam(7, 0.75);
    expect(plugins.params[0].value).toBe(0.75); // optimistic local

    const closing = plugins.endParamGesture();
    // the flush is on the wire; the gesture must NOT be closed yet
    await Promise.resolve();
    expect(calls).toEqual(["gestureBegin", "setParam"]);
    resolveSet?.();
    await closing;
    expect(calls).toEqual(["gestureBegin", "setParam", "gestureEnd"]);
    expect(gestureEnd).toHaveBeenCalledWith("gid-plugin");
    expect(pluginSetParam).toHaveBeenCalledTimes(1);
    expect(pluginSetParam.mock.calls[0][1]).toEqual([{ id: 7, value: 0.75 }]);
  });
});
