/**
 * Project operations (save / open / new): the orchestration between the
 * backend project commands, the confirm-unsaved guard, the name dialog, and
 * the store re-pull after switching projects. These tests pin the flows the
 * TransportBar project menu and the Ctrl+S/O/N shortcuts drive.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { readLastProjectDir, readRecentProjectDirs, writeLastProjectDir } from "../utils/last-project";

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
  historyUndoTo: vi.fn(
    (
      _targetRev: number,
      _expectedEpoch: number,
      _expectedHeadRev: number | null,
    ): Promise<{ steps: number; label: string | null }> =>
      Promise.resolve({ steps: 2, label: "move clip" }),
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
  automationGet: vi.fn(() => Promise.resolve([])),
  modulationGet: vi.fn(() =>
    Promise.resolve({ curves: [], bindings: [], automationClips: [] }),
  ),
  pluginList: vi.fn(() =>
    Promise.resolve({ plugins: [], scanned: true, instances: [] }),
  ),
  pluginGetParams: vi.fn(() => Promise.resolve([])),
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

function fakeStorage(initial: Record<string, string> = {}) {
  const map = new Map(Object.entries(initial));
  return {
    getItem: (key: string) => (map.has(key) ? (map.get(key) as string) : null),
    setItem: (key: string, value: string) => {
      map.set(key, value);
    },
    map,
  };
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
  projectops.pendingRecent = null;
  projectops.recent = [];
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

  /**
   * M-3 (Plan E whole-branch review): `step()` re-pulled project + midi and
   * nothing else, so undoing an automation-lane or plugin-param step left
   * those views showing pre-undo values until something else refreshed
   * them. The re-pull now covers every store an op can address.
   */
  it("re-pulls automation lanes and plugin panels after an undo", async () => {
    const { plugins } = await import("./plugins.svelte");
    plugins.openInstanceId = "inst-1";
    await projectops.undo();
    expect(invokes.automationGet).toHaveBeenCalledTimes(1);
    expect(invokes.modulationGet).toHaveBeenCalledTimes(1);
    expect(invokes.pluginList).toHaveBeenCalledTimes(1);
    expect(invokes.pluginGetParams).toHaveBeenCalledWith("inst-1");
  });

  it("re-pulls them after a redo too", async () => {
    await projectops.redo();
    expect(invokes.automationGet).toHaveBeenCalledTimes(1);
    expect(invokes.modulationGet).toHaveBeenCalledTimes(1);
    expect(invokes.pluginList).toHaveBeenCalledTimes(1);
  });
});

