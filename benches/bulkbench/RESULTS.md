# Weighted gesture-mix storage study — results

**Date:** 2026-08-13. **Closes:** `docs/CORE-REDESIGN-ROUND-2.md` §10.2 open
question 2 (where does the replay-only threshold sit; does the memory budget
hold under an agent-heavy mix?).
**Environment:** Intel Core i9-14900, Linux 6.8, rustc 1.94.1, `--release`
(`lto = true`, `codegen-units = 1`). Byte counts are exact and deterministic
(pointer-diffed Arc node graphs, true allocation sizes); µs figures are
single-shot and carry the dossier's ±40 % noise band.
**Raw output:** `run-output.txt` (regenerate with
`cargo run --release -- all`).

Verification anchors, reproduced this run:

| check | expected | measured | |
|---|---|---|---|
| point edit, 16 B, single 10⁶ tree | 3 968 B | 3 968 B | PASS |
| quantize-100k contiguous, 16 B | 1 661 344 B | 1 661 344 B | PASS |
| `pdsbench` `btree` bytes/version | 4 114 B | 4 114 B | PASS |

---

## 1. The 20-byte record (`note_id: u32` — now canonical)

Single 10⁶-event tree, same ops as the previous round:

| op | 16 B retained | 20 B retained | ratio |
|---|---|---|---|
| point edit | 3 968 | 4 480 | 1.13× |
| quantize 100k contiguous | 1 661 344 | 2 061 728 | 1.24× |
| quantize 10k contiguous | 169 904 | 210 352 | 1.24× |
| transpose 1k contiguous | 21 552 | 26 160 | 1.21× |
| transpose 1k scattered over 1M | 2 298 344 | 2 810 344 | 1.22× |
| humanize 10k scattered in 20k window | 335 024 | 415 408 | 1.24× |
| humanize 10k scattered over 1M | 15 303 264 | 18 991 200 | 1.24× |
| delete 50k contiguous | 4 424 | 5 128 | 1.16× |
| paste 20k notes | 335 704 | 416 216 | 1.24× |

Leaf-dominated ops scale by 20/16 = 1.25 as predicted by the dossier's
correction #4; quantize-100k is now **2.06 MB, over the old 2 MB p99 line**
— confirming that the old falsifier threshold is dead and per-op-class caps
(below) replace it.

## 2. Tree granularity: one 10⁶ tree vs 200 pattern trees × 5 000

20 B records; per-pattern numbers include the COW pattern-map path
(root 216 B + 816 B per touched group of 32 — an imbl-HAMT-shaped analytic
constant, ±1 KB).

| logical gesture | single tree | per-pattern | change |
|---|---|---|---|
| point edit | 4 480 | 4 552 | ~same |
| quantize 100k contiguous (20 whole patterns) | 2 061 728 | 2 062 472 | ~same |
| quantize 10k (2 whole patterns) | 210 352 | 207 176 | ~same |
| transpose 1k contiguous (in one pattern) | 26 160 | 25 416 | ~same |
| transpose 1k scattered across session | 2 810 344 | 2 699 944 | ~same |
| humanize 10k scattered, 4-pattern window | 415 408 | 413 320 | ~same |
| **humanize 10k scattered across session** | 18 991 200 | 14 969 848 | −21 % |
| **delete 50k (drop 10 whole patterns)** | 5 128 | **1 512** | map-only |
| paste 20k as NEW content (4 patterns) | 416 216 | 413 320 | ~same |
| **paste 20k as COW duplicate (linked content)** | n/a | **1 032** | free |

What granularity actually buys:

* **A hard per-gesture cap for within-clip ops.** Any op confined to one
  5 000-event pattern retains at most the whole pattern ≈ **104–131 KB**
  (measured max 131 524 B). The dossier's 15.3 MB humanize horror shrinks
  to ≤ 126 KB when the humanize is *within a clip* — which is what the
  gesture almost always is.
* **Whole-pattern deletes and COW duplicates are map-path-only**
  (1.5 KB / 1.0 KB). Round 2 §5's "linked placements share ContentId by
  construction" is measured here: duplication retains 1 032 B.
* **It does NOT fix cross-clip scattered selections.** "Transpose all C3s
  in the song" still drags ~2.7 MB per 1 000 notes; every touched leaf is
  copied no matter which tree it lives in. Granularity bounds the *within*
  case; the *across* case needs replay-only (or placement offsets, §5
  below).

## 3. The gesture-mix profiles (the weights, and why)

