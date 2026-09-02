/**
 * The command-emission half of the surface panel: what a widget does when
 * you actually click it. The layout algebra is covered in
 * `utils/control-surface.test.ts`; this mounts the widgets and asserts the
 * backend calls they emit, plus the bind picker that makes an inserted
 * widget usable at all.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { tick } from "svelte";
import type { TrackState } from "../../types/ipc";
import type { SurfaceTarget } from "../../utils/control-surface";

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
    transportSeek: (samples: number) => {
      calls.push(`seek:${samples}`);
      return Promise.resolve();
    },
    pluginGetParams: () => Promise.resolve([]),
    pluginList: () => Promise.resolve({ plugins: [], scanned: true, instances: [] }),
    playersGet: () => Promise.resolve([]),
    playerFire: (id: string) => {
      calls.push(`player_fire:${id}`);
      return Promise.resolve();
    },
    playerStop: (id: string) => {
      calls.push(`player_stop:${id}`);
      return Promise.resolve();
    },
    playerAdd: (source: unknown, raw?: boolean) => {
      calls.push(`player_add:${JSON.stringify(source)}:${raw}`);
      return Promise.resolve({ id: "new-p1", name: "new pad", source, raw: !!raw });
    },
    playerSetTriggerMode: (id: string, mode: string) => {
      calls.push(`player_set_trigger_mode:${id}:${mode}`);
      return Promise.resolve();
    },
    launchSetQuantize: (bindingId: string, quantize: string) => {
      calls.push(`launch_set_quantize:${bindingId}:${quantize}`);
      return Promise.resolve({ maps: launch.maps });
    },
    playerSetQuantize: (id: string, quantize: string) => {
      calls.push(`player_set_quantize:${id}:${quantize}`);
      return Promise.resolve();
    },
    playerSetChokeGroup: (id: string, group: number | null) => {
      calls.push(`player_set_choke_group:${id}:${group}`);
      return Promise.resolve();
    },
    playerSetVelocityToGain: (id: string, depth: number) => {
      calls.push(`player_set_velocity_to_gain:${id}:${depth}`);
      return Promise.resolve();
    },
  },
}));

const AddMenu = (await import("./AddMenu.svelte")).default;
const ChannelStrip = (await import("./ChannelStrip.svelte")).default;
const PadGrid = (await import("./PadGrid.svelte")).default;
const BindPicker = (await import("./BindPicker.svelte")).default;
const SurfacePanel = (await import("./SurfacePanel.svelte")).default;
const { surface } = await import("../../state/surface.svelte");
const { project } = await import("../../state/project.svelte");
const { midi } = await import("../../state/midi.svelte");
const { launch } = await import("../../state/launch.svelte");
const { players } = await import("../../state/players.svelte");
const Rack = (await import("./Rack.svelte")).default;
const { addWidget, deviceById, emptyLayout, rackWidgets, stripWidgets, unboundWidget } =
  await import("../../utils/control-surface");

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

/** A press on a Pad. `onpress` now fires from pointerdown, but the whole
 *  sequence is still sent — the click the browser synthesises afterwards must
 *  not fire it a second time, and this helper is what would catch that. */
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
  players.list = [];
  project.clips = [];
});

afterEach(() => {
  cleanup();
  surface.layout = emptyLayout();
  surface.addOpen = false;
  surface.bindFor = null;
});

