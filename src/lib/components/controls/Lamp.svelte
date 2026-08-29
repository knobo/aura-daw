<script lang="ts">
  /**
   * Channel-strip key: mute / solo / arm. Click toggles.
   *
   * Two shapes, chosen by the `stripKeys` preference and passed in as
   * `variant`, because the row they sit in is 76px wide and there is more
   * than one honest answer to that:
   *
   * - **KEYS** — three keys standing proud of the panel, each moulded on all
   *   four sides. Lit, a key presses IN and its legend lights: the shape of
   *   every mixer key made since the 1970s.
   * - **SEGMENTED** — one routed slot with the three functions divided
   *   inside it by hairlines. Lit, a segment fills solid. It reads as one
   *   switch rather than three buttons, which is the truer description —
   *   they belong to the same channel and you never think about them apart.
   *
   * ## Why `compact` exists
   *
   * The row was 28px per key with the word MUTE in it, three of them plus
   * gaps: 92px of content inside a 76px box, so the outer keys sat ON the
   * strip's own bevel and the row started at x=0 of a strip with 8px
   * padding. Measured, not eyeballed.
   *
   * Two of those 16px could have come off the gaps, and the rest could have
   * come off the type — which is how the row got to 8px type with a 0.12em
   * letter-spacing in the first place. A `compact` key drops the word for
   * the single letter instead, which is what the hardware does and for the
   * same reason: at this size a four-letter word is an unreadable smudge,
   * and one letter at 10px is legible across a room. The word stays on the
   * button's `title` and its accessible name, so nothing is lost to anyone
   * who cannot see the layout that made the letter necessary.
   *
   * A lamp placed as a free-standing widget on the deck has room for the
   * word and keeps it.
   */
  interface Props {
    on: boolean;
    label: string;
    /** Visual role — colours the lit state, and picks the compact glyph. */
    role?: "mute" | "solo" | "arm";
    /** The shape. See above; the caller reads this off the preference. */
    variant?: "key" | "bar";
    /** Show the single-letter glyph rather than the label. */
    compact?: boolean;
    ariaLabel: string;
    onclick?: () => void;
  }

  const {
    on,
    label,
    role = "mute",
    variant = "key",
    compact = false,
    ariaLabel,
    onclick,
  }: Props = $props();

  /**
   * A, not R, and not because A is the industry convention — it is not. Most
   * DAWs draw record-arm as a red dot and hardware surfaces print REC; a bare
   * letter is nobody's standard.
   *
   * It is A because it is AURA's: the track header and the lane group header
   * both already say A M S (TrackHeader.svelte, LaneGroupHeader.svelte), and
   * a strip that says R for the same function teaches two names for one
   * thing. And R is taken — AutomationModeSelector prints R for automation
   * Read, in the same track header row, so R on a strip would be the same
   * letter meaning two things in one window.
   */
  const GLYPH = { mute: "M", solo: "S", arm: "A" } as const;
</script>

<button
  class="lamp grain"
  class:on
  class:key={variant === "key"}
  class:bar={variant === "bar"}
  class:compact
  class:mute={role === "mute"}
  class:solo={role === "solo"}
  class:arm={role === "arm"}
  type="button"
  aria-pressed={on}
  aria-label={ariaLabel}
  title={label}
  {onclick}
>
  <span class="legend">{compact ? GLYPH[role] : label}</span>
</button>