10 000 gestures each; a gesture = one history node after coalescing
(dossier §1.5: a knob drag is one node, so weights are per *committed
gesture*, not per input event). Session: 200 patterns × 5 000 events,
20 B records. Sticky targeting: the next gesture stays on the same pattern
with p = 0.8 (HUMAN — people work inside one clip) / 0.65 (AGENT — agents
iterate on a clip, then move on). Seeds fixed; runs are reproducible.

### HUMAN (per-mille)

Grounding: piano-roll editing is dominated by note-level mouse work;
quantize is applied to selections after recording passes; song-wide
operations are rare, deliberate acts. Ratios follow the editing-behavior
shape the dossier's Falsifier 1 assumed but never quantified: bulk ops
must be measured *at* their real frequency, so every class the previous
round measured appears with a defensible weight.

| gesture | ‰ | shape |
|---|---|---|
| PointEdit (move/resize/velocity, 1 note) | 420 | 1 note |
| DrawNote (insert 1) | 220 | 1 note |
| DeleteNote | 100 | 1 note |
| SmallDrag (contiguous selection nudge) | 100 | 8–32 notes |
| PasteBar (paste/duplicate a bar) | 50 | 32–256 notes inserted |
| QuantizeSel (quantize a selection) | 60 | 50–500 contiguous |
| ScatteredSel ("all C3s in clip" transform) | 35 | 20–200 scattered in clip |
| HumanizeClip (whole clip, seeded PRNG) | 8 | 5 000 |
| QuantizeClip (whole clip) | 5 | 5 000 |
| SongWide (transpose every pattern) | 2 | 200 × 5 000 |

74 % of gestures are single-note; 96.5 % touch ≤ 500 notes. If anything
this *over*-weights bulk ops for a human (1.5 % whole-clip-or-larger).

### AGENT (per-mille)

Grounding — what AURA's MCP surface actually drives
(`src-tauri/src/mcp/tools.rs`, names frozen): `run_sidecar_job`
(`amtInfill`, `aceStepGenerate`, …) regenerates or generates clip content
→ **Regen/PastePattern**; `import_audio_clip` + auto-import lands whole
clips → **PastePattern**; and round 2 §4's MCP mutation tier submits
*batched* op streams — an agent never drags one note at a time, it emits
one batch per instruction ("quantize the drums", "humanize the hats",
"transpose these 8 clips", "raise off-beat velocities"). Bulk transforms
are therefore the **common case**, point edits the tail — the inversion
round 2 §6 predicted.

| gesture | ‰ | shape |
|---|---|---|
| Regen (regenerate pattern content, `amtInfill`-shaped) | 180 | 5 000 replaced, blob-backed |
| QuantizeClip | 140 | 5 000 |
| ScatteredCond ("all off-beat notes" in clip) | 120 | 30–50 % of clip, scattered |
| InsertPhrase (batched melody insert) | 120 | 50–300 notes |
| HumanizeClip (seeded) | 80 | 5 000 |
| TransposeClip | 80 | 5 000 |
| MultiClip (same transform, 4–16 clips) | 80 | 20–80 k notes |
| PointEdit (targeted fix) | 80 | 1 note |
| PastePattern (COW duplicate) | 70 | map-only |
| DeleteRangeOp | 50 | 100–1 000 contiguous |

### Variant profiles (the §5 lever)

`HUMAN_V` / `AGENT_V`: identical weights, but transpose-class gestures
(SongWide, TransposeClip, and the transpose-flavoured half of MultiClip)
ride the **placement transpose/velocity offset field that round 2 §5
already puts in the schema** — a map-row edit, no leaf rewrite. Quantize,
humanize, scattered-conditional and regeneration still rewrite leaves
(they change per-note data; an offset field cannot express them).

### Op-log (op + inverse) payload model

Every node retains its op log regardless of snapshot retention (dossier
§1.4). Charged per node, in RAM: point/draw/delete-note 56–64 B; selection
transforms 32 B + 4 B/note ids (+ 4 B/note deltas for quantize, + old
values for velocity edits); whole-clip quantize 4 B/note; humanize **40 B
(seeded PRNG — the §6 constraint)**; transpose O(1); inserts carry their
notes (20 B/note); deletes carry theirs (24 B/note); generated/pasted
content is **blob-backed** (content-addressed store, on disk): 96–160 B of
refs. Plus 96 B fixed HistoryNode overhead.

## 4. Simulation results

Seven retention configs per profile. **A** = round 2 §6 as written:
replay-only iff class is bulk-or-scattered AND the op's own created bytes
exceed the cap. **B** = charge-based: replay-only iff the node's actual
incremental charge exceeds the cap, forced capture every 32 nodes.
`total` = snapshots + op log for 10 000 nodes; `steps@512M` = how many
gestures fit the 512 MiB history budget at that mean; `chain` = worst
replay distance a hover/undo must replay.

