<script lang="ts">
  /**
   * The level meter on the control surface, in four faces. Which one you get
   * is the `meterFace` preference; none of them is a different component,
   * because all four read the same bus the same way and the only thing that
   * varies is what the numbers are drawn as.
   *
   * - **LED ARC** — a ring of segments around a sunken well with the number
   *   inside it. The face that matches the knob: same 270° travel, same
   *   lamps, so a rack of knobs and meters reads as one instrument rather
   *   than as two widgets that happen to be round.
   * - **DIAL** — the same well with a single thin arc filling round it and a
   *   fine needle riding its head. Quieter than the segments at a glance and
   *   the most precise of the four to read a small change on, because a
   *   continuous arc has no step size.
   * - **LADDER** — the horizontal PPM bargraph, in a routed slot. It is the
   *   only face that is not round, which is the point: in a row of channel
   *   strips the eye compares BAR LENGTHS across the row far faster than it
   *   compares angles, so this is the face for balancing a mix.
   * - **ANALOG VU** — the swinging needle over a printed scale. It reads
   *   ballistics better than any of the others: you see the SHAPE of a
   *   transient rather than a number that already happened.
   *
   * Every face carries a peak hold except the VU, which by construction
   * cannot: a needle IS the peak, and once it has fallen back the peak is
   * gone. That is the one real thing the old face could not do.
   *
   * No face carries a colour of its own. The lamp ramp is
   * `green → amber → red` off the palette, so a theme with no green in it is
   * not a meter with the wrong green in it.
   *
   * Range matches Meter.svelte (−60..+6 dBFS). Read-only.
   */
  import { latestMeter } from "../../state/meters.svelte";
  import { prefs } from "../../prefs/prefs.svelte";
  import { formatDb, linToDb } from "../../utils/format";
  import { runWhileVisible } from "../../utils/visible-raf";

  interface Props {
    trackId: string;
    label?: string;
    size?: number;
  }

  const { trackId, label, size = 88 }: Props = $props();

  const DB_MIN = -60;
  const DB_MAX = 6;
  /** The VU face's sweep. A real VU movement covers about 90°. */
  const VU_SWEEP = 90;
  const VU_START = -VU_SWEEP / 2;

  /** The round faces' sweep — the knob's, so the two agree by construction. */
  const SWEEP = 270;
  /** A circle's dash arithmetic in hundredths, via `pathLength="100"`. */
  const PATH = 100;
  const ARC = (PATH * SWEEP) / 360;
  /** Segments around the LED ring. Denser than the knob's, because a meter is
   * read at a glance and a coarser ring steps visibly under a fade. */
  const SEGS = 44;
  const SEG_R = 43;
  const SEG_W = 2.3;
  /** Rungs on the ladder. Fewer, because each one has to stay a rung at the
   * 72px a channel strip gives it. */
  const RUNGS = 26;
  const CX = 50;

  const face = $derived(prefs.values.meterFace);
  const round = $derived(face === "led" || face === "dial");

  /** Each lamp's angle from 12 o'clock and its position along the scale. */
  const segs = Array.from({ length: SEGS }, (_, i) => ({
    deg: -SWEEP / 2 + (i / (SEGS - 1)) * SWEEP,
    at: i / (SEGS - 1),
  }));
  const rungs = Array.from({ length: RUNGS }, (_, i) => i / (RUNGS - 1));

  /**
   * The lamp ramp. Green up to −12 dB, amber to 0, red above — the
   * boundaries a mixer already has in their head, expressed as positions
   * along the scale rather than as segment indices, so moving DB_MIN cannot
   * silently move where "hot" starts.
   */
  const HOT = (0 - DB_MIN) / (DB_MAX - DB_MIN);
  const WARM = (-12 - DB_MIN) / (DB_MAX - DB_MIN);

  /** 0..1 along the scale. */
  function normal(db: number): number {
    return Math.max(0, Math.min(1, (db - DB_MIN) / (DB_MAX - DB_MIN)));
  }
  function vuAngle(db: number): number {
    return VU_START + normal(db) * VU_SWEEP;
  }

  let rootEl: HTMLDivElement | undefined = $state();
  let needleEl: SVGGElement | undefined = $state();
  let readoutEl: HTMLElement | undefined = $state();
  let segsEl: SVGGElement | undefined = $state();
  let peakEl: SVGGElement | undefined = $state();
  let dialArcEl: SVGCircleElement | undefined = $state();
  let dialNeedleEl: SVGGElement | undefined = $state();
  let dialPeakEl: SVGGElement | undefined = $state();
  let ladderEl: HTMLElement | undefined = $state();
  let clipped = $state(false);

  /** The zone a level is in, as the class name every face paints it with. */
  function zoneOf(n: number): string {
    return n >= HOT ? "hot" : n >= WARM ? "warm" : "ok";
  }

  // Every face is written imperatively inside the rAF loop, and none of the
  // values it writes may be `$state`: ruling S-3 is that the meter bus does
  // not go on the reactivity graph, and add-all-tracks puts one gauge per
  // mixable track on the page. `clipped` is the one exception — a rare latch,
  // reset by a click, exactly as in Meter.svelte.
  $effect(() => {
    const id = trackId;
    const root = rootEl;
    const readout = readoutEl;
    const ring = segsEl;
    const peakLamp = peakEl;
    const swing = needleEl;
    const dialArc = dialArcEl;
    const dialNeedle = dialNeedleEl;
    const dialPeak = dialPeakEl;
    const ladder = ladderEl;
    if (!root) return;
    let disp = DB_MIN;
    let peakHold = DB_MIN;
    let peakUntil = 0;
    let shown = "";
    let litShown = -1;
    let peakShown = -1;
    let zoneShown = "";

    const tick = (now: number) => {
      const m = latestMeter(id);
      const peak = m ? linToDb(Math.max(m.peakL, m.peakR)) : DB_MIN;
      // Fast attack, slower release — a needle, not a bar.
      disp = peak > disp ? peak : disp + (peak - disp) * 0.18;
      const n = normal(disp);

      // Peak hold: the highest reading of the last second, shown ahead of the
      // level as its own mark. 1000ms is the AES convention, and it is long
      // enough to catch a transient you were not watching for.
      if (peak >= peakHold || now > peakUntil) {
        peakHold = peak;
        peakUntil = now + 1000;
      }
      const p = normal(peakHold);

      if (swing) swing.setAttribute("transform", `rotate(${vuAngle(disp)} 50 78)`);

      if (dialArc) {
        dialArc.setAttribute("stroke-dasharray", `${(n * ARC).toFixed(2)} ${PATH}`);
        const zone = zoneOf(n);
        if (zone !== zoneShown) {
          dialArc.classList.remove("ok", "warm", "hot");
          dialArc.classList.add(zone);
          zoneShown = zone;
        }
      }
      if (dialNeedle) {
        dialNeedle.setAttribute(
          "transform",
          `rotate(${-SWEEP / 2 + n * SWEEP} ${CX} ${CX})`,
        );
      }
      // The dial's peak is a tick sitting further round the same arc. It is
      // free of the rung quantisation the segment faces have, so it moves
      // continuously and gets written every frame rather than on a change.
      if (dialPeak) {
        dialPeak.setAttribute("transform", `rotate(${-SWEEP / 2 + p * SWEEP} ${CX} ${CX})`);
        dialPeak.style.opacity = peakHold > DB_MIN ? "1" : "0";
      }

      // The segment faces count rungs rather than tracking a fraction, so the
      // DOM write is skipped whenever the count has not changed — which is
      // most frames of most tracks.
      const cells = ring ?? ladder;
      const total = ring ? SEGS : RUNGS;
      if (cells) {
        const lit = Math.max(0, Math.min(total, Math.round(n * total)));
        if (lit !== litShown) {
          for (let i = 0; i < total; i++) {
            (cells.children[i] as HTMLElement).classList.toggle("on", i < lit);
          }
          litShown = lit;
        }
      }

      const mark = Math.max(0, Math.min(total - 1, Math.round(p * (total - 1))));
      if (mark !== peakShown) {
        if (peakLamp) {
          peakLamp.setAttribute("transform", `rotate(${segs[mark].deg} ${CX} ${CX})`);
          peakLamp.style.opacity = peakHold > DB_MIN ? "1" : "0";
        } else if (ladder) {
          if (peakShown >= 0) (ladder.children[peakShown] as HTMLElement).classList.remove("peak");
          if (peakHold > DB_MIN) (ladder.children[mark] as HTMLElement).classList.add("peak");
        }
        peakShown = mark;
      }

      if (readout) {
        const text = formatDb(disp);
        if (text !== shown) {
          readout.textContent = text;
          shown = text;
        }
      }
      if (m?.clipped) clipped = true;
    };
    return runWhileVisible(root, tick);
  });
