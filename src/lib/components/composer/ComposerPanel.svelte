<script lang="ts">
  /**
   * ♪ COMPOSER — the panel over the harmony document.
   *
   * Four things, in the order a beginner needs them (product doc §3's
   * assistance ladder, top to bottom): the KEY and the circle to pick chords
   * with, the PROGRESSION you have so far, GENERATE to turn it into parts, and
   * WHY — the sentence for whatever is selected. Every explanation on this
   * panel came from the backend with the thing it explains; none of it is
   * written here.
   */
  import { onMount } from "svelte";
  import { composer, PARTS, type Part } from "../../state/composer.svelte";
  import { midi } from "../../state/midi.svelte";
  import CircleOfFifths from "./CircleOfFifths.svelte";
  import ProgressionStrip from "./ProgressionStrip.svelte";

  const TONICS = ["C", "G", "D", "A", "E", "B", "F#", "Db", "Ab", "Eb", "Bb", "F"];
  const SCALES = [
    { id: "ionian", label: "major" },
    { id: "aeolian", label: "minor" },
    { id: "dorian", label: "dorian" },
    { id: "mixolydian", label: "mixolydian" },
    { id: "lydian", label: "lydian" },
    { id: "phrygian", label: "phrygian" },
    { id: "harmonicMinor", label: "harmonic minor" },
    { id: "majorPentatonic", label: "major pentatonic" },
    { id: "minorPentatonic", label: "minor pentatonic" },
    { id: "blues", label: "blues" },
  ];

  let tonic = $state("C");
  let scale = $state("ionian");
  let plan = $state("axis");
  let bars = $state(8);
  let genre = $state("rock");
  let voicing = $state("close");
  let rhythm = $state("block");
  let bassStyle = $state("root");
  let density = $state(45);
  let syncopation = $state(20);
  let chosen = $state<Record<Part, boolean>>({ chords: true, bass: true, melody: true, drums: true });
  let atBar = $state(1);

  const view = $derived(composer.view);
  const selected = $derived(composer.selectedSpan);
  const lastAnnotations = $derived(
    (composer.lastGenerate?.clips ?? []).flatMap((c) =>
      c.annotations.map((a) => ({ part: c.part, ...a })),
    ),
  );
  const parts = $derived(PARTS.filter((p) => chosen[p]));
  const planLabel = $derived(
    view?.schemas.find((s) => s.id === plan)?.label ??
      (plan === "circle" ? "circle of fifths walk" : "functional grammar"),
  );
  const planWhy = $derived(
    view?.schemas.find((s) => s.id === plan)?.why ??
      (plan === "circle"
        ? "Every chord is a fifth above the next, so each acts as the dominant of the one that follows — which is why falling fifths pull forward."
        : "Generated from the key's functional grammar: tonic departs, predominant prepares, dominant resolves, and the last two bars are a cadence."),
  );

  /** Sharps/flats as a musician reads them. */
  function signature(fifths: number): string {
    if (fifths === 0) return "no accidentals";
    const n = Math.abs(fifths);
    return `${n} ${fifths > 0 ? "sharp" : "flat"}${n === 1 ? "" : "s"}`;
  }

  onMount(() => void composer.init());

  // The panel's cursor is a BAR, because that is the unit a progression is
  // written in; ticks are the wire form.
  $effect(() => {
    composer.atTicks = Math.max(0, Math.round((atBar - 1) * composer.ticksPerBar));
  });

  // Keep the key selects showing what the document actually says, including
  // after an undo (which re-pulls the view).
  $effect(() => {
    const canonical = view?.key;
    if (!canonical) return;
    const [t, s] = canonical.split(" ");
    if (t && TONICS.includes(t)) tonic = t;
    if (s) scale = s;
  });

  async function applyKey() {
    await composer.setKey(`${tonic} ${scale}`);
    await composer.loadSuggestions();
  }

  async function generate() {
    await composer.generate({
      parts,
      plan: plan || null,
      bars,
      genre,
      voicing,
      rhythm,
      bassStyle,
      density,
      syncopation,
    });
    await composer.loadSuggestions();
  }

  async function generateParts() {
    // Arrange what is already there: no plan, so the harmony document stands.
    await composer.generate({ parts, plan: null, bars });
  }

  async function pickChord(symbol: string) {
    await composer.appendChord(symbol);
    await composer.loadSuggestions();
  }
