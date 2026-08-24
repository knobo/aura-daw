<script lang="ts">
  /**
   * Sends popover (Plan G2): the list of bus returns this track feeds, with
   * an amount knob, a pre/post tap toggle and a remove button per row, plus
   * a picker of the buses it does not feed yet.
   *
   * Same popover pattern as InsertChain, hung off the SENDS chip in the
   * track-header metadata row.
   */
  import type { SendSlot, TrackState } from "../../types/ipc";
  import { project } from "../../state/project.svelte";
  import { toasts } from "../../state/toasts.svelte";
  import { backend } from "../../tauri";

  let {
    track,
    onclose,
  }: {
    track: TrackState;
    onclose: () => void;
  } = $props();

  const sends = $derived(track.sends ?? []);
  const buses = $derived(project.tracks.filter((t) => t.kind === "bus"));
  const unused = $derived(buses.filter((b) => !sends.some((s) => s.dest === b.id)));
  const busy = $state<Record<string, boolean>>({});

  function busName(id: string): string {
    return buses.find((b) => b.id === id)?.name ?? "(missing bus)";
  }

  /** The slider bottoms out at -60 dB and MEANS silence there: the document
   * encodes -inf as -160, the same convention `TrackState.gainDb` uses, so
   * a send pulled all the way down is actually off rather than 60 dB down. */
  const FLOOR_DB = -60;
  function toDoc(v: number): number {
    return v <= FLOOR_DB ? -160 : v;
  }
  function fromDoc(v: number): number {
    return v <= FLOOR_DB ? FLOOR_DB : v;
  }
  function label(v: number): string {
    return v <= FLOOR_DB ? "-inf" : `${v > 0 ? "+" : ""}${v.toFixed(1)} dB`;
  }

  // ── gesture bracket, mirroring TrackHeader's fader drag ──
  let token: Promise<string | undefined> | undefined;
  let tail: Promise<void> = Promise.resolve();
  function settle(op: Promise<unknown>): Promise<void> {
    return op.then(
      () => undefined,
      () => undefined,
    );
  }
  function openGesture() {
    token = tail.then(() => project.beginGesture("send amount"));
    tail = settle(token);
  }
  function closeGesture() {
    const idp = token;
    token = undefined;
    if (!idp) return;
    tail = settle(
      tail.then(async () => {
        const id = await idp;
        await project.endGesture(id);
      }),
    );
  }

  function onAmount(slot: SendSlot, e: Event) {
    const v = parseFloat((e.currentTarget as HTMLInputElement).value);
    // Optimistic: the knob has to follow the pointer, and the backend echoes
    // the document back through `project://changed` anyway.
    slot.amountDb = toDoc(v);
    tail = settle(
      tail.then(() => backend.sendSetAmount?.(track.id, slot.id, toDoc(v)) ?? Promise.resolve()),
    );
  }

  async function addSend(destId: string) {
    busy[`add:${destId}`] = true;
    try {
      await backend.sendAdd?.(track.id, destId);
      await project.reload();
    } catch (err) {
      console.error("[aura] send_add failed:", err);
      toasts.error("SEND FAILED", String(err));
    } finally {
      busy[`add:${destId}`] = false;
    }
  }

  async function removeSend(slot: SendSlot) {
    busy[slot.id] = true;
    try {
      await backend.sendRemove?.(track.id, slot.id);
      await project.reload();
    } catch (err) {
      console.error("[aura] send_remove failed:", err);
      toasts.error("SEND REMOVE FAILED", String(err));
    } finally {
      busy[slot.id] = false;
    }
  }

  async function toggleTap(slot: SendSlot) {
    busy[slot.id] = true;
    try {
      await backend.sendSetPreFader?.(track.id, slot.id, !slot.preFader);
      await project.reload();
    } catch (err) {
      console.error("[aura] send_set_pre_fader failed:", err);
      toasts.error("SEND TAP FAILED", String(err));
    } finally {
      busy[slot.id] = false;
    }
  }
</script>

