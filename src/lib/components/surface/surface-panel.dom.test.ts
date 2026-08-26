/**
 * The command-emission half of the surface panel: what a widget does when
 * you actually click it. The layout algebra is covered in
 * `utils/control-surface.test.ts`; this mounts the widgets and asserts the
 * backend calls they emit, plus the bind picker that makes an inserted
 * widget usable at all.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import type { TrackState } from "../../types/ipc";

const calls: string[] = [];

vi.mock("../../tauri", () => ({
  backend: {
    mode: "tauri",
    on: () => () => {},
    gestureBegin: (label: string) => {
      calls.push(`begin:${label}`);
      return Promise.resolve("gid-1");
    },
    gestureEnd: (id?: string) => {
      calls.push(`end:${id}`);
      return Promise.resolve();
    },
    setTrackGain: (id: string, db: number) => {
      calls.push(`gain:${id}:${db}`);
      return Promise.resolve();
    },
    setTrackPan: (id: string, pan: number) => {
      calls.push(`pan:${id}:${pan}`);
      return Promise.resolve();
    },
    setTrackMute: (id: string, muted: boolean) => {
      calls.push(`mute:${id}:${muted}`);
      return Promise.resolve();
    },
    setTrackSolo: (id: string, soloed: boolean) => {
      calls.push(`solo:${id}:${soloed}`);
      return Promise.resolve();
    },
    launchFire: (id: string) => {
      calls.push(`fire:${id}`);
      return Promise.resolve();
    },
    pluginGetParams: () => Promise.resolve([]),
    pluginList: () => Promise.resolve({ plugins: [], scanned: true, instances: [] }),
  },
}));

const AddMenu = (await import("./AddMenu.svelte")).default;
const ChannelStrip = (await import("./ChannelStrip.svelte")).default;
const PadGrid = (await import("./PadGrid.svelte")).default;
const BindPicker = (await import("./BindPicker.svelte")).default;
const { surface } = await import("../../state/surface.svelte");
const { project } = await import("../../state/project.svelte");
const { midi } = await import("../../state/midi.svelte");
const { launch } = await import("../../state/launch.svelte");
const { emptyLayout, addWidget, stripWidgets, unboundWidget } = await import(
  "../../utils/control-surface"
);

const TRACK: TrackState = {
  id: "t1",
  name: "Drums",
  kind: "midi",
  color: "#8ac",
  gainDb: 0,
  pan: 0,
  muted: false,
  soloed: false,
  armed: false,
  automationMode: "off",
};

const CLIP = { id: "c1", name: "Verse", trackId: "t1" };

/** A click on a Pad: the pointer pair for the visual, then the click that fires. */
async function pressPad(el: HTMLElement) {
  Object.assign(el, { setPointerCapture: vi.fn(), releasePointerCapture: vi.fn() });
  await fireEvent.pointerDown(el, { button: 0, pointerId: 1 });
  await fireEvent.pointerUp(el, { pointerId: 1 });
  await fireEvent.click(el);
}

beforeEach(() => {
  calls.length = 0;
  surface.layout = emptyLayout();
  surface.addOpen = false;
  surface.bindFor = null;
  project.tracks = [TRACK];
  midi.clips = [CLIP] as unknown as typeof midi.clips;
  // `bindings` is a getter over the active map.
  launch.maps = [
    {
      ...launch.maps[0],
      bindings: [{ id: "b1", name: "Verse", target: { kind: "clip", clipId: "c1" } }],
    },
  ] as unknown as typeof launch.maps;
  launch.activeMapId = launch.maps[0].id;
});

afterEach(() => {
  cleanup();
  surface.layout = emptyLayout();
  surface.addOpen = false;
  surface.bindFor = null;
});

describe("the add menu", () => {
  it("lists the fill recipes and the LPD8 template", async () => {
    render(AddMenu);
    await fireEvent.click(screen.getByRole("button", { name: /add a control/i }));
    expect(screen.getByText("Add all tracks")).toBeTruthy();
    expect(screen.getByText("Add all clips")).toBeTruthy();
    expect(screen.getByText("Add all automations")).toBeTruthy();
    expect(screen.getByText("AKAI LPD8")).toBeTruthy();
  });

  it("stamps a blank page from the template item", async () => {
    render(AddMenu);
    await fireEvent.click(screen.getByRole("button", { name: /add a control/i }));
    await fireEvent.click(screen.getByRole("menuitem", { name: /blank page/i }));
    expect(surface.page.templateId).toBe("blank");
    expect(surface.page.widgets).toEqual([]);
    expect(surface.addOpen).toBe(false);
  });
});

