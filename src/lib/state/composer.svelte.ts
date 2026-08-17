/**
 * The Composer's frontend store (Plan H1 Task 7).
 *
 * PUSHED STATE ONLY. Every field here came from the backend, and nothing in
 * this file derives a chord, a scale degree, a Roman numeral or a note name —
 * that is the whole of ADR 0006 applied to a feature whose subject matter is
 * unusually tempting to reimplement client-side. Edits go out as one
 * `harmony_set` per gesture (one op, one undo).
 */

import { backend } from "../tauri";
import { midi } from "./midi.svelte";
import { project } from "./project.svelte";
import { toasts } from "./toasts.svelte";
import type {
  ChordSpan,
  ComposerGenerateReply,
  ComposerGenerateRequest,
  ComposerSuggestion,
  HarmonyView,
  KeySpan,
  PaletteView,
} from "../types/ipc";

/** The generator parts a request can ask for, in the order the panel lists them. */
export const PARTS = ["chords", "bass", "melody", "drums"] as const;
export type Part = (typeof PARTS)[number];

class ComposerStore {
  /** Everything the panel draws — the document, the circle, the analysis. */
  view = $state<HarmonyView | null>(null);
  /** Which notes are available at the cursor; drives the piano-roll tint. */
  palette = $state<PaletteView | null>(null);
  suggestions = $state<ComposerSuggestion[]>([]);
  /** The last generate's reply: the clips it wrote and their whys. */
  lastGenerate = $state<ComposerGenerateReply | null>(null);
  /** Chord region the user clicked, by tick — the WHY panel reads its analysis. */
  selectedTick = $state<number | null>(null);
  /** Where the panel is pointing: the bar a click or a generate applies to. */
  atTicks = $state(0);
  /** The seed the next generate uses. Shown, editable, and re-rollable, so a
   * take the user liked can be got back (ruling H-4). */
  seed = $state(1);
  busy = $state(false);
  /** Set when the backend has no composer commands (an older build). */
  unavailable = $state(false);
  /** Palette per chord region, keyed by the region's tick — what the piano
   * roll tints its rows with. One backend call per region, cached until the
   * document changes, because the answer is different in every bar and a
   * per-repaint round trip would be absurd. */
  regionPalettes = $state<Record<number, PaletteView>>({});

  get chords(): ChordSpan[] {
    return this.view?.harmony.chords ?? [];
  }

  get keyLabel(): string {
    return this.view?.keyLabel ?? "—";
  }

  get selectedSpan() {
    if (this.selectedTick === null) return null;
    return this.view?.spans.find((s) => s.tick === this.selectedTick) ?? null;
  }

  /** One bar in ticks, backend-shipped (never derived from a time signature
   * here — that is `midi.ticksPerBar`'s job and this store does not duplicate
   * it, it just prefers the value the composer view came with). */
  get ticksPerBar(): number {
    return this.view?.ticksPerBar ?? midi.ticksPerBar;
  }

  private subscribed = false;

  async init() {
    this.subscribe();
    await this.refresh();
  }

  /** One subscription per process: an undo re-pulls the project, and the
   * harmony document is part of what an undo can address. */
  private subscribe() {
    if (this.subscribed) return;
    this.subscribed = true;
    backend.on("project://changed", () => void this.refresh());
  }

  async refresh(atTicks?: number) {
    if (atTicks !== undefined) this.atTicks = atTicks;
    if (!backend.harmonyGet) {
      this.unavailable = true;
      return;
    }
    try {
      const next = await backend.harmonyGet(this.atTicks);
      // The cache is keyed by region tick, and a tick can hold a different
      // chord after an edit — so it is dropped whenever the document moves.
      if (JSON.stringify(next.harmony) !== JSON.stringify(this.view?.harmony)) {
        this.regionPalettes = {};
      }
      this.view = next;
      this.unavailable = false;
    } catch (err) {
      console.warn("[aura] harmony_get failed:", err);
    }
  }

  /** Send the whole document — one invoke, one op, one undo (D-03). */
  private async write(keys: KeySpan[], chords: ChordSpan[], label: string) {
    if (!backend.harmonySet) return;
    this.busy = true;
    try {
      this.view = await backend.harmonySet(keys, chords, this.atTicks);
      this.regionPalettes = {};
    } catch (err) {
      toasts.error(label, String(err));
    } finally {
      this.busy = false;
    }
  }

  /** Change the key of the whole song. The chords are left exactly as they
   * are: re-reading them in a new key is the point (a `vi` becomes a `ii`),
   * not a reason to rewrite them. */
  async setKey(key: string) {
    const chords = this.chords.map((c) => ({ ...c }));
    await this.write([{ tick: 0, key }], chords, "COULD NOT SET THE KEY");
  }

  /** Append a chord one bar long after the last one — what a circle wedge
   * click does. */
  async appendChord(chord: string) {
    const keys = this.view?.harmony.keys.map((k) => ({ ...k })) ?? [];
    const chords = this.chords.map((c) => ({ ...c }));
    const last = chords[chords.length - 1];
    const tick = last ? last.tick + last.lengthTicks : this.atTicks;
    chords.push({ tick, lengthTicks: this.ticksPerBar, chord });
    await this.write(keys.length ? keys : [{ tick: 0, key: "C ionian" }], chords, "COULD NOT ADD THE CHORD");
  }

