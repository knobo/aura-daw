<script lang="ts">
  /**
   * The take report — what the last recorded take did against the reference
   * melody, note by note.
   *
   * Thin renderer (ADR 0006): every number here is the backend's. The report
   * is fetched, formatted and sorted; nothing in this file decides whether a
   * note was hit, what the tolerance means, or what the rating word is. That
   * matters more than usual here, because a second opinion about a score
   * would be indistinguishable from a bug.
   *
   * Two numbers carry the row, not one. `hitFraction` says how in-tune the
   * note was; `coverage` says whether it was sung at all. Collapsing them
   * turns "you skipped this line" into "you sang this line badly", which is
   * different advice.
   */
  import { centsBarGeometry, keyName, sortNotes, type NoteSort } from "../../pitch/report";
  import type { NoteScore, PitchScoreReport } from "../../types/ipc";

  interface Props {
    report: PitchScoreReport | null;
    /** Set while `pitch_score` is in flight. The first score of a take
     * analyses its audio, which is seconds of work, so this is a real state
     * and not a flicker. */
    pending?: boolean;
    /** Why there is no report, when there is none. */
    error?: string | null;
    /** Seek the transport to a note's onset. */
    onseek?: (sample: number) => void;
  }

  const { report, pending = false, error = null, onseek }: Props = $props();

  let sort = $state<NoteSort>("time");
  const rows = $derived(report ? sortNotes(report.notes, sort) : []);

  /** Pixel width of the cents column's bar track. Fixed so every row's
   * centre line is at the same x — a bar that shifted per row would make
   * "further right is sharper" unreadable. */
  const BAR_W = 132;

  function signed(cents: number): string {
    return `${cents > 0 ? "+" : ""}${cents.toFixed(0)}¢`;
  }

  function ms(value: number): string {
    return `${value > 0 ? "+" : ""}${value.toFixed(0)}ms`;
  }

  /** What the row says went wrong, in the singer's terms. Coverage first:
   * an unsung note's tuning numbers are noise over a handful of frames. */
  function verdict(n: NoteScore): string {
    if (n.coverage < 0.25) return "not sung";
    if (n.hitFraction >= 0.9) return "held it";
    if (n.hitFraction >= 0.5) return "wavered";
    return n.meanCents < 0 ? "under it" : "over it";
  }
</script>