describe("the add menu", () => {
  const openMenu = async () =>
    fireEvent.click(screen.getByRole("button", { name: /add a control/i }));

  it("lists the fill recipes, and keeps the devices one level down", async () => {
    render(AddMenu);
    await openMenu();
    expect(screen.getByText("Add all tracks")).toBeTruthy();
    expect(screen.getByText("Add all clips")).toBeTruthy();
    expect(screen.getByText("Add all automations")).toBeTruthy();
    expect(screen.getByText("Clear page")).toBeTruthy();
    // Four devices — and the list is meant to grow — so the root stays short.
    // (The opener names them in its hint; what must not be here is a row
    // that STAMPS one, which is the row whose name starts with the device.)
    expect(screen.queryByRole("menuitem", { name: /^AKAI LPD8/ })).toBeNull();
    expect(screen.getByRole("menuitem", { name: /add rack/i })).toBeTruthy();
  });

  it("drills into the rack list and back out again", async () => {
    render(AddMenu);
    await openMenu();
    await fireEvent.click(screen.getByRole("menuitem", { name: /add rack/i }));
    for (const name of [/akai lpd8/i, /launchpad/i, /mcu 8-strip/i, /nanokontrol/i]) {
      expect(screen.getByRole("menuitem", { name })).toBeTruthy();
    }
    expect(screen.queryByText("Add all tracks")).toBeNull();
    await fireEvent.click(screen.getByRole("menuitem", { name: /^\s*‹\s*RACK\s*$/ }));
    expect(screen.getByText("Add all tracks")).toBeTruthy();
    expect(surface.addOpen).toBe(true);
  });

  it("peels one level on Escape before it closes", async () => {
    render(AddMenu);
    await openMenu();
    await fireEvent.click(screen.getByRole("menuitem", { name: /add rack/i }));
    const menu = screen.getByRole("menu");
    await fireEvent.keyDown(menu, { key: "Escape" });
    expect(screen.getByText("Add all tracks")).toBeTruthy();
    expect(surface.addOpen).toBe(true);
    await fireEvent.keyDown(menu, { key: "Escape" });
    expect(surface.addOpen).toBe(false);
  });

  it("adds a rack to the deck instead of replacing it", async () => {
    surface.layout = addWidget(emptyLayout(), unboundWidget("pad", { label: "KEEP ME" }));
    render(AddMenu);
    await openMenu();
    await fireEvent.click(screen.getByRole("menuitem", { name: /add rack/i }));
    await fireEvent.click(screen.getByRole("menuitem", { name: /akai lpd8/i }));
    // The reported defect: the `+` menu wiped whatever was on the page.
    expect(surface.page.widgets.some((w) => w.label === "KEEP ME")).toBe(true);
    expect(surface.page.widgets.filter((w) => w.deviceId === "lpd8")).toHaveLength(9);
    expect(surface.addOpen).toBe(false);
  });

  it("reopens at the root after a device was picked", async () => {
    render(AddMenu);
    await openMenu();
    await fireEvent.click(screen.getByRole("menuitem", { name: /add rack/i }));
    await fireEvent.click(screen.getByRole("menuitem", { name: /akai lpd8/i }));
    await openMenu();
    expect(screen.getByText("Add all tracks")).toBeTruthy();
  });

  it("empties the deck only when asked to", async () => {
    surface.layout = addWidget(emptyLayout(), unboundWidget("pad"));
    render(AddMenu);
    await openMenu();
    await fireEvent.click(screen.getByRole("menuitem", { name: /clear page/i }));
    expect(surface.page.widgets).toEqual([]);
  });
});

