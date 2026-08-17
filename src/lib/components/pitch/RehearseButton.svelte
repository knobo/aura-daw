<script lang="ts">
  /**
   * Press-and-hold rehearse (owner ruling R5). While it is down the take
   * writes SILENCE for the held span — the transport keeps rolling and the
   * take stays sample-aligned, so a rehearsed bar is a hole, not a shift.
   *
   * `pointerleave` releases as well as `pointerup`. A pointer dragged off
   * the button must not leave the take silently discarding audio — that is
   * the difference between a rehearsal and a lost verse.
   */
  import { pitchMode } from "../../state/pitch.svelte";
  import { setRehearseSource } from "../../state/rehearse.svelte";
  import { transport } from "../../state/transport.svelte";

  /**
   * The button is one of two sources for a single engine-side hold; the key
   * in App.svelte is the other. Both go through `setRehearseSource`, which
   * counts them — a private flag here would let releasing the button end a
   * hold the key is still asserting, and the take would start writing real
   * audio mid-rehearsal.
   *
   * Holding this before recording starts is deliberate (matches the "h" key
   * in App.svelte, live whenever the coach is open): pre-arm here, then hit
   * record with the take starting silent from sample 0. Only the visible
   * feedback while idle is wrong — see the veil in PitchCoach.svelte, gated
   * on `transport.isRecording` so it stops claiming a take is being silenced
   * when there is no take.
   */
  function set(on: boolean) {
    setRehearseSource("button", on);
  }
</script>

<button
  type="button"
  class="rehearse mono"
  class:on={pitchMode.rehearseHold}
  title={transport.isRecording
    ? "Hold to rehearse: the transport rolls, the take records silence"
    : "Hold to arm rehearse: the next take will start silent while held"}
  onpointerdown={(e) => {
    e.preventDefault();
    set(true);
  }}
  onpointerup={() => set(false)}
  onpointercancel={() => set(false)}
  onpointerleave={() => set(false)}
  onblur={() => set(false)}
>
  REHEARSE
</button>

<style>
  .rehearse {
    font-size: 9px;
    letter-spacing: 0.18em;
    padding: 3px 10px;
    border-radius: 999px;
    border: var(--border-width) solid var(--glass-border);
    background: transparent;
    color: var(--text-dim);
    cursor: pointer;
    transition:
      color 90ms ease,
      border-color 90ms ease,
      box-shadow 90ms ease;
  }
  .rehearse:hover {
    color: var(--text);
  }
  .rehearse.on {
    color: var(--amber);
    border-color: rgb(var(--amber-rgb) / 0.5);
    box-shadow: 0 0 calc(14px * var(--glow-scale)) rgb(var(--amber-rgb) / 0.25);
  }
  @media (prefers-reduced-motion: reduce) {
    .rehearse {
      transition: none;
    }
  }
</style>