### HUMAN

| retention | mean B/node | p99 B/node | snapshots B | op-log B | total B | steps@512M | replay % | chain |
|---|---|---|---|---|---|---|---|---|
| no replay-only | 55 036 | 113 240 | 544 880 976 | 5 486 480 | 550 367 456 | 9 754 | 0 | 0 |
| A class > 64K | 54 683 | 111 248 | 541 352 120 | 5 486 480 | 546 838 600 | 9 817 | 4.7 | 2 |
| A class > 256K | 55 016 | 113 240 | 544 674 932 | 5 486 480 | 550 161 412 | 9 758 | 0.2 | 1 |
| A class > 1M | 55 016 | 113 240 | 544 674 932 | 5 486 480 | 550 161 412 | 9 758 | 0.2 | 1 |
| B charge > 64K | 50 009 | 276 072 | 494 607 248 | 5 486 480 | 500 093 728 | 10 735 | 61.8 | 32 |
| B charge > 256K | 52 240 | 111 512 | 516 918 004 | 5 486 480 | 522 404 484 | 10 276 | 6.4 | 32 |
| B charge > 1M | 52 240 | 111 512 | 516 918 004 | 5 486 480 | 522 404 484 | 10 276 | 6.4 | 32 |

21 SongWide gestures (0.21 %) retain 449 MB — **82 % of the HUMAN total**.
Everything else about HUMAN is benign: point-class p99 is 4.9–5.1 KB,
whole-clip ops ~110 KB at their honest ~1.5 % frequency.

### AGENT

| retention | mean B/node | p99 B/node | snapshots B | op-log B | total B | steps@512M | replay % | chain |
|---|---|---|---|---|---|---|---|---|
| no replay-only | 153 592 | 1 541 260 | 1 479 095 916 | 56 832 316 | 1 535 928 232 | 3 495 | 0 | 0 |
| A class > 64K | 121 921 | 1 939 124 | 1 162 377 728 | 56 832 316 | 1 219 210 044 | 4 403 | 67.9 | 23 |
| A class > 256K | 152 560 | 1 644 504 | 1 468 770 172 | 56 832 316 | 1 525 602 488 | 3 519 | 8.1 | 3 |
| A class > 1M | 153 088 | 1 601 092 | 1 474 056 564 | 56 832 316 | 1 530 888 880 | 3 506 | 4.0 | 3 |
| B charge > 64K | 108 685 | 4 024 840 | 1 030 025 384 | 56 832 316 | 1 086 857 700 | 4 939 | 95.3 | 32 |
| B charge > 256K | 117 518 | 4 239 580 | 1 118 355 924 | 56 832 316 | 1 175 188 240 | 4 568 | 71.3 | 32 |
| B charge > 1M | 123 088 | 4 031 244 | 1 174 057 348 | 56 832 316 | 1 230 889 664 | 4 361 | 56.9 | 32 |

### HUMAN_V (placement-offset transpose)

| retention | mean B/node | p99 B/node | total B | steps@512M | replay % |
|---|---|---|---|---|---|
| no replay-only | 10 127 | 110 688 | 101 275 440 | **53 010** | 0 |
| A class > 64K | 9 813 | 109 788 | 98 138 032 | 54 705 | 4.5 |
| B charge > 64K | 7 748 | 271 048 | 77 481 084 | 69 290 | 60.2 |

### AGENT_V (placement-offset transpose)

| retention | mean B/node | p99 B/node | total B | steps@512M | replay % | chain |
|---|---|---|---|---|---|---|
| no replay-only | 104 707 | 1 369 624 | 1 047 072 948 | 5 127 | 0 | 0 |
| **A class > 64K** | **81 918** | 1 546 636 | **819 183 788** | **6 553** | 56.1 | 15 |
| A class > 256K | 104 322 | 1 456 168 | 1 043 225 944 | 5 146 | 4.2 | 2 |
| B charge > 64K | 71 635 | 2 661 716 | 716 353 044 | 7 494 | 94.1 | 32 |
| B charge > 256K | 84 396 | 2 788 992 | 843 969 356 | 6 361 | 56.6 | 32 |

## 5. What the numbers mean

