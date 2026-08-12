/**
 * DEV-ONLY verification hook for the real app.
 *
 * Active only in `vite dev` builds running inside Tauri. Reads a same-origin
 * scratch script (`/src/dev-drive-cmd.json`, served by the vite dev server;
 * absent in production and in the plain-browser demo) and runs its typed
 * steps sequentially against the real stores — the exact code paths the UI
 * buttons call. No dynamic code ever runs: the file carries data for
 * predefined actions only. Progress is surfaced as console lines + toasts
 * and POSTed to the session's local result collector when one is running.
 *
 * NOTE: vite full-reloads the page whenever the script file changes (it is
 * not part of the module graph), so each write starts a fresh run — the
 * whole sequence therefore lives in ONE file.
 */

import { backend, isTauri } from "./tauri";

interface DriveStep {
  id: string;
  action: string;
  args?: Record<string, unknown>;
}

interface DriveScript {
  /** Only the instance whose page origin matches executes (multi-instance
   * dev machines: each app loads from its own vite port). */
  origin?: string;
  /** Skip marker: a script already executed is keyed by this run id. */
  run: string;
  steps: DriveStep[];
}

const COLLECTOR = "http://127.0.0.1:14311/";

async function stores() {
  const [
    { project },
    { midi },
    { transport },
    { hum },
    { exporter },
    { loopjam },
    { plugins },
    { zyn },
    { ui },
  ] = await Promise.all([
    import("./state/project.svelte"),
    import("./state/midi.svelte"),
    import("./state/transport.svelte"),
    import("./state/hum.svelte"),
    import("./state/exporter.svelte"),
    import("./state/loopjam.svelte"),
    import("./state/plugins.svelte"),
    import("./state/zynpatches.svelte"),
    import("./state/ui.svelte"),
  ]);
  return { project, midi, transport, hum, exporter, loopjam, plugins, zyn, ui };
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function runStep(action: string, args: Record<string, unknown>): Promise<unknown> {
  const s = await stores();
  switch (action) {
    case "state":
      return {
        mode: backend.mode,
        tracks: s.project.tracks.map(
          (t) => `${t.name}${t.instrumentId ? ` [${t.instrumentId.slice(0, 18)}…]` : ""}`,
        ),
        midiClips: s.midi.clips.map((c) => `${c.name}:${c.notes.length}n`),
        clips: s.project.clips.length,
        loopjam: s.loopjam.status.state,
      };
    case "openDock":
      s.ui.dock = (args.tab as typeof s.ui.dock) ?? "hum";
      return s.ui.dock;
    case "sleep":
      await sleep(Number(args.ms ?? 1000));
      return null;
    case "seedDemo": {
      try {
        const snap = await backend.seedDemoProject?.();
        if (snap) {
          s.project.applySnapshot(snap);
          s.midi.applySnapshot(snap);
        }
      } catch (err) {
        return { note: String(err), tracks: s.project.tracks.length };
      }
      return { tracks: s.project.tracks.length };
    }
    case "humRun": {
      s.ui.dock = "hum";
      const ok = await s.hum.run({
        path: String(args.path),
        accompany: !!args.accompany,
        quantizeGrid: (args.quantizeGrid as number) ?? null,
      });
      return { submitted: ok, error: s.hum.error };
    }
    case "humWait": {
      const deadline = Date.now() + Number(args.timeoutMs ?? 60_000);
      while (s.hum.busy && Date.now() < deadline) await sleep(300);
      return {
        phase: s.hum.phase,
        stage: s.hum.stage,
        error: s.hum.error,
        clip: s.hum.melodyClipId,
        track: s.hum.melodyTrackId,
        notes: s.hum.melodyNoteCount,
        createdTrack: s.hum.createdTrackName,
        log: s.hum.log.slice(-3),
      };
    }
    case "exportRun": {
      await s.exporter.openDialog();
      const ok = await s.exporter.start({
        path: String(args.path),
        format: (args.format as string) ?? null,
        region: (args.region as string) ?? null,
        bitDepth: (args.bitDepth as number) ?? null,
      });
      if (!ok) return { started: false, error: s.exporter.submitError };
      const deadline = Date.now() + Number(args.timeoutMs ?? 120_000);
      while (s.exporter.job?.state === "running" && Date.now() < deadline) await sleep(300);
      return { caps: s.exporter.caps, job: s.exporter.job };
    }
    case "createProject": {
      const p = await backend.createProject(String(args.name ?? "Drive Test"));
      await s.project.reload();
      return { name: p.name, path: p.path ?? null };
    }
    case "importClip": {
      const clip = await backend.importAudioClip({ path: String(args.path) });
      await s.project.reload();
      return { clip: clip.id, track: clip.trackId, length: clip.lengthSamples };
    }
    case "setLoop": {
      const bar = s.project.samplesPerBar;
      await s.transport.setLoop(
        true,
        Number(args.startBar ?? 0) * bar,
        Number(args.endBar ?? 4) * bar,
      );
      return s.transport.snap;
    }
    case "loopjamEvolve": {
      await s.loopjam.evolve(String(args.prompt ?? ""), String(args.seed ?? ""));
      return { status: s.loopjam.status, error: s.loopjam.error };
    }
    case "loopjamWait": {
      const deadline = Date.now() + Number(args.timeoutMs ?? 60_000);
      while (s.loopjam.busy && Date.now() < deadline) await sleep(400);
      return { status: s.loopjam.status, error: s.loopjam.error };
    }
    case "loopjamCancel":
      await s.loopjam.cancel();
      return s.loopjam.status;
    case "pluginScan": {
      s.ui.dock = "plugins";
      await s.plugins.scan();
      return {
        found: s.plugins.descriptors.length,
        zyn: s.plugins.descriptors.filter((d) => d.uid.includes("zyn")).map((d) => d.uid),
        error: s.plugins.scanError,
      };
    }
    case "zynOpenPatches": {
      s.ui.dock = "plugins";
      let inst = s.plugins.instances.find((i) => i.uid.toLowerCase().includes("zyn"));
      if (!inst) {
        await s.plugins.refresh();
        inst = s.plugins.instances.find((i) => i.uid.toLowerCase().includes("zyn"));
      }
      if (!inst) {
        const desc = s.plugins.descriptors.find(
          (d) => d.uid.toLowerCase().includes("zyn") && d.isInstrument,
        );
        if (!desc) return { error: "no zyn descriptor — scan first" };
        inst = (await s.plugins.instantiate(desc.uid, (args.trackId as string) ?? null)) ?? undefined;
        await sleep(1500);
      }
      if (!inst) return { error: s.plugins.error };
      await s.zyn.openFor(inst.id);
      return {
        instance: inst.id,
        status: s.plugins.byId(inst.id)?.status,
        boundTrack: s.plugins.byId(inst.id)?.trackId ?? inst.trackId,
        patches: s.zyn.patches.length,
        banks: [...new Set(s.zyn.patches.map((p) => p.bank))].length,
        error: s.zyn.error,
      };
    }
    case "zynBind": {
      const inst = s.plugins.instances.find((i) => i.uid.toLowerCase().includes("zyn"));
      if (!inst) return { error: "no zyn instance" };
      await s.plugins.bind(inst.id, String(args.trackId));
      return { bound: String(args.trackId), error: s.plugins.error };
    }
    case "zynLoadPatch": {
      const inst = s.plugins.instances.find((i) => i.uid.toLowerCase().includes("zyn"));
      if (!inst) return { error: "no zyn instance" };
      if (s.zyn.patches.length === 0) await s.zyn.refresh();
      const q = String(args.query ?? "pluck").toLowerCase();
      const patch =
        s.zyn.patches.find((p) => p.name.toLowerCase().includes(q)) ??
        s.zyn.patches.find((p) => p.bank.toLowerCase().includes(q));
      if (!patch) return { error: `no patch matching ${q}` };
      const ok = await s.zyn.load(inst.id, patch);
      return { loaded: ok, patch: `${patch.bank}/${patch.name}`, error: s.zyn.error };
    }
    case "trackIdByName": {
      const t = s.project.tracks.find((t) => t.name === String(args.name));
      return t ? { id: t.id, instrumentId: t.instrumentId ?? null } : { error: "not found" };
    }
    default:
      return { error: `unknown action: ${action}` };
  }
}

export function startDevDrive(): void {
  if (!import.meta.env.DEV || !isTauri) return;
  void (async () => {
    let script: DriveScript;
    try {
      const res = await fetch(`/src/dev-drive-cmd.json?t=${Date.now()}`, { cache: "no-store" });
      if (!res.ok) return;
      script = (await res.json()) as DriveScript;
    } catch {
      return; // no script — normal dev session
    }
    if (!script?.run || !Array.isArray(script.steps)) return;
    if (script.origin && script.origin !== window.location.origin) return;
    if (sessionStorage.getItem("aura-drive-run") === script.run) return;
    sessionStorage.setItem("aura-drive-run", script.run);

    const { toasts } = await import("./state/toasts.svelte");
    const results: Record<string, unknown>[] = [];
    const post = async () => {
      try {
        await fetch(COLLECTOR, {
          method: "POST",
          body: JSON.stringify({ run: script.run, origin: window.location.origin, results }),
        });
      } catch {
        /* collector not running */
      }
    };
    await sleep(1200); // let the app finish booting/stores init
    for (const step of script.steps) {
      let entry: Record<string, unknown>;
      try {
        const result = await runStep(step.action, step.args ?? {});
        console.log(`[dev-drive] ${step.id} ok:`, JSON.stringify(result));
        entry = { id: step.id, ok: true, result: result ?? null };
      } catch (err) {
        console.error(`[dev-drive] ${step.id} failed:`, err);
        entry = { id: step.id, ok: false, error: String(err) };
      }
      results.push(entry);
      await post();
    }
    results.push({ id: "__done__", ok: true });
    await post();
    toasts.info(`DRIVE ${script.run} DONE`, `${results.length - 1} steps`);
  })();
}