describe("undo to here (Task 4)", () => {
  it("does nothing when the backend has no op log (demo mode)", async () => {
    const saved = mockBackend.historyUndoTo;
    // The optional-method pattern: the demo backend simply omits it.
    // @ts-expect-error deliberately removing an optional method
    delete mockBackend.historyUndoTo;

    const result = await projectops.undoTo(8, 4, 9);

    expect(result).toBe(false);
    expect(invokes.getProjectState).not.toHaveBeenCalled();
    expect(toasts.list).toHaveLength(0);
    mockBackend.historyUndoTo = saved;
  });

  it("is silent, and re-pulls nothing, when the target is already the head (steps === 0)", async () => {
    invokes.historyUndoTo.mockResolvedValueOnce({ steps: 0, label: null });

    const result = await projectops.undoTo(9, 4, 9);

    expect(result).toBe(true);
    expect(invokes.getProjectState).not.toHaveBeenCalled();
    expect(toasts.list).toHaveLength(0);
  });

  it("re-pulls and toasts the destination by revision, never by out.label, when the walk moved", async () => {
    invokes.historyUndoTo.mockResolvedValueOnce({ steps: 2, label: "trim clip" });

    const result = await projectops.undoTo(8, 4, 9);

    expect(invokes.historyUndoTo).toHaveBeenCalledWith(8, 4, 9);
    expect(result).toBe(true);
    expect(invokes.getProjectState).toHaveBeenCalled();
    expect(lastToast().title).toBe("UNDO");
    // The destination is the revision the caller asked for (r8), not the
    // backend's `out.label` ("trim clip" — the last step undone, which for
    // a multi-step walk names the wrong edit for "where you landed").
    expect(lastToast().lines).toEqual(["2 steps back to r8"]);
    expect(lastToast().lines.join(" ")).not.toContain("trim clip");
  });

  it("toasts the failure and rethrows so the caller can render the reason", async () => {
    invokes.historyUndoTo.mockRejectedValueOnce(new Error("the edit history changed under this request"));

    await expect(projectops.undoTo(8, 4, 9)).rejects.toThrow(
      "the edit history changed under this request",
    );

    expect(lastToast().title).toBe("UNDO TO HERE FAILED");
    expect(lastToast().kind).toBe("error");
  });

  /**
   * I-2 (whole-branch review). `undo_to` leaves the steps it applied before
   * a mid-walk abort APPLIED, so a rejection does NOT mean "nothing
   * happened". `project` and `midi` self-heal off `project://changed`, but
   * automation, modulation, launch and an open plugin's params only refresh
   * in `repull()` — so without one here a partial walk leaves the automation
   * lanes and the param panel showing pre-walk values.
   */
  it("re-pulls the stores a partial walk left stale, then rethrows", async () => {
    const { plugins } = await import("./plugins.svelte");
    plugins.openInstanceId = "inst-1";
    invokes.historyUndoTo.mockRejectedValueOnce(
      new Error("stopped after 1 of 3 steps: the edit history moved under the walk"),
    );

    await expect(projectops.undoTo(8, 4, 9)).rejects.toThrow("stopped after 1 of 3 steps");

    expect(invokes.getProjectState).toHaveBeenCalled();
    expect(invokes.automationGet).toHaveBeenCalledTimes(1);
    expect(invokes.modulationGet).toHaveBeenCalledTimes(1);
    expect(invokes.pluginList).toHaveBeenCalledTimes(1);
    expect(invokes.pluginGetParams).toHaveBeenCalledWith("inst-1");
    expect(lastToast().title).toBe("UNDO TO HERE FAILED");
  });
});

