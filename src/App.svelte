<script lang="ts">
  /**
   * AURA — DAW shell. Boots the stores against the backend adapter (real
   * Tauri engine, or the synthetic demo engine in a plain browser), wires
   * global shortcuts, and lays out transport / timeline / piano roll /
   * master chrome plus the right dock and the MCP confirm overlay.
   */
  import { onMount } from "svelte";
  import { transport } from "./lib/state/transport.svelte";
  import { project } from "./lib/state/project.svelte";
  import { midi } from "./lib/state/midi.svelte";
  import { instruments } from "./lib/state/instruments.svelte";
  import { plugins } from "./lib/state/plugins.svelte";
  import { automation } from "./lib/state/automation.svelte";
  import { modulation } from "./lib/state/modulation.svelte";
  import { mcp } from "./lib/state/mcp.svelte";
  import { startMeterStream, stopMeterStream } from "./lib/state/meters.svelte";
  import { view } from "./lib/state/view.svelte";
  import {
    dockTabForKey,
    resetUiZoom,
    toggleDock,
    ui,
    zoomUiIn,
    zoomUiOut,
  } from "./lib/state/ui.svelte";
  import { library } from "./lib/state/library.svelte";
  import { prefs } from "./lib/prefs/prefs.svelte";
  import { applyUiZoom } from "./lib/utils/ui-zoom";
  import { exporter } from "./lib/state/exporter.svelte";
  import { loopjam } from "./lib/state/loopjam.svelte";
  import { generation } from "./lib/state/generation.svelte";
  import { clipEdges, edgeJump, gridStep } from "./lib/utils/timeline-nav";
  import { clipClipboard } from "./lib/state/clip-clipboard.svelte";
  import { clipDrag } from "./lib/state/clip-drag.svelte";
  import { clipSelection } from "./lib/state/clip-selection.svelte";
  import TransportBar from "./lib/components/TransportBar.svelte";
  import Timeline from "./lib/components/Timeline.svelte";
  import MasterBar from "./lib/components/MasterBar.svelte";
  import PianoRoll from "./lib/components/pianoroll/PianoRoll.svelte";
  import Dock from "./lib/components/Dock.svelte";
  import McpConfirmDialog from "./lib/components/mcp/McpConfirmDialog.svelte";
  import PreferencesDialog from "./lib/components/prefs/PreferencesDialog.svelte";
  import ExportDialog from "./lib/components/export/ExportDialog.svelte";
  import ProjectDialog from "./lib/components/project/ProjectDialog.svelte";
  import { projectops } from "./lib/state/projectops.svelte";
  import Toasts from "./lib/components/Toasts.svelte";

  onMount(() => {
    prefs.init(); // restore persisted preferences before anything paints or boots
    void transport.init();
    void project.init();
    void midi.init();
    void instruments.refresh();
    void plugins.refresh();
    void automation.reload();
    void modulation.reload();
    void mcp.init();
    exporter.init();
    void loopjam.init();
    generation.init(); // adopt jobs an agent starts over MCP
    void startMeterStream();
    return () => {
      stopMeterStream();
      generation.dispose();
    };
  });

  // Interface zoom on <body> (not .app) so fixed overlays — dialogs,
  // toasts — scale along with the shell.
  $effect(() => applyUiZoom(document.body, prefs.values.uiZoom));

  /** Where the playhead is right now — interpolated while rolling. */
  function playhead(): number {
    return Math.round(transport.positionAt(performance.now()));
  }

  /** True when the event targets somewhere the user is typing or picking —
   * an `<input>`, a `<textarea>`, a `<select>`, or any contenteditable
   * host. Such a target owns its own editing keys (Ctrl+Z included). The
   * list matches the guard the rest of `onKeydown` already applies below;
   * keep the two in step. */
  function isTextEntry(target: EventTarget | null): boolean {
    const el = target as HTMLElement | null;
    if (!el) return false;
    const tag = el.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return true;
    return el.isContentEditable === true;
  }

  function onKeydown(e: KeyboardEvent) {
    // Interface zoom first: like browser page zoom it must work everywhere,
    // including inputs and the piano roll, so it runs before the guards.
    if (e.ctrlKey || e.metaKey) {
      if (e.key === "+" || e.key === "=") {
        e.preventDefault();
        zoomUiIn();
        return;
      }
      if (e.key === "-") {
        e.preventDefault();
        zoomUiOut();
        return;
      }
      if (e.key === "0") {
        e.preventDefault();
        resetUiZoom();
        return;
      }
      // Ctrl/Cmd+, — the conventional preferences shortcut.
      if (e.key === "," && !e.altKey && !e.shiftKey) {
        e.preventDefault();
        prefs.dialogOpen = !prefs.dialogOpen;
        return;
      }
    }
    // Project shortcuts work everywhere — inputs and piano roll included.
    if ((e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey) {
      const k = e.key.toLowerCase();
      if (k === "s") {
        e.preventDefault();
        void projectops.save();
        return;
      }
      if (k === "o") {
        e.preventDefault();
        projectops.requestOpen();
        return;
      }
      if (k === "n") {
        e.preventDefault();
        projectops.requestNew();
        return;
      }
    }
    // Undo / redo (Plan E Task 17). GUARDED, unlike Ctrl+S/O/N above: a
    // focused text field owns its own undo stack, and hijacking Ctrl+Z
    // while someone renames a track would feel broken. The piano roll is
    // NOT excluded — note edits are ordinary history steps, so Ctrl+Z must
    // work with the roll focused.
    if ((e.metaKey || e.ctrlKey) && !e.altKey && e.key.toLowerCase() === "z" && !isTextEntry(e.target)) {
      e.preventDefault();
      void (e.shiftKey ? projectops.redo() : projectops.undo());
      return;
    }
    const tag = (e.target as HTMLElement)?.tagName;
    if (tag === "INPUT" || tag === "SELECT" || tag === "TEXTAREA") return;
    // piano roll owns its own keys while hovered/focused
    if ((e.target as HTMLElement)?.closest?.(".roll")) return;
    if (e.code === "Space") {
      e.preventDefault();
      void transport.togglePlay();
    } else if (e.key === "Home") {
      void transport.seek(0);
    } else if (e.key === "End") {
      // The engine's end, not our own guess from clip bounds: it also
      // accounts for the final scheduled note-off.
      void transport.seek(transport.snap.songEndSamples);
    } else if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
      // Grid walk: a bar at a time, a beat with shift.
      e.preventDefault();
      const dir = e.key === "ArrowRight" ? 1 : -1;
      const step = e.shiftKey ? project.samplesPerBeat : project.samplesPerBar;
      void transport.seek(gridStep(playhead(), step, dir));
    } else if (e.key === "," || e.key === ".") {
      // Jump between clip edges across every track — the fastest way
      // through an arrangement. Staying put beats jumping somewhere
      // arbitrary when there is no edge left.
      const dir = e.key === "." ? 1 : -1;
      const target = edgeJump(
        [
          ...clipEdges(project.clips),
          // MIDI clips are musical time; the tempo map converts them. Length
          // is the DIFFERENCE of two converted positions, not a direct
          // conversion of the tick length — the latter drifts under a
          // non-constant tempo map (pre-existing bug, fixed alongside the
          // clip-looping work per spec §6).
          ...clipEdges(
            midi.clips.map((c) => {
              const start = midi.ticksToSamples(c.timelineStartTicks);
              return {
                timelineStartSamples: start,
                lengthSamples: midi.ticksToSamples(c.timelineStartTicks + c.lengthTicks) - start,
              };
            }),
          ),
        ],
        playhead(),
        dir,
      );
      if (target !== null) void transport.seek(target);
    } else if (e.key === "Escape" && clipDrag.active) {
      e.preventDefault();
      clipDrag.cancel();
    } else if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "c") {
      // Copy the timeline selection (multi-clip, audio + MIDI). Falls back
      // to the legacy single-clip stamp when nothing is multi-selected, so
      // the old Ctrl+C behaviour never regresses.
      e.preventDefault();
      if (clipSelection.count() > 0) void clipClipboard.copy();
      else midi.copySelected();
    } else if ((e.ctrlKey || e.metaKey) && !e.altKey && e.key.toLowerCase() === "v") {
      // Paste at the playhead: Shift pastes onto NEW tracks.
      // `clipClipboard.pasteAtPlayhead` OWNS the multi-clip-first / legacy-
      // fallback / nothing-to-paste-toast orchestration (fix round 2) — a
      // thin delegation here, not reimplemented, so App.svelte stays inside
      // the "no logic a test can't reach" rule. `playhead()` is evaluated
      // ONCE, right here, as the argument: it is a live interpolated
      // position, and reading it again after an internal await would let
      // the legacy fallback land at a later position than the one the key
      // was actually pressed at.
      e.preventDefault();
      void clipClipboard.pasteAtPlayhead(playhead(), e.shiftKey);
    } else if ((e.ctrlKey || e.metaKey) && e.shiftKey && !e.altKey && e.key.toLowerCase() === "m") {
      // Export the selected MIDI clips as a .mid file — the interchange
      // half of copy, for other DAWs (SMF never rides on the clipboard).
      e.preventDefault();
      void clipClipboard.exportSelectionSmf();
    } else if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "d") {
      // Duplicate immediately after the selected clip.
      e.preventDefault();
      void midi.duplicateSelected();
    } else if (e.key === "+" || e.key === "=") {
      view.zoomAt(view.width / 2, 0.75);
    } else if (e.key === "-") {
      view.zoomAt(view.width / 2, 1.33);
    } else if (e.key.toLowerCase() === "s" && !e.metaKey && !e.ctrlKey) {
      view.snap = !view.snap;
    } else if (!e.metaKey && !e.ctrlKey && !e.altKey && dockTabForKey(e.key)) {
      toggleDock(dockTabForKey(e.key)!);
    } else if (e.key.toLowerCase() === "c" && !e.metaKey && !e.ctrlKey && !e.altKey) {
      // The library's CLIPS root is a destination in its own right, so it
      // gets its own key rather than "open the library, now find the tab".
      const onClips = ui.dock === "library" && library.root === "clips";
      library.root = "clips";
      ui.dock = onClips ? "" : "library";
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

<div class="app">
  <TransportBar />
  <div class="main">
    <Timeline />
    <Dock />
  </div>
  <PianoRoll />
  <MasterBar />
</div>

<McpConfirmDialog />
<ExportDialog />
<ProjectDialog />
<PreferencesDialog />
<Toasts />

<style>
  .app {
    /* % (not vh): under interface zoom, viewport units get multiplied by
       the zoom factor and would overflow the window; percentages resolve
       against the parent's real size (html/body are height: 100%). */
    height: 100%;
    display: flex;
    flex-direction: column;
    /* clip (not hidden): focus moves must never scroll the app shell */
    overflow: clip;
  }
  .main {
    flex: 1;
    min-height: 0;
    position: relative;
    display: flex;
    flex-direction: column;
  }
</style>
