/**
 * Project operations (save / open / new): the orchestration between the
 * backend project commands, the confirm-unsaved guard, the name dialog, and
 * the store re-pull after switching projects. These tests pin the flows the
 * TransportBar project menu and the Ctrl+S/O/N shortcuts drive.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";

const invokes = {
  saveProject: vi.fn(() => Promise.resolve()),
  saveProjectAs: vi.fn((_name: string, _parentDir: string) =>
    Promise.resolve({ name: "x" }),
  ),
  createProject: vi.fn((_name: string, _parentDir?: string | null) =>
    Promise.resolve({ name: "x" }),
  ),
  openProject: vi.fn((_path: string) => Promise.resolve({ name: "x" })),
  undo: vi.fn(
    (): Promise<{ label: string | null; undoDepth: number; redoDepth: number }> =>
      Promise.resolve({ label: "set gain", undoDepth: 0, redoDepth: 1 }),
  ),
  redo: vi.fn(
    (): Promise<{ label: string | null; undoDepth: number; redoDepth: number }> =>
      Promise.resolve({ label: "set gain", undoDepth: 1, redoDepth: 0 }),
  ),
  getProjectState: vi.fn(() =>
    Promise.resolve({
      projectName: "Loaded",
      projectDir: "/home/u/Music/AURA/Loaded.aura",
      transport: { sampleRate: 48000, tempoBpm: 120 },
      tracks: [],
      clips: [],
      ppq: 960,
      tempoEvents: [{ tick: 0, bpm: 120 }],
      midiClips: [],
    }),
  ),
};

const mockBackend = {
  mode: "tauri" as "tauri" | "demo",
  on: () => () => {},
  ...invokes,
};

vi.mock("../tauri", () => ({ backend: mockBackend }));

const { project } = await import("./project.svelte");
const { midi } = await import("./midi.svelte");
const { toasts } = await import("./toasts.svelte");
const { clipEditLoop } = await import("./clip-edit-loop.svelte");
const { projectops } = await import("./projectops.svelte");

function lastToast() {
  return toasts.list[toasts.list.length - 1];
}

beforeEach(() => {
  vi.clearAllMocks();
  mockBackend.mode = "tauri";
  project.tracks = [];
  project.clips = [];
  project.projectDir = null;
  project.name = "Untitled";
  midi.clips = [];
  toasts.list = [];
  projectops.dialog = null;
  projectops.confirm = null;
  projectops.pickDirectory = async () => null;
  projectops.defaultParentDir = async () => "/home/u/Music/AURA";
  clipEditLoop.reset();
});

describe("save", () => {
  it("invokes save_project and toasts success when a project is open", async () => {
    project.projectDir = "/p/Song.aura";

    await projectops.save();

    expect(invokes.saveProject).toHaveBeenCalled();
    expect(lastToast().kind).toBe("success");
  });

  it("opens the Save As dialog prefilled when no project is open", async () => {
    project.name = "Jam";

    await projectops.save();

    expect(invokes.saveProject).not.toHaveBeenCalled();
    expect(projectops.dialog).toEqual({
      mode: "saveAs",
      name: "Jam",
      parentDir: "/home/u/Music/AURA",
    });
  });

  it("surfaces backend failure as an error toast", async () => {
    project.projectDir = "/p/Song.aura";
    invokes.saveProject.mockRejectedValueOnce(new Error("disk full"));

    await projectops.save();

    expect(lastToast().kind).toBe("error");
    expect(lastToast().lines.join(" ")).toContain("disk full");
  });

  it("only informs in demo mode", async () => {
    mockBackend.mode = "demo";
    project.projectDir = "/p/Song.aura";

    await projectops.save();

    expect(invokes.saveProject).not.toHaveBeenCalled();
    expect(lastToast().kind).toBe("info");
  });
});

describe("new project", () => {
  it("asks for confirmation first when the session has content", () => {
    project.tracks = [{ id: "t1" } as (typeof project.tracks)[number]];

    projectops.requestNew();

    expect(projectops.confirm).toBe("new");
    expect(projectops.dialog).toBeNull();
  });

  it("skips confirmation when the session is empty", async () => {
    projectops.requestNew();
    await vi.waitFor(() => expect(projectops.dialog).not.toBeNull());

    expect(projectops.dialog?.mode).toBe("new");
    expect(projectops.confirm).toBeNull();
  });

  it("creates the project and re-pulls the snapshot on dialog submit", async () => {
    projectops.dialog = { mode: "new", name: "Fresh", parentDir: "/home/u/Music/AURA" };

    await projectops.submitDialog();

    expect(invokes.createProject).toHaveBeenCalledWith("Fresh", "/home/u/Music/AURA");
    expect(invokes.getProjectState).toHaveBeenCalled();
    expect(projectops.dialog).toBeNull();
    expect(lastToast().kind).toBe("success");
  });

  it("keeps the dialog open and toasts on create failure", async () => {
    invokes.createProject.mockRejectedValueOnce(new Error("project already exists"));
    projectops.dialog = { mode: "new", name: "Dup", parentDir: "/p" };

    await projectops.submitDialog();

    expect(projectops.dialog).not.toBeNull();
    expect(lastToast().kind).toBe("error");
  });
});

describe("save as (unsaved session)", () => {
  it("materializes via save_project_as on dialog submit", async () => {
    projectops.dialog = { mode: "saveAs", name: "First", parentDir: "/p" };

    await projectops.submitDialog();

    expect(invokes.saveProjectAs).toHaveBeenCalledWith("First", "/p");
    expect(invokes.createProject).not.toHaveBeenCalled();
    expect(projectops.dialog).toBeNull();
  });
});

// Finding 8: adopt() (the re-pull that runs after create/save-as/open) used
// to leave the piano roll's loop-while-editing snapshot (loop region, solo
// map) pointed at the OLD project. A non-replaying reset() must run before
// every adopt so a switch never replays a stale snapshot onto the newly
// opened project.
describe("adopting a project resets the clip-edit loop (finding 8)", () => {
  it("submitDialog (new project) resets the clip-edit loop before re-pulling", async () => {
    const resetSpy = vi.spyOn(clipEditLoop, "reset");
    projectops.dialog = { mode: "new", name: "Fresh", parentDir: "/home/u/Music/AURA" };

    await projectops.submitDialog();

    expect(resetSpy).toHaveBeenCalled();
  });

  it("submitDialog (save as) resets the clip-edit loop before re-pulling", async () => {
    const resetSpy = vi.spyOn(clipEditLoop, "reset");
    projectops.dialog = { mode: "saveAs", name: "First", parentDir: "/p" };

    await projectops.submitDialog();

    expect(resetSpy).toHaveBeenCalled();
  });

  it("openViaPicker resets the clip-edit loop before re-pulling", async () => {
    const resetSpy = vi.spyOn(clipEditLoop, "reset");
    projectops.pickDirectory = async () => "/p/Other.aura";

    await projectops.openViaPicker();

    expect(resetSpy).toHaveBeenCalled();
  });
});

describe("open project", () => {
  it("uses the directory picker and adopts the opened project", async () => {
    projectops.pickDirectory = async () => "/p/Other.aura";

    await projectops.openViaPicker();

    expect(invokes.openProject).toHaveBeenCalledWith("/p/Other.aura");
    expect(invokes.getProjectState).toHaveBeenCalled();
    expect(project.projectDir).toBe("/home/u/Music/AURA/Loaded.aura");
  });

  it("is a no-op when the picker is cancelled", async () => {
    projectops.pickDirectory = async () => null;

    await projectops.openViaPicker();

    expect(invokes.openProject).not.toHaveBeenCalled();
  });

  it("asks for confirmation first when the session has content", () => {
    midi.clips = [{ id: "mc" } as (typeof midi.clips)[number]];

    projectops.requestOpen();

    expect(projectops.confirm).toBe("open");
    expect(invokes.openProject).not.toHaveBeenCalled();
  });
});

describe("the unsaved-changes confirmation", () => {
  it("proceeds with the pending action on 'continue without saving'", async () => {
    project.tracks = [{ id: "t1" } as (typeof project.tracks)[number]];
    projectops.requestNew();

    await projectops.proceedConfirmed(false);

    expect(projectops.confirm).toBeNull();
    expect(projectops.dialog?.mode).toBe("new");
  });

  it("saves first when asked and a project is open", async () => {
    project.tracks = [{ id: "t1" } as (typeof project.tracks)[number]];
    project.projectDir = "/p/Song.aura";
    projectops.requestNew();

    await projectops.proceedConfirmed(true);

    expect(invokes.saveProject).toHaveBeenCalled();
    expect(projectops.dialog?.mode).toBe("new");
  });

  it("cancel clears the pending action", () => {
    project.tracks = [{ id: "t1" } as (typeof project.tracks)[number]];
    projectops.requestOpen();

    projectops.cancelConfirm();

    expect(projectops.confirm).toBeNull();
  });

  // Finding 3: save() used to swallow a failed pre-save into a toast and
  // return void, so proceedConfirmed(true) had no way to tell the save
  // failed — it proceeded to New/Open anyway and destroyed the session that
  // failed to save. save() now reports success/failure and proceedConfirmed
  // must stop on failure.
  it("does not proceed with the pending action when the pre-save fails", async () => {
    project.tracks = [{ id: "t1" } as (typeof project.tracks)[number]];
    project.projectDir = "/p/Song.aura";
    invokes.saveProject.mockRejectedValueOnce(new Error("disk full"));
    projectops.requestNew();

    await projectops.proceedConfirmed(true);

    expect(invokes.saveProject).toHaveBeenCalled();
    expect(lastToast().kind).toBe("error");
    expect(invokes.createProject).not.toHaveBeenCalled();
    expect(projectops.dialog).toBeNull();
  });
});

describe("undo / redo (Plan E Task 17)", () => {
  it("invokes undo, re-pulls the snapshot and reports the step's label", async () => {
    await projectops.undo();
    expect(invokes.undo).toHaveBeenCalledTimes(1);
    expect(invokes.getProjectState).toHaveBeenCalled();
    expect(lastToast().title).toBe("UNDO");
    expect(lastToast().lines).toEqual(["set gain"]);
  });

  it("invokes redo the same way", async () => {
    await projectops.redo();
    expect(invokes.redo).toHaveBeenCalledTimes(1);
    expect(lastToast().title).toBe("REDO");
  });

  it("is silent, and re-pulls nothing, when the stack is empty", async () => {
    invokes.undo.mockResolvedValueOnce({ label: null, undoDepth: 0, redoDepth: 0 });
    await projectops.undo();
    expect(invokes.getProjectState).not.toHaveBeenCalled();
    expect(toasts.list).toHaveLength(0);
  });

  it("surfaces a backend failure as an error toast", async () => {
    invokes.undo.mockRejectedValueOnce("nope");
    await projectops.undo();
    expect(lastToast().title).toBe("UNDO FAILED");
  });

  it("does nothing when the backend has no op log (demo mode)", async () => {
    const saved = mockBackend.undo;
    // The optional-method pattern: the demo backend simply omits it.
    // @ts-expect-error deliberately removing an optional method
    delete mockBackend.undo;
    await projectops.undo();
    expect(toasts.list).toHaveLength(0);
    mockBackend.undo = saved;
  });
});