describe("remember / restore last project", () => {
  let storage: ReturnType<typeof fakeStorage>;

  beforeEach(() => {
    storage = fakeStorage();
    vi.stubGlobal("localStorage", storage);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("openViaPicker remembers the adopted project dir", async () => {
    projectops.pickDirectory = async () => "/p/Other.aura";

    await projectops.openViaPicker();

    expect(readLastProjectDir()).toBe("/home/u/Music/AURA/Loaded.aura");
  });

  it("creating a project remembers the adopted project dir", async () => {
    projectops.dialog = { mode: "new", name: "Fresh", parentDir: "/home/u/Music/AURA" };

    await projectops.submitDialog();

    expect(readLastProjectDir()).toBe("/home/u/Music/AURA/Loaded.aura");
  });

  it("save-as remembers the adopted project dir", async () => {
    projectops.dialog = { mode: "saveAs", name: "First", parentDir: "/p" };

    await projectops.submitDialog();

    expect(readLastProjectDir()).toBe("/home/u/Music/AURA/Loaded.aura");
  });

  it("opening a new untitled dialog does not clear a remembered path", async () => {
    writeLastProjectDir("/p/Song.aura");

    await projectops.openNewDialog();

    expect(readLastProjectDir()).toBe("/p/Song.aura");
  });

  it("restoreLast opens the remembered path and adopts it", async () => {
    writeLastProjectDir("/p/Song.aura");

    await projectops.restoreLast();

    expect(invokes.openProject).toHaveBeenCalledWith("/p/Song.aura");
    expect(invokes.getProjectState).toHaveBeenCalled();
    expect(project.projectDir).toBe("/home/u/Music/AURA/Loaded.aura");
    expect(toasts.list).toHaveLength(0);
  });

  it("restoreLast is a silent no-op when nothing is remembered", async () => {
    await projectops.restoreLast();

    expect(invokes.openProject).not.toHaveBeenCalled();
    expect(toasts.list).toHaveLength(0);
  });

  it("restoreLast is a silent no-op in the browser demo", async () => {
    writeLastProjectDir("/p/Song.aura");
    mockBackend.mode = "demo";

    await projectops.restoreLast();

    expect(invokes.openProject).not.toHaveBeenCalled();
    expect(toasts.list).toHaveLength(0);
  });

  it("restoreLast is a no-op when a project is already open", async () => {
    writeLastProjectDir("/p/Other.aura");
    project.projectDir = "/p/Song.aura";

    await projectops.restoreLast();

    expect(invokes.openProject).not.toHaveBeenCalled();
  });

  it("restoreLast toasts when the remembered project cannot be opened", async () => {
    writeLastProjectDir("/p/Gone.aura");
    invokes.openProject.mockRejectedValueOnce(new Error("no such project"));

    await projectops.restoreLast();

    expect(lastToast().kind).toBe("error");
    expect(lastToast().title).toBe("OPEN FAILED");
    expect(project.projectDir).toBeNull();
  });

  it("a failed restore keeps the remembered path for the next launch", async () => {
    writeLastProjectDir("/p/Gone.aura");
    invokes.openProject.mockRejectedValueOnce(new Error("no such project"));

    await projectops.restoreLast();

    expect(readLastProjectDir()).toBe("/p/Gone.aura");
  });

  it("remembers a history of adopted dirs, newest first", async () => {
    const snap = {
      projectName: "Loaded",
      transport: { sampleRate: 48000, tempoBpm: 120 },
      tracks: [],
      clips: [],
      ppq: 960,
      tempoEvents: [{ tick: 0, bpm: 120 }],
      midiClips: [],
    };
    let dir = "/p/A.aura";
    invokes.getProjectState.mockImplementation(() =>
      Promise.resolve({ ...snap, projectDir: dir }),
    );
    projectops.pickDirectory = async () => "/p/A.aura";
    await projectops.openViaPicker();
    dir = "/p/B.aura";
    projectops.pickDirectory = async () => "/p/B.aura";
    await projectops.openViaPicker();

    expect(readRecentProjectDirs()).toEqual(["/p/B.aura", "/p/A.aura"]);
    expect(projectops.recent).toEqual(["/p/B.aura", "/p/A.aura"]);
  });

  it("requestOpenRecent opens that path and adopts it", async () => {
    await projectops.requestOpenRecent("/p/Song.aura");

    expect(invokes.openProject).toHaveBeenCalledWith("/p/Song.aura");
    expect(invokes.getProjectState).toHaveBeenCalled();
    expect(lastToast().title).toBe("PROJECT OPENED");
  });

  it("requestOpenRecent asks for confirmation when the session has content", () => {
    midi.clips = [{ id: "mc" } as (typeof midi.clips)[number]];

    void projectops.requestOpenRecent("/p/Song.aura");

    expect(projectops.confirm).toBe("recent");
    expect(projectops.pendingRecent).toBe("/p/Song.aura");
    expect(invokes.openProject).not.toHaveBeenCalled();
  });

  it("proceedConfirmed opens the pending recent path", async () => {
    midi.clips = [{ id: "mc" } as (typeof midi.clips)[number]];
    void projectops.requestOpenRecent("/p/Song.aura");

    await projectops.proceedConfirmed(false);

    expect(invokes.openProject).toHaveBeenCalledWith("/p/Song.aura");
    expect(projectops.confirm).toBeNull();
    expect(projectops.pendingRecent).toBeNull();
  });

  it("requestOpenRecent is a no-op when that project is already open", async () => {
    project.projectDir = "/p/Song.aura";

    await projectops.requestOpenRecent("/p/Song.aura");

    expect(invokes.openProject).not.toHaveBeenCalled();
  });

  it("refreshRecent loads stored history into the store", () => {
    writeLastProjectDir("/p/A.aura");
    writeLastProjectDir("/p/B.aura");

    projectops.refreshRecent();

    expect(projectops.recent).toEqual(["/p/B.aura", "/p/A.aura"]);
  });
});