describe("a channel strip", () => {
  it("mutes and solos the track it is bound to", async () => {
    render(ChannelStrip, { widgets: stripWidgets(TRACK), track: TRACK });
    await fireEvent.click(screen.getByRole("button", { name: /drums mute/i }));
    await fireEvent.click(screen.getByRole("button", { name: /drums solo/i }));
    await vi.waitFor(() => expect(calls).toContain("mute:t1:true"));
    expect(calls).toContain("solo:t1:true");
  });

  it("brackets a fader drag in one gesture, writes inside it", async () => {
    render(ChannelStrip, { widgets: stripWidgets(TRACK), track: TRACK });
    const fader = screen.getByRole("slider", { name: /drums level/i });
    Object.assign(fader, { setPointerCapture: vi.fn(), releasePointerCapture: vi.fn() });
    await fireEvent.pointerDown(fader, { button: 0, clientY: 200, pointerId: 1 });
    await fireEvent.pointerMove(fader, { clientY: 180, pointerId: 1 });
    await fireEvent.pointerUp(fader, { pointerId: 1 });
    await vi.waitFor(() => expect(calls.at(-1)).toBe("end:gid-1"));
    expect(calls[0]).toBe("begin:gain drag");
    // begin, exactly one end, and every gain write between the two
    expect(calls.filter((c) => c.startsWith("end:"))).toHaveLength(1);
    expect(calls.filter((c) => c.startsWith("gain:")).length).toBeGreaterThan(0);
  });
});

describe("a pad grid", () => {
  it("fires the clip in a filled cell", async () => {
    const widget = { ...unboundWidget("padGrid"), cells: ["c1", null, null, null, null, null, null, null] };
    render(PadGrid, { widget });
    await pressPad(screen.getByRole("button", { name: /play verse/i }));
    await vi.waitFor(() => expect(calls).toContain("fire:b1"));
  });

  it("assigns an empty cell from the picker in edit mode", async () => {
    const widget = unboundWidget("padGrid");
    surface.layout = addWidget(emptyLayout(), widget);
    render(PadGrid, { widget, edit: true });
    // In edit mode the pad opens its cell picker instead of firing.
    await pressPad(screen.getByRole("button", { name: /assign pad 1/i }));
    await fireEvent.click(screen.getByRole("menuitem", { name: "Verse" }));
    const cells = surface.page.widgets.find((w) => w.id === widget.id)?.cells;
    expect(cells?.[0]).toBe("c1");
    expect(calls).toEqual([]); // assigning a cell is layout chrome, not a command
  });
});

describe("the bind picker", () => {
  it("gives an inserted knob somewhere to point", async () => {
    const widget = unboundWidget("knob");
    surface.layout = addWidget(emptyLayout(), widget);
    render(BindPicker, { widget });
    await fireEvent.click(screen.getByRole("menuitem", { name: /drums — level/i }));
    const bound = surface.page.widgets.find((w) => w.id === widget.id);
    expect(bound?.target).toEqual({ kind: "trackGain", trackId: "t1" });
    expect(bound?.label).toBe("Drums");
    expect(surface.bindFor).toBe(null);
  });

  it("carries the lamp role a lamp needs to act", async () => {
    const widget = unboundWidget("lamp");
    surface.layout = addWidget(emptyLayout(), widget);
    render(BindPicker, { widget });
    await fireEvent.click(screen.getAllByRole("menuitem", { name: "Drums" })[1]); // SOLO group
    const bound = surface.page.widgets.find((w) => w.id === widget.id);
    expect(bound?.target).toEqual({ kind: "trackSolo", trackId: "t1" });
    expect(bound?.lampRole).toBe("solo");
  });

  it("unbinds a bound widget", async () => {
    const widget = unboundWidget("gauge", { target: { kind: "meter", trackId: "t1" } });
    surface.layout = addWidget(emptyLayout(), widget);
    render(BindPicker, { widget });
    await fireEvent.click(screen.getByRole("menuitem", { name: /unbind/i }));
    expect(surface.page.widgets.find((w) => w.id === widget.id)?.target).toBe(null);
  });
});
