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
  import { backend } from "../../tauri";
  import { pitchMode } from "../../state/pitch.svelte";

  let held = $state(false);

  async function set(on: boolean) {
    if (held === on) return;
    held = on;
    try {
      await backend.setRehearseHold(on);
    } catch (err) {
      console.warn("[aura] set_rehearse_hold failed:", err);
      held = false;
    }
  }
</script>

<button
  type="button"
  class="rehearse mono"
  class:on={pitchMode.rehearseHold}
  title="Hold to rehearse: the transport rolls, the take records silence"
  onpointerdown={(e) => {
    e.preventDefault();
    void set(true);
  }}
  onpointerup={() => void set(false)}
  onpointercancel={() => void set(false)}
  onpointerleave={() => void set(false)}
  onblur={() => void set(false)}
>
  REHEARSE
</button>

<style>
  .rehearse {
    font-size: 9px;
    letter-spacing: 0.18em;
    padding: 3px 10px;
    border-radius: 999px;
    border: 1px solid var(--glass-border);
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
    border-color: rgba(255, 200, 87, 0.5);
    box-shadow: 0 0 14px rgba(255, 200, 87, 0.25);
  }
  @media (prefers-reduced-motion: reduce) {
    .rehearse {
      transition: none;
    }
  }
</style>