describe("a rack", () => {
  /** An LPD8 on the page, plus the props the panel would hand its faceplate. */
  function lpd8() {
    const device = deviceById("lpd8")!;
    const widgets = rackWidgets(device, surface.context(), surface.page);
    surface.layout = {
      ...surface.layout,
      pages: [{ ...surface.page, widgets }],
    };
    return { device, groupId: widgets[0].groupId!, widgets };
  }

  it("drives the track its knob is bound to", async () => {
    render(Rack, lpd8());
    const knob = screen.getByRole("slider", { name: "Drums" });
    Object.assign(knob, { setPointerCapture: vi.fn(), releasePointerCapture: vi.fn() });
    await fireEvent.pointerDown(knob, { button: 0, clientY: 200, pointerId: 1 });
    await fireEvent.pointerMove(knob, { clientY: 180, pointerId: 1 });
    await fireEvent.pointerUp(knob, { pointerId: 1 });
    await vi.waitFor(() => expect(calls.at(-1)).toBe("end:gid-1"));
    expect(calls[0]).toBe("begin:knob drag");
    expect(calls.filter((c) => c.startsWith("gain:t1:")).length).toBeGreaterThan(0);
  });

  it("comes off the page as one unit", async () => {
    const props = lpd8();
    render(Rack, { ...props, edit: true });
    await fireEvent.click(screen.getByRole("button", { name: /remove akai lpd8/i }));
    expect(surface.page.widgets).toEqual([]);
  });

  it("keeps its pad block: the block has no remove button of its own", () => {
    render(Rack, { ...lpd8(), edit: true });
    expect(screen.getByLabelText("Pad grid")).toBeTruthy();
    expect(screen.queryByTitle("Remove pad grid")).toBeNull();
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
    const widget = {
      ...unboundWidget("padGrid"),
      cells: [{ kind: "clipLaunch" as const, clipId: "c1" }, null, null, null, null, null, null, null],
    };
    render(PadGrid, { widget });
    await pressPad(screen.getByRole("button", { name: /play verse/i }));
    await vi.waitFor(() => expect(calls).toContain("fire:b1"));
    // Fix round 1 regression, caught mutation-testing the Critical-2 fix:
    // `fireClip` briefly delegated straight to `launch.mapClip`, which
    // also focuses/seeks on a HIT — so firing a pad that already has a
    // binding started moving the transport on every press, the same
    // class of defect Task 8 killed on the Rust side. A pad already
    // mapped must only fire, never seek.
    expect(calls.some((c) => c.startsWith("seek:"))).toBe(false);
  });

  it("fires a player cell exactly like a loose player pad, including gate mode", async () => {
    players.list = [
      { id: "p1", name: "KICK", source: { kind: "audioClip", clipId: "c1" }, raw: true, trigger: { mode: "gate" } },
    ];
    const widget = {
      ...unboundWidget("padGrid"),
      cells: [{ kind: "player" as const, playerId: "p1" }, null, null, null, null, null, null, null],
    };
    render(PadGrid, { widget });
    const pad = screen.getByRole("button", { name: "Play KICK" });
    Object.assign(pad, { setPointerCapture: vi.fn(), releasePointerCapture: vi.fn() });
    await fireEvent.pointerDown(pad, { button: 0, pointerId: 1 });
    await fireEvent.pointerUp(pad, { pointerId: 1 });
    // A gate cell must not ALSO fire on the click a pointer pair produces.
    await fireEvent.click(pad);
    expect(calls).toEqual(["player_fire:p1", "player_stop:p1"]);
  });

  it("assigns an empty cell from the picker in edit mode", async () => {
    const widget = unboundWidget("padGrid");
    surface.layout = addWidget(emptyLayout(), widget);
    render(PadGrid, { widget, edit: true });
    // In edit mode the pad opens its cell picker instead of firing.
    await pressPad(screen.getByRole("button", { name: /assign pad 1/i }));
    await fireEvent.click(screen.getByRole("menuitem", { name: /^clip/i }));
    await fireEvent.click(screen.getByRole("menuitem", { name: "Verse" }));
    const cells = surface.page.widgets.find((w) => w.id === widget.id)?.cells;
    expect(cells?.[0]).toEqual({ kind: "clipLaunch", clipId: "c1" });
    expect(calls).toEqual([]); // assigning a cell is layout chrome, not a command
  });

  it("mutes a track from a grid cell, because a cell's options are a loose pad's", async () => {
    // `cellOptions` IS `bindOptions("pad", ctx)`, so a cell can name a track
    // mute — and `isLit` already lights it from the track's state. Without a
    // trackMute arm in `pressCell` the cell reads the mute correctly and does
    // nothing when pressed: lit, live-looking, inert.
    const base = unboundWidget("padGrid");
    const cells = [...(base.cells ?? [])];
    cells[0] = { kind: "trackMute", trackId: "t1" };
    const widget = { ...base, cells };
    surface.layout = addWidget(emptyLayout(), widget);
    render(PadGrid, { widget, edit: false });
    // Named for its track, not "1" — the label arm was missing too, so the
    // cell was both inert AND unlabelled.
    await pressPad(screen.getByRole("button", { name: "Mute Drums" }));
    await vi.waitFor(() => expect(calls).toContain("mute:t1:true"));
  });

  it("adds a pad from an audio clip into a cell, same as a loose pad", async () => {
    const widget = unboundWidget("padGrid");
    surface.layout = addWidget(emptyLayout(), widget);
    project.clips = [
      {
        id: "ac1",
        trackId: "t1",
        name: "KICK.wav",
        sourcePath: "audio/kick.wav",
        sourceChannels: 2,
        sourceSampleRate: 48000,
        sourceLengthSamples: 4800,
        timelineStartSamples: 0,
        offsetSamples: 0,
        lengthSamples: 4800,
        gainDb: 0,
        fadeInSamples: 0,
        fadeOutSamples: 0,
      },
    ];
    render(PadGrid, { widget, edit: true });
    await pressPad(screen.getByRole("button", { name: /assign pad 1/i }));
    await fireEvent.click(screen.getByRole("menuitem", { name: /add pad from clip/i }));
    await fireEvent.click(screen.getByRole("menuitem", { name: "KICK.wav" }));
    await vi.waitFor(() =>
      expect(surface.page.widgets.find((w) => w.id === widget.id)?.cells?.[0]).toEqual({
        kind: "player",
        playerId: "new-p1",
      }),
    );
    expect(calls).toEqual([`player_add:${JSON.stringify({ kind: "audioClip", clipId: "ac1" })}:true`]);
  });
});