<style>
  /* Each role names its own colour once, so every lit rule below reads
     `var(--role)` and neither variant repeats the three-way branch. */
  .lamp.mute {
    --role: var(--red);
    --role-rgb: var(--red-rgb);
  }
  .lamp.solo {
    --role: var(--amber);
    --role-rgb: var(--amber-rgb);
  }
  .lamp.arm {
    --role: var(--red-soft);
    --role-rgb: var(--red-rgb);
  }

  .lamp {
    position: relative;
    display: grid;
    place-items: center;
    /* `min-width: 0` is the line that makes the overflow impossible rather
       than merely unlikely: a grid item defaults to `min-width: auto`, which
       is its CONTENT's width, and a track told to be `1fr` will still refuse
       to go below that. Without it the row grows back the moment a label is
       long enough — which is exactly how the 92px row happened. */
    min-width: 0;
    width: 100%;
    font-family: var(--font-mono);
    font-weight: 600;
    letter-spacing: 0.1em;
    cursor: pointer;
    border: none;
    transition: color 90ms, box-shadow 120ms, transform 90ms, background-color 120ms;
  }
  .lamp.compact {
    letter-spacing: 0;
  }
  /* `width: 100%` is right inside the strip's grid, where the track sets the
     width. Free on the deck there is no track to set it, and a percentage
     width contributes nothing to a shrink-to-fit parent's max-content — so a
     lamp there needs a floor of its own. `compact` is exactly the signal for
     which case this is: it means "your container is telling you the width". */
  .lamp:not(.compact) {
    min-width: 42px;
  }
  .legend {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 100%;
  }
  .lamp:focus-visible {
    outline: var(--focus-width) solid var(--cyan);
    outline-offset: 1px;
    z-index: 1;
  }

  /* ── KEYS ── a moulded key standing on the panel. */
  .lamp.key {
    height: 21px;
    padding: 0 3px;
    border-radius: var(--ctrl-radius);
    background-color: var(--bg-2);
    background-image: var(--sheen-face);
    box-shadow: var(--bevel-frame), var(--relief-1);
    color: var(--text-dim);
    font-size: 10px;
  }
  .lamp.key:not(.compact) {
    font-size: 8px;
  }
  .lamp.key:hover {
    color: var(--text);
  }
  /* Lit: the key presses in and its legend lights, with a bar along the
     bottom lip — the `.module.lit` idiom, where the legend carries the state
     and the face does not. Deliberately NOT a solid accent fill with the
     letter knocked out: `--text-on-accent` is one value per theme, and the
     one that reads on red does not read on amber. */
  .lamp.key.on {
    color: var(--role);
    background-color: var(--bg-sunken);
    background-image: linear-gradient(
      rgb(var(--role-rgb) / 0.14),
      rgb(var(--role-rgb) / 0.14)
    );
    box-shadow:
      var(--bevel-inset),
      inset 0 -2px 0 0 var(--role),
      0 0 calc(7px * var(--glow-scale)) rgb(var(--role-rgb) / 0.4);
    transform: translateY(calc(1px * var(--relief)));
  }

  /* ── SEGMENTED ── one routed slot, divided by hairlines.
     The corners are rounded by position rather than by a prop, so the row
     needs no wrapper that knows how many keys are in it — and a lamp placed
     on its own as a deck widget is `:only-child` and comes out as a normal
     rounded button. */
  .lamp.bar {
    height: 19px;
    padding: 0 2px;
    border-radius: 0;
    background-color: var(--bg-sunken);
    box-shadow:
      var(--bevel-inset),
      inset -1px 0 0 0 rgb(var(--line-rgb) / 0.22);
    color: var(--text-faint);
    font-size: 9px;
  }
  .lamp.bar:hover {
    color: var(--text-mid);
  }
  .lamp.bar:first-child {
    border-radius: var(--ctrl-radius) 0 0 var(--ctrl-radius);
  }
  /* No divider after the last one — a hairline against the slot's own right
     wall reads as a rendering seam, not as a division. */
  .lamp.bar:last-child {
    border-radius: 0 var(--ctrl-radius) var(--ctrl-radius) 0;
    box-shadow: var(--bevel-inset);
  }
  .lamp.bar:only-child {
    border-radius: var(--ctrl-radius);
    box-shadow: var(--bevel-inset);
  }
  /* Lit: the segment fills. The legend is knocked out in the WELL colour
     rather than in `--text-on-accent`, and that is what makes it safe on
     every theme: the well sits on the same side of the surface ramp as the
     panel, and an accent is by construction chosen to contrast with the
     panel — so the knock-out contrasts on a dark theme and on a light one
     without either needing a rule of its own. */
  .lamp.bar.on {
    color: var(--bg-sunken);
    background-color: var(--role);
    background-image: var(--sheen-face);
    box-shadow:
      inset 0 var(--border-width) 0 0 var(--bevel-hi),
      0 0 calc(8px * var(--glow-scale)) rgb(var(--role-rgb) / 0.45);
  }
</style>