<div class="backdrop" role="presentation" onpointerdown={onclose}></div>
<div class="popover glass mono" role="menu" aria-label="Sends for {track.name}">
  <div class="pophead">
    <span class="ptitle">Sends</span>
  </div>

  {#if sends.length > 0}
    <div class="slots">
      {#each sends as slot (slot.id)}
        <div class="slot">
          <div class="slot-row">
            <button
              class="tap mono"
              class:pre={slot.preFader}
              title={slot.preFader
                ? "Pre-fader: the send ignores this track's fader and mute"
                : "Post-fader: the send follows this track's fader and mute"}
              onclick={() => toggleTap(slot)}
              disabled={!!busy[slot.id]}
            >
              {slot.preFader ? "PRE" : "POST"}
            </button>
            <span class="dest">{busName(slot.dest)}</span>
            <span class="amt">{label(slot.amountDb)}</span>
            <button
              class="del mono"
              title="Remove send"
              onclick={() => removeSend(slot)}
              disabled={!!busy[slot.id]}
            >
              ×
            </button>
          </div>
          <input
            class="knob"
            type="range"
            min={FLOOR_DB}
            max="6"
            step="0.5"
            value={fromDoc(slot.amountDb)}
            aria-label="Send amount to {busName(slot.dest)}"
            oninput={(e) => onAmount(slot, e)}
            onpointerdown={openGesture}
            onpointerup={closeGesture}
            onpointercancel={closeGesture}
          />
        </div>
      {/each}
    </div>
  {:else}
    <span class="empty-silk">No sends yet</span>
  {/if}

  <div class="addlist">
    {#if buses.length === 0}
      <span class="empty-silk">No bus tracks — add one with + BUS</span>
    {:else if unused.length === 0}
      <span class="empty-silk">Sending to every bus</span>
    {:else}
      {#each unused as b (b.id)}
        <button
          class="busitem"
          disabled={!!busy[`add:${b.id}`]}
          onclick={() => addSend(b.id)}
        >
          <span class="bbadge mono">bus</span>
          <span class="bname">{b.name}</span>
        </button>
      {/each}
    {/if}
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 89;
  }
  .popover {
    position: absolute;
    top: calc(100% + 4px);
    left: 0;
    z-index: 90;
    min-width: 190px;
    max-width: 260px;
    max-height: 280px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    padding: 6px;
    border-radius: 7px;
    background: rgb(var(--bg-sunken-rgb) / 0.96);
    border: var(--border-width) solid var(--glass-border);
  }
  .pophead {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    margin-bottom: 4px;
  }
  .ptitle {
    font-size: 9px;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    color: var(--text-dim);
  }
  .slots {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .slot {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 3px 5px;
    border-radius: 4px;
    border: var(--border-width) solid rgb(var(--cyan-rgb) / 0.2);
    background: rgb(var(--bg-1-rgb) / 0.5);
  }
  .slot-row {
    display: flex;
    align-items: center;
    gap: 5px;
    min-height: 18px;
  }
  .tap {
    flex: none;
    height: 13px;
    line-height: 1;
    font-size: 7px;
    letter-spacing: 0.08em;
    border-radius: 2px;
    border: var(--border-width) solid rgb(var(--edge-rgb) / 0.2);
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
    padding: 0 3px;
  }
  .tap.pre {
    color: var(--bg-0);
    background: var(--amber);
    border-color: var(--amber);
  }
  .tap:hover:not(:disabled) {
    border-color: rgb(var(--cyan-rgb) / 0.4);
  }
  .dest {
    flex: 1;
    min-width: 0;
    font-size: 9px;
    color: var(--text);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .amt {
    flex: none;
    font-size: 8px;
    color: var(--text-dim);
  }
  .knob {
    width: 100%;
    height: 12px;
    margin: 0;
    accent-color: var(--cyan);
  }
  .del {
    flex: none;
    width: 12px;
    height: 12px;
    line-height: 1;
    border: none;
    background: transparent;
    color: var(--text-faint);
    font-size: 11px;
    cursor: pointer;
    padding: 0;
  }
  .del:hover:not(:disabled) {
    color: var(--red);
  }
  .addlist {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin-top: 4px;
    padding-top: 4px;
    border-top: var(--border-width) solid var(--glass-border);
  }
  .busitem {
    display: flex;
    align-items: center;
    gap: 6px;
    background: transparent;
    border: none;
    border-radius: 4px;
    padding: 4px 6px;
    font-size: 10px;
    cursor: pointer;
    text-align: left;
  }
  .busitem:hover:not(:disabled) {
    background: rgb(var(--cyan-rgb) / 0.09);
    color: var(--cyan);
  }
  .busitem:disabled {
    cursor: wait;
    opacity: 0.5;
  }
  .bbadge {
    flex: none;
    font-size: 7px;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    padding: 1px 3px;
    border-radius: 2px;
    border: var(--border-width) solid rgb(var(--cyan-rgb) / 0.4);
    color: var(--cyan);
    background: rgb(var(--cyan-rgb) / 0.08);
  }
  .bname {
    flex: 1;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .empty-silk {
    font-size: 9px;
    color: var(--text-faint);
    padding: 4px 0;
  }
</style>