  /** Replace the chord in the region at `tick`. */
  async replaceChordAt(tick: number, chord: string) {
    const keys = this.view?.harmony.keys.map((k) => ({ ...k })) ?? [];
    const chords = this.chords.map((c) => (c.tick === tick ? { ...c, chord } : { ...c }));
    await this.write(keys, chords, "COULD NOT CHANGE THE CHORD");
  }

  /** Remove the region at `tick` and close the gap, so the progression stays
   * contiguous rather than developing a hole. */
  async removeChordAt(tick: number) {
    const keys = this.view?.harmony.keys.map((k) => ({ ...k })) ?? [];
    const removed = this.chords.find((c) => c.tick === tick);
    if (!removed) return;
    const chords = this.chords
      .filter((c) => c.tick !== tick)
      .map((c) => (c.tick > tick ? { ...c, tick: c.tick - removed.lengthTicks } : { ...c }));
    if (this.selectedTick === tick) this.selectedTick = null;
    await this.write(keys, chords, "COULD NOT REMOVE THE CHORD");
  }

  async clear() {
    const keys = this.view?.harmony.keys.map((k) => ({ ...k })) ?? [];
    this.selectedTick = null;
    await this.write(keys, [], "COULD NOT CLEAR THE PROGRESSION");
  }

  /** Generate parts as ordinary MIDI clips. One transaction, one undo. */
  async generate(request: ComposerGenerateRequest): Promise<ComposerGenerateReply | null> {
    if (!backend.composerGenerate || this.busy) return null;
    this.busy = true;
    try {
      const reply = await backend.composerGenerate({
        atTicks: this.atTicks,
        seed: this.seed,
        ...request,
      });
      this.lastGenerate = reply;
      this.view = reply.harmony;
      this.regionPalettes = {};
      // New tracks and new clips: pull both, then point the user at the first
      // thing that landed (the same courtesy a hum result gets).
      await project.reload();
      await midi.refresh();
      const first = reply.clips[0];
      if (first) {
        midi.select(first.clipId);
        midi.flash(first.clipId);
      }
      const parts = reply.clips.map((c) => c.part).join(", ");
      toasts.info("COMPOSER", `${reply.bars} bars · ${parts || "nothing"} · seed ${reply.seed}`);
      return reply;
    } catch (err) {
      toasts.error("GENERATE FAILED", String(err));
      return null;
    } finally {
      this.busy = false;
    }
  }

  /**
   * Fetch (and cache) the palette of every chord region overlapping
   * `[fromTick, toTick)`. Called when a clip opens and when the harmony
   * changes; a region already cached is not re-fetched.
   */
  async loadRegionPalettes(fromTick: number, toTick: number) {
    if (!backend.composerPalette) return;
    const spans = this.chords.filter(
      (c) => c.tick < toTick && c.tick + c.lengthTicks > fromTick,
    );
    // Bounded: a clip spanning hundreds of regions would otherwise fire
    // hundreds of invokes at once.
    for (const span of spans.slice(0, 64)) {
      if (this.regionPalettes[span.tick]) continue;
      try {
        const pal = await backend.composerPalette(span.tick);
        this.regionPalettes = { ...this.regionPalettes, [span.tick]: pal };
      } catch (err) {
        console.warn("[aura] composer_palette failed:", err);
        return;
      }
    }
  }

  /** The palette in force at `tick`, if its region has been fetched. */
  paletteAt(tick: number): PaletteView | null {
    const span = this.chords.find((c) => tick >= c.tick && tick < c.tick + c.lengthTicks);
    return span ? (this.regionPalettes[span.tick] ?? null) : null;
  }

  async loadPalette(tick: number) {
    if (!backend.composerPalette) return;
    try {
      this.palette = await backend.composerPalette(tick);
    } catch (err) {
      console.warn("[aura] composer_palette failed:", err);
    }
  }

  async loadSuggestions() {
    if (!backend.composerSuggest) return;
    try {
      // Suggestions are "what comes NEXT", so they are asked for at the end of
      // the progression, not at the panel's cursor.
      const chords = this.chords;
      const last = chords[chords.length - 1];
      const at = last ? last.tick + last.lengthTicks : this.atTicks;
      this.suggestions = await backend.composerSuggest(at, 6);
    } catch (err) {
      console.warn("[aura] composer_suggest failed:", err);
    }
  }

  select(tick: number | null) {
    this.selectedTick = tick;
    if (tick !== null) void this.loadPalette(tick);
  }

  /** A fresh seed and nothing else — the dice button. Deterministic source
   * (the current seed) so the sequence of takes is itself reproducible. */
  rollSeed() {
    // xorshift on the current seed: no clock, no Math.random, so a session
    // that starts from seed 1 always offers the same succession of ideas.
    let x = this.seed >>> 0 || 1;
    x ^= x << 13;
    x >>>= 0;
    x ^= x >>> 17;
    x ^= x << 5;
    x >>>= 0;
    this.seed = x || 1;
  }
}

export const composer = new ComposerStore();
