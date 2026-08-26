# AURA — Research dossier

Status: reference material, owned by the architecture agent.
Produced: 2026-08-13, against commit `f632580`.
Audience: anyone about to change the core — human or agent.

## Why this directory exists

In August 2026 the project ran a single large research round before
re-architecting the core. Fourteen agents read competitor source, plugin
specs, DAW manuals, academic papers and this repository, and one of them
ran first-hand benchmarks. The findings are worth more than the code they
will produce, and they existed only inside one conversation.

These documents are that conversation, written down. They are **not**
summaries — citations, verbatim quotes, code excerpts and the distinction
between "verified from a source" and "engineering judgment" are preserved
throughout, because the value is in being able to check the reasoning
rather than take it on trust.

On 2026-08-13 the first redesign draft built on these dossiers was put
through an adversarial review (six independent attack agents plus a
naive-questions pass). The result:
[`CORE-REDESIGN-ROUND-2.md`](../CORE-REDESIGN-ROUND-2.md) supersedes
round 1, dossiers [06](06-time-travel-storage.md) and
[10](10-aura-current-state.md) carry **marked corrections** (never silent
edits), and the surviving decisions are distilled as ADRs — Status:
Accepted 2026-08-13 — in [`docs/adr/`](../adr/README.md).

**This directory is history, not law.** What binds code lives in
`ARCHITECTURE.md` (as built), `SCALABILITY.md` (the debt register), and
`docs/adr/` (the decisions and their consequences). Read a dossier when you
want to know *why* one of those says what it says, or when you are about to
re-litigate a decision and want to know what was already considered.

## The documents

| # | Document | What it answers |
|---|---|---|
| 01 | [Zrythm](01-zrythm.md) | The DAW we set out to study. What its feature list actually costs, the four data-model decisions that produce nearly all of it, and the two mistakes — bits where types belonged, view state inside model types — that turned a toolkit port into a four-year rewrite. |
| 02 | [DAW engine architecture](02-daw-engine-architecture.md) | How serious engines build the processing graph, schedule it across cores, move data to and from the RT thread, model parameters and modulation, and represent time. Ardour, Tracktion, JUCE, Zrythm, Firewheel, CLAP. |
| 03 | [FL Studio, Bitwig, Ableton](03-fl-bitwig-ableton.md) | The architectural bets behind the signature features of the three products we compete with — and the leverage ranking: what is cheap now and ruinous to retrofit. |
| 04 | [Command and undo architecture](04-command-and-undo-architecture.md) | The mutation channel. Command-pattern variants, how Ardour, Zrythm, Blender and Figma each solved it, why one of them cannot undo track deletion, and the addressable-object problem that decides whether undo is possible at all. |
| 05 | [History, takes and variants](05-history-takes-and-variants.md) | The differentiating feature: browsable history, extracting a past state as a take, and A/B comparison. Prior art, the design, and the three invariants. |
| 06 | [Time-travel storage](06-time-travel-storage.md) | **Measured.** What data structures make any past revision cheap to materialize, with benchmarks, a recommended design, and the three results that would falsify it. |
| 07 | [Real-time undo hazards](07-realtime-undo-hazards.md) | What breaks when state changes under a running audio thread, who owns deallocation, and the forty rules the design must obey. |
| 08 | [Rust core engineering](08-rust-core-engineering.md) | Crate layout, SOLID as it actually expresses itself in Rust, and — the important half — how to make architectural boundaries survive years of edits by agents who never read the docs. |
| 09 | [Plugin isolation and UI scaling](09-plugin-isolation-and-ui-scaling.md) | Out-of-process plugin hosting, the plugin-GUI problem honestly stated, and the risk profile of a web-stack DAW UI on Linux. |
| 10 | [AURA current state](10-aura-current-state.md) | Where we actually are: the engine, control plane, project model, frontend and debt register as they stand, with file references — and a consolidated list of the concrete defects this round found. |
| 11 | [History UX and contracts](11-history-ux-and-contracts.md) | The interaction design for the history feature, and the 26 numbered contracts it imposes on the backend. The bridge between what the user experiences and what the engine must provide. |
| 12 | [Control surfaces](12-control-surfaces.md) | Virtual mixers and pad decks. MCU/HUI, Push/Launchpad, CSI, OSC, the Akai LPD8, and why this is host chrome rather than a plugin. Written 2026-08-26 for the control-surface track. |
| 13 | [Players and performance](13-players-and-performance.md) | What a pad has to *be*: an audit of AURA's own engine against a polyphonic launcher — one shadow playhead, a transport hijack, a MIDI-only clip target, automation on the wrong clock — plus the two design traps (hidden tracks, MIDI-channel multiplexing). Written 2026-08-26 for Plan V. Cites this repository, not external source. |

## Reading orders

**Changing the audio engine:** 10 → 02 → 07.

**Changing how the project is mutated, saved or undone:** 10 → 04 → 06 → 11.

**Building the history/takes feature:** 05 → 11 → 06 → 07.

**Deciding where a new crate, module or boundary goes:** 08 → 10.

**Wondering whether a feature is worth it:** 03, then 01 for the
cautionary version of the same question.

## Conventions

Each document opens with its own provenance and marks claims as verified
or as judgment. Several also carry a list of things the research could not
confirm — those are labelled, and they are not settled. Where a document
recommends changing something this repository has already decided, it says
so explicitly in a closing "What this means for AURA" section; those are
the passages that turn into ADRs.
