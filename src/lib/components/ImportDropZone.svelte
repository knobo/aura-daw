<script lang="ts">
  /**
   * Timeline drag-and-drop import: drop audio anywhere over the lanes.
   * Two landing halves — plain import, or import + Demucs stem split
   * (import_audio_clip_split_stems). Real app: Tauri webview drag-drop
   * events (HTML5 drags are swallowed by the native handler); demo mode:
   * plain browser DnD with fabricated paths.
   */
  import { onMount } from "svelte";
  import { backend } from "../tauri";
  import { jobs } from "../state/jobs.svelte";

  let rootEl: HTMLDivElement | undefined = $state();
  let active = $state(false);
  let overSplit = $state(false);

  function zoneAt(clientX: number): boolean {
    const rect = rootEl?.getBoundingClientRect();
    if (!rect) return false;
    return clientX - rect.left > rect.width / 2;
  }

  function insideLanes(clientX: number, clientY: number): boolean {
    const rect = rootEl?.getBoundingClientRect();
    if (!rect) return false;
    return (
      clientX >= rect.left && clientX <= rect.right && clientY >= rect.top && clientY <= rect.bottom
    );
  }

  async function importPaths(paths: string[], split: boolean) {
    for (const path of paths) {
      await jobs.importDropped(path, split);
    }
  }

  onMount(() => {
    // ── real app: webview drag-drop events ──
    if (backend.onFileDrop) {
      return backend.onFileDrop((e) => {
        if (e.type === "leave") {
          active = false;
          return;
        }
        if (e.type === "enter" || e.type === "over") {
          active = true;
          overSplit = zoneAt(e.x);
          return;
        }
        // drop
        active = false;
        if (e.paths.length && insideLanes(e.x, e.y)) {
          void importPaths(e.paths, zoneAt(e.x));
        }
      });
    }

    // ── demo mode: browser DnD (files carry names only — fabricate paths) ──
    let depth = 0;
    const hasFiles = (e: DragEvent) => !!e.dataTransfer?.types.includes("Files");
    const onEnter = (e: DragEvent) => {
      if (!hasFiles(e)) return;
      e.preventDefault();
      depth++;
      active = true;
    };
    const onOver = (e: DragEvent) => {
      if (!hasFiles(e)) return;
      e.preventDefault(); // required for drop to fire
      if (e.dataTransfer) e.dataTransfer.dropEffect = "copy";
      overSplit = zoneAt(e.clientX);
    };
    const onLeave = (e: DragEvent) => {
      if (!hasFiles(e)) return;
      depth = Math.max(0, depth - 1);
      if (depth === 0) active = false;
    };
    const onDrop = (e: DragEvent) => {
      if (!hasFiles(e)) return;
      e.preventDefault();
      depth = 0;
      active = false;
      if (!insideLanes(e.clientX, e.clientY)) return;
      const split = zoneAt(e.clientX);
      const paths = Array.from(e.dataTransfer?.files ?? []).map(
        (f) => `/demo/dropped/${f.name}`,
      );
      if (paths.length) void importPaths(paths, split);
    };
    window.addEventListener("dragenter", onEnter);
    window.addEventListener("dragover", onOver);
    window.addEventListener("dragleave", onLeave);
    window.addEventListener("drop", onDrop);
    return () => {
      window.removeEventListener("dragenter", onEnter);
      window.removeEventListener("dragover", onOver);
      window.removeEventListener("dragleave", onLeave);
      window.removeEventListener("drop", onDrop);
    };
  });
</script>

<div class="droproot" bind:this={rootEl} class:active>
  {#if active}
    <div class="zones">
      <div class="zone" class:hot={!overSplit}>
        <div class="icon">⤓</div>
        <div class="zlabel mono">IMPORT AUDIO</div>
        <div class="zhint silk">lands on a new track</div>
      </div>
      <div class="zone split" class:hot={overSplit}>
        <div class="icon">◫</div>
        <div class="zlabel mono">IMPORT + SPLIT STEMS</div>
        <div class="zhint silk">demucs → drums · bass · vocals · other</div>
      </div>
    </div>
    <div class="exts silk">wav · mp3 · flac · ogg · aac · m4a</div>
  {/if}
</div>

<style>
  .droproot {
    position: absolute;
    inset: 0;
    z-index: 25;
    pointer-events: none;
    display: none;
  }
  .droproot.active {
    display: flex;
    flex-direction: column;
    background: rgba(3, 4, 8, 0.72);
    backdrop-filter: blur(2px);
    animation: drop-in 120ms ease-out;
  }
  @keyframes drop-in {
    from {
      opacity: 0;
    }
  }

  .zones {
    flex: 1;
    min-height: 0;
    display: flex;
    gap: 10px;
    padding: 14px;
  }
  .zone {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 6px;
    border: 1px dashed rgba(122, 160, 220, 0.35);
    border-radius: 10px;
    color: var(--text-dim);
    transition: border-color 100ms, box-shadow 100ms, background 100ms;
  }
  .zone.hot {
    border-color: var(--cyan);
    background: rgba(82, 229, 255, 0.06);
    box-shadow:
      inset 0 0 30px rgba(82, 229, 255, 0.08),
      0 0 22px rgba(82, 229, 255, 0.15);
    color: var(--cyan);
  }
  .zone.split.hot {
    border-color: var(--magenta);
    background: rgba(255, 79, 216, 0.06);
    box-shadow:
      inset 0 0 30px rgba(255, 79, 216, 0.08),
      0 0 22px rgba(255, 79, 216, 0.15);
    color: var(--magenta);
  }
  .icon {
    font-size: 26px;
    line-height: 1;
  }
  .zlabel {
    font-size: 11px;
    letter-spacing: 0.22em;
  }
  .zhint {
    letter-spacing: 0.14em;
  }

  .exts {
    text-align: center;
    padding: 0 0 10px;
    letter-spacing: 0.2em;
  }
</style>