**Finding 1 — the capture effect: per-node replay-only does not defend the
budget.** A materialized history node retains an `Arc<Session>` — the
*whole* session. So when a bulk op goes replay-only, the very next
materialized node (any point edit) captures the bulk op's surviving output
through structural sharing and gets charged for it. In a mixed stream the
bytes are retained either way; only the *label* moves. Measured: the §6
rule at its own 256 KB cap saves **0.04 % (HUMAN)** and **0.7 % (AGENT)**
of the total. This is physics, not a tunable: as long as one pre-bulk
snapshot and one post-bulk snapshot exist, both generations of the
rewritten leaves are live. Replay-only genuinely frees memory in exactly
one situation: **consecutive bulk rewrites of the same pattern with no
materialized node in between** (agent iteration bursts) — which is where
A > 64K's real 21 % AGENT saving comes from, and why the saving grows with
the stickiness of agent behaviour.

**Finding 2 — the cap that matters is 64 KB, because whole-clip ops sit at
~104–131 KB.** A 256 KB or 1 MB cap exempts the single most common agent
gestures (whole-clip quantize/humanize/regen, ~104 KB each at 5 000
events); the rule then fires on almost nothing (4–8 % of nodes) and the
iteration-burst saving evaporates. At 64 KB the entire whole-clip class is
replay-only and AGENT drops 1 536 → 1 219 MB.

**Finding 3 — rule B is not worth its cost.** Charge-based classification
with forced capture saves another ~10 % (AGENT 1 087 MB) but makes 95 % of
history replay-only, pushes worst-case hover/undo to a 32-op replay, and
concentrates p99 into 4–8 MB capture nodes. Snapshot density is the
feature; B sells it for a marginal saving. (Its one attraction — bounded
per-node charge — is better obtained from eviction.)

**Finding 4 — the placement-offset field is the single biggest lever.**
Routing transpose-class gestures through round 2 §5's placement
transpose/velocity offset (a map-row edit, 1–6 KB) instead of leaf
rewrites: HUMAN total **550 → 101 MB (5.4×)**, because 21 song-wide
transposes were 82 % of the bill; AGENT total 1 536 → 1 047 MB, and
combined with A > 64K, **819 MB**. No other measured intervention comes
within a factor of 3 of this.

**Finding 5 — the budget verdict.** Under the recommended settings
(per-pattern trees, A > 64K, placement-offset transpose) the 512 MiB
ceiling holds:

| profile | mean/step | steps in 512 MiB |
|---|---|---|
| HUMAN | 9.8 KB | **~54 700** |
| AGENT | 81.9 KB | **~6 550** |

A human working day fits outright. An agent-heavy session fits ~6 500
gestures before the GIMP-style ceiling starts coarsening oldest nodes —
the budget *mechanism* holds (eviction is designed for exactly this), but
the dossier's "85 000 steps ≈ a very long working day" arithmetic is
**point-edit arithmetic** and overstates agent capacity by ~13×. Round 2
§6 should say so. Op-log RAM is 5.5–57 MB per 10 k nodes (7 % of budget at
worst, dominated by delete inverses) and is journal-spillable if it ever
matters.

## 6. Recommendations

* **Per-op-class caps** (replaces the dossier's undecidable weighted-mean
  falsifier; all values measured p99 + headroom):
  * **Point class: 8 KB/node.** Measured p99 4.9–5.1 KB across 7 434
    point-class HUMAN gestures including map path and op log; 8 KB gives
    ~60 % headroom and flags any accidental leaf-fanout regression.
  * **Bulk-contiguous: 256 KB/node.** A whole 5 000-event clip rewrite
    measures ≤ 131 524 B; 256 KB covers clips to ~12 k events, and
    anything bigger is a multi-clip op that should be N nodes' worth of
    budget anyway.
  * **Scattered: 256 KB/node** — same bound, because per-pattern trees cap
    within-clip scatter at the clip itself (§2). A scattered selection
    *across* clips has no natural cap and must rely on the replay-only
    rule below.
* **Replay-only threshold: class-based (rule A) at 64 KB own-created
  bytes** — not 256 KB as §6 currently words it, which measured as a
  no-op (0.7 % saving) because whole-clip ops sit below it. 64 KB puts
  the whole-clip class in replay-only, saves 21 % on AGENT via iteration
  bursts, and worst measured replay chain is 23 whole-clip MIDI
  transforms — well under a millisecond of replay against the dossier's
  ~600 µs *per 100 k-note* figure.
* **Route transpose/velocity-class gestures through the §5 placement
  offset fields, never through leaf rewrites.** One sentence of schema
  discipline is worth 5.4× on the human budget and more than every
  replay-only variant combined.
* **Do not adopt charge-based replay-only (rule B)**: −10 % memory for
  95 %-replay history and 4–8 MB capture spikes.
* **State the capture effect in §6** so nobody re-derives replay-only as
  a budget defense: replay-only bounds *node charges* and saves
  *iteration bursts*; the *budget* is defended by eviction/coarsening,
  which remains mandatory.