<section class="report" aria-label="Take report">
  {#if pending}
    <p class="note mono">ANALYSING THE TAKE…</p>
  {:else if error}
    <p class="note mono err">{error}</p>
  {:else if !report}
    <p class="note mono">Record a take against the reference melody to see how it went.</p>
  {:else}
    <header class="summary">
      {#if report.scoredNotes === 0}
        <!-- Nothing scoreable is not a score of zero: the melody line was
             guessed for every note, so no summary number would mean anything.
             The wording says "ambiguous" and not "chord" because a chord is
             only one of the two causes — a single held note under a
             single-line melody covers every note of it without any two notes
             sharing an onset. The rows below still say what happened. -->
        <span class="headline">
          <strong class="nothing">—</strong>
          <span class="rating">No note to score against</span>
        </span>
        <p class="note mono chordy">
          Every reference note overlaps another, so the melody line was guessed throughout and the
          take is not scored. Pick a track that carries a single line — or mute whatever is held
          underneath it.
        </p>
      {:else}
        <span class="headline">
          <strong>{report.inTolerancePct.toFixed(0)}<span class="pct">%</span></strong>
          <span class="rating">{report.rating}</span>
        </span>
        <dl class="stats mono">
          <div title="Mean absolute error across every scored frame">
            <dt>off by</dt>
            <dd>{report.meanAbsCents.toFixed(0)}¢</dd>
          </div>
          <div title="Median SIGNED error — a consistent sign is the part you can act on">
            <dt>lean</dt>
            <dd>{signed(report.medianSignedCents)}</dd>
          </div>
          <div title="Mean entry offset; positive is late">
            <dt>timing</dt>
            <dd>{ms(report.meanOnsetOffsetMs)}</dd>
          </div>
          <div title="The tolerance this report was computed with">
            <dt>within</dt>
            <dd>±{report.toleranceCents}¢</dd>
          </div>
        </dl>
      {/if}
      <span class="spacer"></span>
      <div class="sorts" role="group" aria-label="Sort notes">
        <button class="chip mono" class:on={sort === "time"} onclick={() => (sort = "time")}>TIME</button>
        <button
          class="chip mono"
          class:on={sort === "worst"}
          title="Worst first, so practice starts where it pays"
          onclick={() => (sort = "worst")}
        >
          WORST
        </button>
      </div>
    </header>

    <div class="rows" role="table">
      <div class="row head mono" role="row">
        <span role="columnheader">note</span>
        <span role="columnheader" class="bar-head">flat · sharp</span>
        <span role="columnheader" class="num">hit</span>
        <span role="columnheader" class="num">sung</span>
        <span role="columnheader" class="num">entry</span>
        <span role="columnheader" class="num" title="Drift over the sustain, with vibrato averaged out">steady</span>
        <span role="columnheader"></span>
      </div>
      {#each rows as n (`${n.noteId}:${n.startSample}`)}
        {@const bar = centsBarGeometry(n.meanCents, report.toleranceCents, BAR_W)}
        <button
          class="row"
          role="row"
          class:unsung={n.coverage < 0.25}
          title={n.ambiguous
            ? "This note came out of a chord — the melody line was guessed (top note), and the row is left out of the totals above."
            : "Seek to this note"}
          onclick={() => onseek?.(n.startSample)}
        >
          <span class="cell name mono" role="cell">
            {keyName(n.key)}{#if n.ambiguous}<span class="amb" aria-label="from a chord">◇</span>{/if}
          </span>
          <span class="cell" role="cell">
            <span class="bar" style:width="{BAR_W}px">
              <span class="tick" style:left="{BAR_W / 2}px"></span>
              <span
                class="fill band-{bar.band}"
                style:left="{bar.x}px"
                style:width="{Math.max(2, bar.w)}px"
              ></span>
            </span>
          </span>
          <span class="cell mono num" role="cell">{(n.hitFraction * 100).toFixed(0)}%</span>
          <span class="cell mono num dim" role="cell">{(n.coverage * 100).toFixed(0)}%</span>
          <span class="cell mono num dim" role="cell">{n.coverage < 0.25 ? "—" : ms(n.onsetOffsetMs)}</span>
          <span class="cell mono num dim" role="cell">
            ±{n.stabilityCents.toFixed(0)}¢{#if n.vibratoRateHz > 0}<span
                class="vib"
                title="Vibrato: {n.vibratoRateHz.toFixed(1)} Hz, {n.vibratoExtentCents.toFixed(0)}¢ peak to peak"
                >∿</span
              >{/if}
          </span>
          <span class="cell verdict" role="cell">{verdict(n)}</span>
        </button>
      {/each}
    </div>
  {/if}
</section>

<style>
  .report {
    display: flex;
    flex-direction: column;
    min-height: 0;
    /* Never at the lane's expense: the lane is what you sing against, the
       report is what you read afterwards. A long melody scrolls its rows
       inside this instead of pushing the canvas out of the panel. */
    flex: 0 1 auto;
    max-height: 45%;
    border-top: 1px solid var(--glass-border);
  }
  .note {
    font-size: 10px;
    letter-spacing: 0.14em;
    color: var(--text-faint);
    padding: 10px 12px;
    margin: 0;
  }
  .note.err {
    color: var(--amber, #ffc857);
    letter-spacing: 0;
  }

  .summary {
    display: flex;
    align-items: center;
    gap: 18px;
    padding: 8px 12px;
  }
  .headline {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .headline strong {
    font-family: var(--font-mono);
    font-size: 26px;
    font-weight: 300;
    color: var(--cyan);
    line-height: 1;
  }
  .pct {
    font-size: 13px;
    color: var(--text-dim);
    margin-left: 1px;
  }
  /* Not cyan: a dash where the score goes must not read as a good result. */
  .headline strong.nothing {
    color: var(--text-dim);
  }
  .chordy {
    max-width: 46ch;
  }
  .rating {
    font-size: 11px;
    color: var(--text);
  }
  .stats {
    display: flex;
    gap: 14px;
    margin: 0;
  }
  .stats div {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .stats dt {
    font-size: 8px;
    letter-spacing: 0.18em;
    text-transform: uppercase;
    color: var(--text-faint);
  }
  .stats dd {
    font-size: 11px;
    color: var(--text);
    margin: 0;
  }
  .spacer {
    flex: 1;
  }
  .sorts {
    display: flex;
    gap: 2px;
  }
  .chip {
    font-size: 9px;
    letter-spacing: 0.18em;
    padding: 3px 9px;
    border-radius: 999px;
    border: 1px solid transparent;
    background: transparent;
    color: var(--text-dim);
    cursor: pointer;
  }
  .chip:hover {
    color: var(--text);
  }
  .chip.on {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }

  .rows {
    overflow-y: auto;
    min-height: 0;
    padding-bottom: 4px;
  }
  .row {
    display: grid;
    grid-template-columns: 62px 140px 46px 46px 60px 66px 1fr;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 2px 12px;
    border: 0;
    background: transparent;
    text-align: left;
    font: inherit;
    color: var(--text);
    cursor: pointer;
  }
  .row.head {
    font-size: 8px;
    letter-spacing: 0.18em;
    text-transform: uppercase;
    color: var(--text-faint);
    cursor: default;
    position: sticky;
    top: 0;
    background: var(--bg-1, rgba(5, 7, 13, 0.92));
    padding-top: 4px;
    padding-bottom: 4px;
  }
  .bar-head {
    text-align: center;
  }
  button.row:hover {
    background: rgba(96, 130, 190, 0.09);
  }
  .row.unsung {
    opacity: 0.55;
  }
  .cell {
    font-size: 11px;
  }
  .num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .dim {
    color: var(--text-dim);
  }
  .name {
    color: var(--text);
  }
  .amb {
    color: var(--text-faint);
    margin-left: 3px;
    font-size: 9px;
  }
  .vib {
    color: var(--text-faint);
    margin-left: 3px;
  }
  .verdict {
    font-size: 10px;
    color: var(--text-faint);
  }

  .bar {
    position: relative;
    display: block;
    height: 9px;
    border-radius: 2px;
    background: rgba(96, 130, 190, 0.09);
  }
  .tick {
    position: absolute;
    top: -1px;
    bottom: -1px;
    width: 1px;
    background: rgba(216, 227, 242, 0.35);
  }
  .fill {
    position: absolute;
    top: 1px;
    bottom: 1px;
    border-radius: 1px;
  }
  /* Same vocabulary as the live lane: cyan in, amber near, magenta far.
     Magenta and not red — red means recording here, and being 40 cents
     flat is ordinary singing, not an error. */
  .fill.band-in {
    background: #52e5ff;
  }
  .fill.band-near {
    background: #ffc857;
  }
  .fill.band-far {
    background: #ff4fd8;
  }
</style>