</script>

<div class="composer">
  {#if composer.unavailable}
    <p class="note mono">
      This build's backend has no composer commands. Rebuild the Rust side to use the Composer.
    </p>
  {/if}

  <!-- KEY -->
  <section>
    <header class="mono">
      KEY <span class="sig">{view ? `${view.keyLabel} · ${signature(view.keySignature)}` : "…"}</span>
    </header>
    <div class="row">
      <select bind:value={tonic} onchange={applyKey} aria-label="Tonic">
        {#each TONICS as t (t)}<option value={t}>{t}</option>{/each}
      </select>
      <select bind:value={scale} onchange={applyKey} aria-label="Scale">
        {#each SCALES as s (s.id)}<option value={s.id}>{s.label}</option>{/each}
      </select>
      <span class="spacer"></span>
      <label class="bar mono">
        BAR
        <input type="number" min="1" max="999" bind:value={atBar} aria-label="Start bar" />
      </label>
    </div>
    {#if view}
      <CircleOfFifths wedges={view.wedges} onpick={pickChord} />
    {/if}
  </section>

  <!-- PROGRESSION -->
  <section>
    <header class="mono">
      PROGRESSION
      {#if composer.chords.length > 0}
        <button class="link mono" onclick={() => composer.clear()}>clear</button>
      {/if}
    </header>
    <ProgressionStrip
      spans={view?.spans ?? []}
      selectedTick={composer.selectedTick}
      ticksPerBar={composer.ticksPerBar}
      onselect={(t) => composer.select(t)}
      onremove={(t) => composer.removeChordAt(t)}
    />
    {#if composer.suggestions.length > 0}
      <div class="suggest">
        <span class="mono label">NEXT</span>
        {#each composer.suggestions.slice(0, 4) as s (s.chord)}
          <button class="sug" title={s.why} onclick={() => pickChord(s.chord)}>
            <span>{s.chord}</span>
            <span class="mono roman">{s.roman}</span>
            <span class="mono score">{Math.round(s.score * 100)}</span>
          </button>
        {/each}
      </div>
    {:else if view}
      <button class="link mono" onclick={() => composer.loadSuggestions()}>what comes next?</button>
    {/if}
  </section>

  <!-- GENERATE -->
  <section>
    <header class="mono">GENERATE</header>
    <div class="row">
      <select bind:value={plan} aria-label="Progression plan">
        {#each view?.schemas ?? [] as s (s.id)}<option value={s.id}>{s.label}</option>{/each}
        <option value="circle">circle of fifths (walk)</option>
        <option value="functional:30">functional — safe</option>
        <option value="functional:70">functional — adventurous</option>
        <option value="functional:100">functional — reckless</option>
      </select>
      <label class="bar mono">
        BARS
        <input type="number" min="1" max="64" bind:value={bars} aria-label="Bars" />
      </label>
    </div>
    <p class="why small">{planWhy}</p>

    <div class="parts">
      {#each PARTS as p (p)}
        <label class="part mono" class:on={chosen[p]}>
          <input type="checkbox" bind:checked={chosen[p]} />
          {p}
        </label>
      {/each}
    </div>

    <div class="row wrap">
      <select bind:value={genre} aria-label="Drum genre" disabled={!chosen.drums}>
        {#each view?.genres ?? [] as g (g)}<option value={g}>{g}</option>{/each}
      </select>
      <select bind:value={voicing} aria-label="Chord voicing" disabled={!chosen.chords}>
        {#each view?.voicingStyles ?? [] as v (v)}<option value={v}>{v}</option>{/each}
      </select>
      <select bind:value={rhythm} aria-label="Chord rhythm" disabled={!chosen.chords}>
        <option value="block">block</option>
        <option value="arpeggio">arpeggio</option>
      </select>
      <select bind:value={bassStyle} aria-label="Bass style" disabled={!chosen.bass}>
        {#each view?.bassStyles ?? [] as b (b)}<option value={b}>{b}</option>{/each}
      </select>
    </div>

    <label class="slider mono">
      DENSITY <input type="range" min="0" max="100" bind:value={density} />
      <span>{density}</span>
    </label>
    <label class="slider mono">
      SYNCOPATION <input type="range" min="0" max="100" bind:value={syncopation} />
      <span>{syncopation}</span>
    </label>

    <div class="row">
      <label class="bar mono seed">
        SEED
        <input type="number" min="0" bind:value={composer.seed} aria-label="Seed" />
      </label>
      <button class="dice mono" title="A new seed — the same settings, a different idea" onclick={() => composer.rollSeed()}>
        ↻
      </button>
      <span class="spacer"></span>
      <button class="go mono" disabled={composer.busy || parts.length === 0} onclick={generate}>
        {composer.busy ? "…" : `${planLabel.split(" ")[0]} ▸`}
      </button>
    </div>
    <button
      class="link mono"
      disabled={composer.busy || parts.length === 0 || composer.chords.length === 0}
      onclick={generateParts}
      title="Write the selected parts against the progression you already have"
    >
      parts only — keep my progression
    </button>
  </section>

  <!-- WHY -->
  <section class="whys">
    <header class="mono">WHY</header>
    {#if selected}
      <p class="why">
        <strong class="mono">{selected.pretty} · {selected.roman}</strong>
        {selected.why}
      </p>
      {#if selected.tones.length > 0}
        <p class="small mono">tones: {selected.tones.join(" ")}</p>
      {/if}
    {/if}
    {#if composer.lastGenerate?.progression}
      <p class="why">
        <strong class="mono">{composer.lastGenerate.progression.keyLabel}</strong>
        {composer.lastGenerate.progression.why}
      </p>
    {/if}
    {#each lastAnnotations as a, i (i)}
      <p class="why">
        <strong class="mono">{a.part} · {a.label}</strong>
        {a.why}
      </p>
    {/each}
    {#if composer.lastGenerate}
      <p class="small mono">
        {composer.lastGenerate.clips.length} clip(s) · seed {composer.lastGenerate.seed} · Ctrl+Z
        removes the whole sketch
      </p>
      {#each composer.lastGenerate.clips as c (c.clipId)}
        <button class="link mono" onclick={() => midi.open(c.clipId)}>
          open {c.part} in the piano roll ({c.noteCount} notes)
        </button>
      {/each}
    {/if}
    {#if !selected && !composer.lastGenerate}
      <p class="small">
        Pick a chord above to read what it does, or generate something and every part will
        explain itself here.
      </p>
    {/if}
  </section>
</div>

<style>
  .composer {
    display: flex;
    flex-direction: column;
    gap: 12px;
    overflow-y: auto;
    min-height: 0;
    flex: 1;
  }

  section {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  header {
    display: flex;
    align-items: baseline;
    gap: 6px;
    font-size: 9px;
    letter-spacing: 0.16em;
    color: var(--cyan);
    border-bottom: var(--border-width) solid var(--glass-border);
    padding-bottom: 3px;
  }
  .sig {
    color: var(--text-dim);
    letter-spacing: 0.04em;
  }

  .row {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .row.wrap {
    flex-wrap: wrap;
  }
  .spacer {
    flex: 1;
  }

  select,
  input[type="number"] {
    background: rgb(var(--bg-2-rgb) / 0.8);
    color: var(--text);
    border: var(--border-width) solid var(--glass-border);
    border-radius: 2px;
    font-size: 10px;
    font-family: var(--font-ui);
    padding: 3px 4px;
    min-width: 0;
  }
  select:disabled {
    color: var(--text-faint);
  }
  input[type="number"] {
    width: 46px;
    font-family: var(--font-mono);
  }

  .bar {
    display: flex;
    align-items: center;
    gap: 3px;
    font-size: 8px;
    letter-spacing: 0.1em;
    color: var(--text-dim);
  }
  .seed input {
    width: 84px;
  }

  .parts {
    display: flex;
    flex-wrap: wrap;
    gap: 3px;
  }
  .part {
    display: flex;
    align-items: center;
    gap: 3px;
    font-size: 8px;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--text-dim);
    border: var(--border-width) solid var(--glass-border);
    border-radius: 2px;
    padding: 2px 5px;
    cursor: pointer;
  }
  .part.on {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }
  .part input {
    accent-color: var(--cyan);
  }

  .slider {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 8px;
    letter-spacing: 0.1em;
    color: var(--text-dim);
  }
  .slider input {
    flex: 1;
    accent-color: var(--cyan);
  }
  .slider span {
    width: 22px;
    text-align: right;
    color: var(--text-mid);
  }

  .go {
    border: var(--border-width) solid var(--cyan-dim);
    background: rgb(var(--cyan-rgb) / 0.14);
    color: var(--cyan);
    border-radius: 2px;
    padding: 4px 10px;
    font-size: 9px;
    letter-spacing: 0.14em;
    cursor: pointer;
  }
  .go:hover:not(:disabled) {
    background: rgb(var(--cyan-rgb) / 0.26);
  }
  .go:disabled {
    color: var(--text-faint);
    border-color: var(--glass-border);
    background: transparent;
    cursor: default;
  }

  .dice {
    border: var(--border-width) solid var(--glass-border);
    background: transparent;
    color: var(--text-mid);
    border-radius: 2px;
    padding: 2px 6px;
    font-size: 12px;
    cursor: pointer;
  }
  .dice:hover {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }

  .link {
    align-self: flex-start;
    border: none;
    background: transparent;
    color: var(--text-dim);
    font-size: 8px;
    letter-spacing: 0.1em;
    text-decoration: underline;
    cursor: pointer;
    padding: 1px 0;
  }
  .link:hover:not(:disabled) {
    color: var(--cyan);
  }
  .link:disabled {
    color: var(--text-faint);
    cursor: default;
    text-decoration: none;
  }

  .suggest {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px;
  }
  .label {
    font-size: 8px;
    letter-spacing: 0.14em;
    color: var(--text-dim);
  }
  .sug {
    display: flex;
    align-items: baseline;
    gap: 4px;
    border: var(--border-width) solid var(--glass-border);
    border-radius: 2px;
    background: transparent;
    color: var(--text);
    padding: 2px 5px;
    font-size: 11px;
    cursor: pointer;
  }
  .sug:hover {
    border-color: var(--cyan-dim);
    background: rgb(var(--cyan-rgb) / 0.1);
  }
  .sug .roman {
    font-size: 8px;
    color: var(--amber);
  }
  .sug .score {
    font-size: 8px;
    color: var(--text-faint);
  }

  .whys {
    gap: 4px;
  }
  .why {
    margin: 0;
    font-size: 10px;
    line-height: 1.45;
    color: var(--text-mid);
  }
  .why strong {
    color: var(--text);
    margin-right: 4px;
    font-size: 9px;
    letter-spacing: 0.06em;
  }
  .small {
    margin: 0;
    font-size: 8px;
    letter-spacing: 0.06em;
    color: var(--text-dim);
    line-height: 1.5;
  }
  .note {
    margin: 0;
    font-size: 9px;
    color: var(--amber);
    line-height: 1.5;
  }
</style>