describe("the bind picker", () => {
  it("gives an inserted knob somewhere to point", async () => {
    const widget = unboundWidget("knob");
    surface.layout = addWidget(emptyLayout(), widget);
    render(BindPicker, { widget });
    // Root is now one drill tile per group; the flat list runs into the
    // bottom of the window once a project has real content.
    await fireEvent.click(screen.getByRole("menuitem", { name: /^level/i }));
    await fireEvent.click(screen.getByRole("menuitem", { name: /drums — level/i }));
    const bound = surface.page.widgets.find((w) => w.id === widget.id);
    expect(bound?.target).toEqual({ kind: "trackGain", trackId: "t1" });
    expect(bound?.label).toBe("Drums");
    expect(surface.bindFor).toBe(null);
  });

  it("drills into a group and back out again", async () => {
    const widget = unboundWidget("knob");
    surface.layout = addWidget(emptyLayout(), widget);
    render(BindPicker, { widget });
    await fireEvent.click(screen.getByRole("menuitem", { name: /^level/i }));
    expect(screen.getByRole("menuitem", { name: /drums — level/i })).toBeTruthy();
    expect(screen.queryByRole("menuitem", { name: /^pan/i })).toBeNull();
    const menu = screen.getByRole("menu");
    await fireEvent.keyDown(menu, { key: "Escape" });
    // Escape from a group peels one level rather than closing the picker —
    // the root's tiles (LEVEL, PAN, …) are back, not the leaf items.
    expect(screen.getByRole("menuitem", { name: /^level/i })).toBeTruthy();
    expect(screen.getByRole("menuitem", { name: /^pan/i })).toBeTruthy();
    expect(screen.queryByRole("menuitem", { name: /drums — level/i })).toBeNull();
  });

  it("carries the lamp role a lamp needs to act", async () => {
    const widget = unboundWidget("lamp");
    surface.layout = addWidget(emptyLayout(), widget);
    render(BindPicker, { widget });
    await fireEvent.click(screen.getByRole("menuitem", { name: /^solo/i }));
    await fireEvent.click(screen.getByRole("menuitem", { name: "Drums" }));
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

  it("adds a pad from an audio clip, raw ticked, and binds the pad to the new player", async () => {
    const widget = unboundWidget("pad");
    surface.layout = addWidget(emptyLayout(), widget);
    project.clips = [
      {
        id: "ac1",
        trackId: "t1",
        name: "KICK.wav",
        sourcePath: "audio/kick.wav",
        sourceChannels: 2,
        sourceSampleRate: 48000,
        sourceLengthSamples: 4800,
        timelineStartSamples: 0,
        offsetSamples: 0,
        lengthSamples: 4800,
        gainDb: 0,
        fadeInSamples: 0,
        fadeOutSamples: 0,
      },
    ];
    render(BindPicker, { widget });
    await fireEvent.click(screen.getByRole("menuitem", { name: /add pad from clip/i }));
    await fireEvent.click(screen.getByRole("menuitem", { name: "KICK.wav" }));
    // raw defaults to ticked — V-6 is the point of the whole plan. The bind
    // itself lands after `addAudioPlayer`'s await, one tick past the click.
    await vi.waitFor(() =>
      expect(surface.page.widgets.find((w) => w.id === widget.id)?.target).toEqual({
        kind: "player",
        playerId: "new-p1",
      }),
    );
    expect(calls).toEqual([`player_add:${JSON.stringify({ kind: "audioClip", clipId: "ac1" })}:true`]);
  });
});

describe("a player pad", () => {
  // `SurfacePanel`'s mount effect calls `surface.hydrate`, which on its
  // FIRST call this file makes replaces `surface.layout` wholesale (nothing
  // in localStorage yet, so it lands on `emptyLayout()`) — setting the
  // layout before `render` loses the race to it. Render first, then set
  // the layout the same way a bind picker or a live edit would.
  async function renderWithPlayerPad(target: { playerId: string }) {
    render(SurfacePanel);
    surface.layout = addWidget(emptyLayout(), unboundWidget("pad", { target: { kind: "player", ...target } }));
    await tick();
  }

  it("shows a player pad's source and raw flag", async () => {
    players.list = [
      {
        id: "p1",
        name: "KICK",
        source: { kind: "audioClip", clipId: "c1" },
        raw: true,
        trigger: { mode: "oneShot" },
      },
    ];
    await renderWithPlayerPad({ playerId: "p1" });
    expect(screen.getByTestId("pad-p1-source").textContent).toContain("KICK");
    expect(screen.getByTestId("pad-p1-raw")).toBeTruthy();
  });

  it("fires the player on pointerdown and stops it on pointerup in gate mode", async () => {
    players.list = [
      { id: "p1", name: "KICK", source: { kind: "audioClip", clipId: "c1" }, raw: true, trigger: { mode: "gate" } },
    ];
    await renderWithPlayerPad({ playerId: "p1" });
    const pad = screen.getByRole("button", { name: "KICK" });
    Object.assign(pad, { setPointerCapture: vi.fn(), releasePointerCapture: vi.fn() });
    await fireEvent.pointerDown(pad, { button: 0, pointerId: 1 });
    await fireEvent.pointerUp(pad, { pointerId: 1 });
    // A gate pad must not ALSO fire on the click a pointer pair produces —
    // that would double-trigger the very press this test presses once.
    await fireEvent.click(pad);
    expect(calls).toEqual(["player_fire:p1", "player_stop:p1"]);
  });

  // ── Plan V — V3 ─────────────────────────────────────────────────────
  // The pad chips are the ONLY thing that makes quantize, choke groups and
  // velocity depth reachable before V7's inspector. Reachability is what a
  // frontend review owes (STANDING-CONSTRAINTS), so these assert both that
  // the chip is visible in edit mode and that clicking it emits.

  const V3_PAD = {
    id: "p1",
    name: "KICK",
    source: { kind: "audioClip" as const, clipId: "c1" },
    raw: true,
    trigger: { mode: "oneShot" as const },
  };

  it("hides the V3 pad chips outside edit mode", async () => {
    players.list = [V3_PAD];
    surface.setEdit(false);
    await renderWithPlayerPad({ playerId: "p1" });
    expect(screen.queryByTestId("pad-p1-quantize")).toBeNull();
    expect(screen.queryByTestId("pad-p1-choke")).toBeNull();
    expect(screen.queryByTestId("pad-p1-velocity")).toBeNull();
  });

  it("cycles quantize, choke group and velocity depth from the pad", async () => {
    players.list = [V3_PAD];
    surface.setEdit(true);
    await renderWithPlayerPad({ playerId: "p1" });

    // A pad with none of the V3 fields set reads as the V2 defaults, and
    // each chip's first click is the first step past that default.
    expect(screen.getByTestId("pad-p1-quantize").textContent).toContain("Q OFF");
    expect(screen.getByTestId("pad-p1-choke").textContent).toContain("CHK —");
    expect(screen.getByTestId("pad-p1-velocity").textContent).toContain("VEL 100%");

    await fireEvent.click(screen.getByTestId("pad-p1-quantize"));
    await fireEvent.click(screen.getByTestId("pad-p1-choke"));
    await fireEvent.click(screen.getByTestId("pad-p1-velocity"));
    expect(calls).toEqual([
      "player_set_quantize:p1:sixteenth",
      "player_set_choke_group:p1:1",
      "player_set_velocity_to_gain:p1:0.5",
    ]);
    surface.setEdit(false);
  });

  it("cycles the velocity depth from the NEAREST step, not from the first", async () => {
    // The depth is a number the command takes anywhere in 0..=1, so a
    // document written by anything but this chip (an agent, a hand edit)
    // would otherwise fall off the cycle and always jump back to step one.
    players.list = [{ ...V3_PAD, velocityToGain: 0.55 }];
    surface.setEdit(true);
    await renderWithPlayerPad({ playerId: "p1" });
    await fireEvent.click(screen.getByTestId("pad-p1-velocity"));
    expect(calls).toEqual(["player_set_velocity_to_gain:p1:0"]);
    surface.setEdit(false);
  });

  it("wraps the choke group back out of every group", async () => {
    players.list = [{ ...V3_PAD, chokeGroup: 4 }];
    surface.setEdit(true);
    await renderWithPlayerPad({ playerId: "p1" });
    expect(screen.getByTestId("pad-p1-choke").textContent).toContain("CHK 4");
    await fireEvent.click(screen.getByTestId("pad-p1-choke"));
    expect(calls).toEqual(["player_set_choke_group:p1:null"]);
    surface.setEdit(false);
  });

  // ── launch quantize (V3's follow-up) ─────────────────────────────────
  // The owner met this half first: the chips only ever showed on a PLAYER
  // pad, and a pad bound to a launcher row had none.

  async function renderWithTarget(target: SurfaceTarget) {
    render(SurfacePanel);
    surface.layout = addWidget(emptyLayout(), unboundWidget("pad", { target }));
    await tick();
  }

  it("cycles quantize on a pad bound to a launcher row", async () => {
    surface.setEdit(true);
    await renderWithTarget({ kind: "launchBinding", bindingId: "b1" });
    const chip = screen.getByTestId("pad-launchBinding:b1-quantize");
    expect(chip.textContent).toContain("Q OFF");
    await fireEvent.click(chip);
    expect(calls).toEqual(["launch_set_quantize:b1:sixteenth"]);
    surface.setEdit(false);
  });

  it("reads a launcher row's stored division back onto the chip", async () => {
    launch.maps = [
      {
        ...launch.maps[0],
        bindings: [
          { id: "b1", name: "Verse", target: { kind: "clip", clipId: "c1" }, quantize: "quarter" },
        ],
      },
    ] as unknown as typeof launch.maps;
    surface.setEdit(true);
    await renderWithTarget({ kind: "launchBinding", bindingId: "b1" });
    expect(screen.getByTestId("pad-launchBinding:b1-quantize").textContent).toContain("Q 1/4");
    surface.setEdit(false);
  });

  it("writes to the PLAYER when the binding is a migrated player target", async () => {
    // V2 migrated launch bindings onto players, and `launch_fire_from`
    // returns early into `player_fire` for such a binding — so the division
    // that governs the press is the player's. Writing the binding's field
    // here would set a value nothing reads.
    players.list = [
      { id: "p1", name: "KICK", source: { kind: "audioClip", clipId: "c1" }, raw: true },
    ];
    launch.maps = [
      {
        ...launch.maps[0],
        bindings: [{ id: "b1", name: "KICK", target: { kind: "player", playerId: "p1" } }],
      },
    ] as unknown as typeof launch.maps;
    surface.setEdit(true);
    await renderWithTarget({ kind: "launchBinding", bindingId: "b1" });
    await fireEvent.click(screen.getByTestId("pad-launchBinding:b1-quantize"));
    expect(calls).toEqual(["player_set_quantize:p1:sixteenth"]);
    surface.setEdit(false);
  });

  it("hides the launch quantize chip outside edit mode", async () => {
    surface.setEdit(false);
    await renderWithTarget({ kind: "launchBinding", bindingId: "b1" });
    expect(screen.queryByTestId("pad-launchBinding:b1-quantize")).toBeNull();
  });

  it("degrades a dead player id to the pad's own label, never throws", async () => {
    // No matching row in `players.list` — a project switch or a removed
    // player. `launch.clipIdForPlayer`'s degrade-to-null contract, mirrored
    // here: the pad falls back to its own stored label instead of crashing.
    players.list = [];
    render(SurfacePanel);
    // Setting the layout after mount is what would throw, if it throws —
    // a bad lookup inside the template runs during this reactive update,
    // not during `render`'s initial (player-less) empty deck.
    surface.layout = addWidget(
      emptyLayout(),
      unboundWidget("pad", { label: "GONE", target: { kind: "player", playerId: "dead" } }),
    );
    await tick();
    const pad = screen.getByRole("button", { name: "GONE" }) as HTMLButtonElement;
    expect(pad).toBeTruthy();
    // The ledger's ruling, and the half this test used to miss: it must LOOK
    // unbound. Enabled, it is indistinguishable from a working pad and does
    // nothing when pressed — silence, which is the failure this branch keeps
    // producing.
    expect(pad.disabled).toBe(true);
  });
});