</script>

<div bind:this={rootEl} class="gauge-unit" class:wide={face === "ladder"} style:--gauge-size="{size}px">
  {#if round}
    <div class="bezel led grain">
      <div class="window">
        <svg class="face" viewBox="0 0 100 100" aria-hidden="true">
          {#if face === "led"}
            <!-- Segments are `<line>`s pointing outward rather than dots, so
                 the ring reads as a bar graph bent round a circle: at a glance
                 the LENGTH of the lit run is the level, which a ring of equal
                 dots does not give you. -->
            <g bind:this={segsEl} class="segs">
              {#each segs as s (s.deg)}
                <line
                  class="seg"
                  class:warm={s.at >= WARM && s.at < HOT}
                  class:hot={s.at >= HOT}
                  x1={CX}
                  y1={CX - SEG_R + SEG_W * 2}
                  x2={CX}
                  y2={CX - SEG_R - SEG_W * 2}
                  transform="rotate({s.deg} {CX} {CX})"
                />
              {/each}
            </g>
            <g bind:this={peakEl} class="peak" opacity="0">
              <line x1={CX} y1={CX - SEG_R + SEG_W * 2} x2={CX} y2={CX - SEG_R - SEG_W * 2} />
            </g>
          {:else}
            <!-- `pathLength="100"` turns the dash arithmetic into hundredths
                 of a turn, so the sweep is a constant and the fill is one
                 multiplication — no circumference, no radius in the maths. -->
            <g transform="rotate(135 {CX} {CX})">
              <circle
                class="dial-track"
                cx={CX}
                cy={CX}
                r={SEG_R}
                pathLength={PATH}
                stroke-dasharray="{ARC} {PATH}"
              />
              <circle
                bind:this={dialArcEl}
                class="dial-arc ok"
                cx={CX}
                cy={CX}
                r={SEG_R}
                pathLength={PATH}
                stroke-dasharray="0 {PATH}"
              />
            </g>
            <g bind:this={dialPeakEl} class="dial-peak" opacity="0">
              <line x1={CX} y1={CX - SEG_R - SEG_W} x2={CX} y2={CX - SEG_R + SEG_W} />
            </g>
            <g bind:this={dialNeedleEl} class="dial-needle">
              <line x1={CX} y1={CX - SEG_R - SEG_W * 2} x2={CX} y2={CX - SEG_R + SEG_W * 4} />
            </g>
          {/if}
        </svg>
        <div class="glass"></div>
        <button
          class="clip"
          class:on={clipped}
          type="button"
          aria-label="Clip indicator — click to reset"
          onclick={() => (clipped = false)}
        ></button>
        <!-- The number lives INSIDE the well on the round faces: the ring is
             the glanceable reading and the digits the exact one, and putting
             both in the same object is what stops the unit needing two. -->
        <output class="mono well-value" bind:this={readoutEl}>-∞</output>
      </div>
    </div>
  {:else if face === "ladder"}
    <div class="slot grain">
      <div class="rungs" bind:this={ladderEl}>
        {#each rungs as at (at)}
          <i class="rung" class:warm={at >= WARM && at < HOT} class:hot={at >= HOT}></i>
        {/each}
      </div>
      <button
        class="clip"
        class:on={clipped}
        type="button"
        aria-label="Clip indicator — click to reset"
        onclick={() => (clipped = false)}
      ></button>
    </div>
  {:else}
    <div class="bezel grain">
      <div class="window">
        <svg class="face" viewBox="0 0 100 100" aria-hidden="true">
          <path class="arc" d="M18 78 A32 32 0 0 1 82 78" />
          {#each [-60, -24, -12, -6, 0, 6] as db (db)}
            <line
              class="tick"
              class:hot={db >= 0}
              x1="50"
              y1="48"
              x2="50"
              y2={db >= 0 ? 44 : 46}
              transform="rotate({vuAngle(db)} 50 78)"
            />
          {/each}
          <g bind:this={needleEl} class="needle">
            <line x1="50" y1="78" x2="50" y2="46" />
          </g>
          <circle class="hub" cx="50" cy="78" r="4.5" />
        </svg>
        <div class="glass"></div>
        <button
          class="clip"
          class:on={clipped}
          type="button"
          aria-label="Clip indicator — click to reset"
          onclick={() => (clipped = false)}
        ></button>
      </div>
    </div>
  {/if}

  {#if label}<span class="silk gauge-label">{label}</span>{/if}
  {#if !round}<output class="mono gauge-value" bind:this={readoutEl}>-∞</output>{/if}
</div>

<style>
  .gauge-unit {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 3px;
    width: var(--gauge-size);
  }

  /* ── the round faces share a bezel and a well ── */
  .bezel {
    position: relative;
    width: var(--gauge-size);
    height: var(--gauge-size);
    border-radius: 50%;
    background-color: var(--bg-3);
    background-image: var(--sheen-dome);
    box-shadow: var(--bevel-frame), var(--relief-2);
    padding: 7px;
  }
  /* The lit faces want a thinner collar: the ring IS the rim, and a wide
     milled one outside it leaves the lit part looking small. */
  .bezel.led {
    padding: 3px;
  }
  .window {
    position: relative;
    width: 100%;
    height: 100%;
    border-radius: 50%;
    background: var(--bg-sunken);
    box-shadow: var(--bevel-inset);
    overflow: hidden;
  }
  /* On the lit faces the sunken part is the DISC INSIDE the ring, not the
     whole window — the ring sits on the rim, level with the bezel, and the
     well is what it is ranged around. An inset ring rather than a smaller
     element, so the ring still has the full viewBox to draw in. */
  .bezel.led .window {
    background: var(--bg-2);
    box-shadow:
      inset 0 0 0 calc(11% + 1px) var(--bg-sunken),
      var(--bevel-inset);
  }
  .face {
    width: 100%;
    height: 100%;
    display: block;
  }

  /* ── LED ARC ──
     An unlit segment is a dark lens, not an absence: it is the printed scale,
     and it has to survive on a theme whose ground already IS the shadow
     colour, which is why it is `--line` and not `--shadow`. */
  .seg {
    stroke: rgb(var(--line-rgb) / 0.24);
    stroke-width: 2.3;
    stroke-linecap: round;
    transition: stroke 60ms linear;
  }
  /* The three zones are only ever WORN by a segment that is lit, so the whole
     ramp stays visible as printed scale while the track is silent.

     `:global(.on)` because the class is added by `classList.toggle` inside
     the rAF loop rather than by the template — Svelte cannot see that, and
     prunes the rule as unused if it is written plainly. */
  .seg:global(.on) {
    stroke: var(--green);
  }
  .seg.warm:global(.on) {
    stroke: var(--amber);
  }
  .seg.hot:global(.on) {
    stroke: var(--red);
  }
  /* One filter for the whole ring rather than one per segment — 44 of them
     re-evaluated every frame is a different component. The bloom is the
     colour of the meter, not of the moment, because a filter cannot read a
     child's own stroke. */
  .segs {
    filter: drop-shadow(0 0 calc(2.5px * var(--glow-scale)) rgb(var(--green-rgb) / 0.55));
  }
  /* Drawn in the text colour rather than in a zone colour, so a peak that
     touched the red does not keep reading as "you are in the red". */
  .peak line {
    stroke: var(--text);
    stroke-width: 2.6;
    stroke-linecap: round;
    filter: drop-shadow(0 0 calc(3px * var(--glow-scale)) rgb(var(--text-rgb) / 0.7));
    transition: opacity 120ms;
  }

  /* ── DIAL ── */
  .dial-track,
  .dial-arc {
    fill: none;
    stroke-width: 4;
    stroke-linecap: butt;
  }
  .dial-track {
    stroke: rgb(var(--line-rgb) / 0.28);
  }
  /* 40ms, not 90: the stroke crossfades while the level is falling through a
     zone boundary, and a slow one leaves the arc amber long enough after the
     reading has dropped back into the green that it reads as a bug. It looked
     like one for a while, which is how this comment got written. */
  .dial-arc {
    stroke: var(--green);
    transition: stroke 40ms linear;
    filter: drop-shadow(0 0 calc(3px * var(--glow-scale)) rgb(var(--green-rgb) / 0.5));
  }
  .dial-arc:global(.warm) {
    stroke: var(--amber);
  }
  .dial-arc:global(.hot) {
    stroke: var(--red);
  }
  /* A short needle riding the head of the arc rather than a full radius from
     the hub: the arc already says how far along the travel you are, so a
     needle across the whole face would say it twice and hide the number. */
  .dial-needle line {
    stroke: var(--text);
    stroke-width: 1.8;
    stroke-linecap: round;
  }
  .dial-peak line {
    stroke: var(--text);
    stroke-width: 1.4;
    stroke-linecap: round;
    opacity: 0.55;
    transition: opacity 120ms;
  }

  /* ── LADDER ──
     A routed slot with the rungs lying in it. `--bevel-frame-inset` rather
     than a border: the bar has to read as sunk into the panel, or it is a
     progress bar drawn on top of one. */
  .slot {
    position: relative;
    width: 100%;
    padding: 4px 5px;
    border-radius: var(--ctrl-radius);
    background: var(--bg-sunken);
    box-shadow: var(--bevel-frame-inset), var(--bevel-inset);
  }
  .rungs {
    display: flex;
    gap: 1px;
    height: 13px;
  }
  .rung {
    flex: 1;
    border-radius: 1px;
    background: rgb(var(--line-rgb) / 0.2);
    transition: background 60ms linear;
  }
  .rung:global(.on) {
    background: var(--green);
    box-shadow: 0 0 calc(4px * var(--glow-scale)) rgb(var(--green-rgb) / 0.6);
  }
  .rung.warm:global(.on) {
    background: var(--amber);
    box-shadow: 0 0 calc(4px * var(--glow-scale)) rgb(var(--amber-rgb) / 0.6);
  }
  .rung.hot:global(.on) {
    background: var(--red);
    box-shadow: 0 0 calc(4px * var(--glow-scale)) rgb(var(--red-rgb) / 0.6);
  }
  /* The peak rung wins over its zone colour whether it is lit or not — it is
     ahead of the bar most of the time, which is the whole point of it. */
  .rungs .rung:global(.peak) {
    background: var(--text);
    box-shadow: 0 0 calc(4px * var(--glow-scale)) rgb(var(--text-rgb) / 0.7);
  }

  /* ── the VU face ── */
  .arc {
    fill: none;
    stroke: rgb(var(--line-rgb) / 0.35);
    stroke-width: 1.6;
  }
  .tick {
    stroke: rgb(var(--line-rgb) / 0.55);
    stroke-width: 1.4;
  }
  .tick.hot {
    stroke: var(--red);
  }
  .needle line {
    stroke: var(--amber);
    stroke-width: 1.6;
    stroke-linecap: round;
    filter: drop-shadow(0 0 calc(3px * var(--glow-scale)) var(--amber));
  }
  .hub {
    fill: var(--bg-3);
    stroke: var(--edge);
    stroke-width: 0.8;
  }

  /* ── shared trim ── */
  .well-value {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    font-size: calc(var(--gauge-size) * 0.17);
    letter-spacing: 0.02em;
    color: var(--text-mid);
    pointer-events: none;
  }
  .glass {
    position: absolute;
    inset: 0;
    border-radius: 50%;
    background-image: var(--sheen-dome);
    opacity: calc(var(--sheen) * 0.55);
    pointer-events: none;
  }
  .clip {
    position: absolute;
    top: 10%;
    right: 18%;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    border: none;
    padding: 0;
    background: rgb(var(--line-rgb) / 0.3);
    cursor: pointer;
  }
  /* The clip lamp belongs on the printed face of the VU, but on the lit faces
     that spot is the ring itself, so it moves into the well; on the ladder it
     goes to the hot end of the slot, where a clip lamp is on real gear. */
  .bezel.led .clip {
    top: 26%;
    right: 50%;
    margin-right: -3.5px;
  }
  /* Set INTO the hot end of the slot rather than beside it: hung outside, it
     reads as a stray dot on the panel and it is the first thing a narrower
     channel strip clips off. */
  .slot .clip {
    top: 50%;
    right: 7px;
    margin-top: -3.5px;
    box-shadow: inset 0 0 0 1px rgb(var(--shadow-rgb) / calc(var(--bevel) * 0.6));
  }
  .clip.on {
    background: var(--red);
    box-shadow: 0 0 calc(8px * var(--glow-scale)) var(--red);
  }
  .gauge-label {
    line-height: 1;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .gauge-value {
    font-size: 10px;
    color: var(--text-mid);
    line-height: 1;
  }
</style>
