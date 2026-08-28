# Landed — do not restart

Everything in this table is merged to `main`. If a job you are about to
start appears here, you are about to redo it. Open the pointer instead.

Newest first.

## The engine performance gate

`scripts/perf-check.sh` — the measurement from §9 turned into something
that answers yes or no. Exit **0** under budget, **1** over, **125**
unjudgeable (did not build, harness absent at that commit, plugins
missing), which is the protocol `git bisect run` needs. Bisect SKIPs on
125 instead of blaming a commit that merely failed to compile.

Defaults to the `bare` column, which needs **no plugins installed** and
runs anywhere; `--run full` covers the plugin path. `--harness-from
<ref>` injects the harness at commits that predate it, so a bisect can
cross the commit that introduced it — verified end to end across
b7fb583, which has no harness of its own.

The budget is passed in rather than committed: an absolute µs baseline in
a file is a property of one CPU. Best-of-N, not median — benchmark noise
is one-sided. A reference-workload ratio was built and then removed: the
thermal drift it was meant to cancel did not survive a cold-vs-hot
measurement, and the ratio was noisier than the absolute it replaced.

Rules in [`STANDING-CONSTRAINTS.md`](STANDING-CONSTRAINTS.md)
§Performance and [`../CLAUDE.md`](../CLAUDE.md); caveats in
[`GAP_ANALYSIS.md`](GAP_ANALYSIS.md) §9.6.

| PR |
|---|
| PR #120 |

## Where a block's time actually goes, under real plugin load

`GAP_ANALYSIS.md` §8.4 asked for one measurement before any further
engine performance work. It exists now:
[`GAP_ANALYSIS.md`](GAP_ANALYSIS.md) §9, reproduced by
`src-tauri/tests/plugin_load_profile.rs`.

The premise held in direction and not in emphasis. At 32 tracks, 8 hosted
instruments and 66 insert slots: plugin DSP **28%** of the block, AURA's
own code **52%** — of which the per-insert host path is **25%** on its
own, **~3 µs per insert slot**. The whole session uses **8.6% of the
10.67 ms deadline**, so none of it is audible today (§9.3); the point is
that the one large addressable line is ours, not the plugins'.

Method: no instrumentation on the RT path. The same session renders four
times with different plugins attached and the costs come out by
subtraction — including a `cheap_fx` pass (same chain lengths, one
multiply per sample) that separates host overhead from DSP without a
profiler, which this machine cannot run (`kernel.perf_event_paranoid=4`).
Figures are the median of three runs: §9.2 shows why the plugin-DSP
column must never be quoted from one.

| PR |
|---|
| PR #119 |

## Plan V — players (a pad that is an instrument)

One cut landed, seven staged behind it: [`backlog/plan-v-players.md`](backlog/plan-v-players.md).

| What | Where |
|---|---|
| **V1 — `MixNode` as the graph compiler's input.** `compile_inserts` and `compile_routing` stop reading `TrackState` and take `&[MixNode]` instead; tracks and buses become two producers of one node type (`audio/node.rs`, `From<&TrackState>` + `mix_nodes()`). Behaviour-neutral by construction: two characterization gates were written and hashed BEFORE the refactor and are still green, unedited, after it — `audio::offline::tests::bounce_of_a_full_strip_is_byte_stable` (a rendered bounce exercising clip + bus + send + output + gain/pan, FNV-1a-64 over the samples) and `audio::bus::tests::routing_plan_of_a_full_strip_is_stable` (the exact routing plan — `track_pdc`, `out_delay`, `output`, `bus_ids`, send delays — with a bypassed insert live so declared PDC ≠ applied PDC). `bus::would_cycle` deliberately keeps `&[TrackState]`: it is a control-plane guard answering "would this edge close a loop" about document rows before the edge is written, not a graph-compiler input; see its doc comment. No new IPC, no document change, no schema bump. | PR #118. Backlog: [`backlog/plan-v-players.md`](backlog/plan-v-players.md) |

## Plugin manager and native floating GUI

Two PRs, one track. Details, scope calls and what is left:
[`backlog/plugin-manager.md`](backlog/plugin-manager.md).

| What | Where |
|---|---|
| **LV2 port properties — stepped, expensive, non-automatable** | PR #114. ZamVerb declares "Room" as `lv2:integer` 0..6 **plus** `pprops:expensive` and `kx:NonAutomatable` (the value picks the convolution impulse response); AURA reported it as a continuous 0..6 param, drew a smooth knob, and a drag streamed ~60 fractional values a second at a port that reloads an IR per write — the crackling the owner heard. `livi` 0.7 exposes no port properties, so `lv2_host::control_port_params` now reads them from raw lilv: `lv2:toggled`/`lv2:enumeration`/`lv2:integer` → `ParamInfo.steps`, `pprops:expensive`/`kx:NonAutomatable` → the new additive `ParamInfo.non_automatable`. `modulation.pickTarget` refuses to mint a lane on a flagged param (an existing one still opens); the panel's `A` button goes dashed with the reason. CLAP reports `false` — see the backlog for why `!IS_AUTOMATABLE` was NOT wired. **Owner ear-check owed.** | PR #114. Detail: [`backlog/plugin-manager.md`](backlog/plugin-manager.md) |
| **Step 7 — unified browser audition** | PR #106. Double-click any browser row to hear it (Shift+Enter for keyboard parity), behind a new `browserAudition` pref defaulting off. `utils/audition-target.ts` resolves a row to an `AuditionTarget`; `state/audition.svelte.ts` plays it. No new IPC, no ops. Detail and rulings R-1…R-4: [`backlog/plugin-manager.md`](backlog/plugin-manager.md). |
| **Step 6 — automation as an inventory** | PR #98, amended by PR #105. Param cache + shared lane reveal (`plugins.svelte.ts` `paramCache` over the frozen `plugin_get_params`; `utils/lane-reveal.ts`'s `revealParamLane`), `AutomationMatrix.svelte` + `utils/automation-matrix.ts` as a 4th `ManagerMode`, pinned params in `PluginParamPanel.svelte`, and `LanePluginStrip.svelte` + `utils/lane-strip.ts` in `TrackHeader.svelte`'s `.metadata-row`. #98 was squashed four minutes before its own fix wave was pushed, so its final review's merge blocker landed separately in #105: `revealParamLane` now refuses an empty `trackId` instead of minting a curve and a binding no lane can show. |
| **Step 5 — plugin manager + native floating GUI** — catalog, browse/split/rack, Ctrl+P frecency, CLAP/LV2/Zyn editors, live on-top pref | PR #93 `e1ec61f`. Winner spec: [`2026-08-20-plugin-admin-winner-design.md`](superpowers/specs/2026-08-20-plugin-admin-winner-design.md). Plan: [`2026-08-20-plugin-manager.md`](superpowers/plans/2026-08-20-plugin-manager.md). |
| Timeline follow-on-seek | `view.revealSamples` (in PR #93) |
| Shared `browser/` layer | instruments / samples / presets migrated (in PR #93) |
| Lane multi-select + bulk M/S/A | `lane-bulk.ts`, ARIA grid (in PR #93) |

## Mixer graph — insert FX, sends, returns (Plan G)

Product cut, what shipped and what did not:
[`backlog/insert-fx-sends-sidechain.md`](backlog/insert-fx-sends-sidechain.md).

| What | Where |
|---|---|
| **G2 — bus tracks, sends AND output routing.** Two primitives, kept apart on purpose: a send is a COPY (shared reverb, everyone still heard dry), an output is a MOVE (a submix — the track stops reaching the master). Buses route and send too, so a drum bus into a mix bus works; the compiler is a real DAG with a topological order and cycle rejection (`bus::would_cycle`), and PDC is per EDGE so a dry path can wait for a slow return while its own send into that return leaves immediately. `kind: "bus"` returns, `TrackState.sends` + `TrackState.output` edges, `audio::bus::compile_routing` shared by the engine and the bounce, two compensating delays (source alignment before the taps, dry-path alignment after them), a windowed render so the bus pass can run after every tap, the balance pan law on returns, send amount as a `ParamTable` lane (no rebuild per knob frame), a SENDS rack on the track header and `+ BUS`. Also closes G1 Task 8's offline half: the bounce now walks insert chains. **Owner ear-check owed** — one convolution reverb, several sources, one room, then export. | PR #109 |
| G1 Task 7 — inserts wired into `engine::rebuild` (effects audible on audio and MIDI tracks) | PR #90 |

## Everything else

| What | Pointer |
|---|---|
| **Control surface v0.2.1 — racks.** A device stops being a page *mode* and becomes an object on the page: `+ → RACK` appends a faceplate as a widget group (`rack:<id>`), so two LPD8s sit side by side, either removes on its own, and `+` never destroys what is already on the deck — the reported defect. `DEVICE_RACKS` is data (knobs and their column span, faders, what a knob drives, pad block, mode silkscreen), so Launchpad 8×8, MCU 8-strip and nanoKONTROL render from `Rack.svelte` with no per-device code; this is the shape Plan V's V8 hardware map binds to. The rows sit one level down behind `Add rack ›` (Escape peels the level before it closes the menu) — a drill-down rather than a flyout, because the popover is already height-capped against the bottom edge of the window. `Clear page` becomes an explicit action, `template:blank`/`template:mixer` are gone, and layout v2 migrates a saved v1 `lpd8` deck into a rack group. Two neighbours fell out of driving the real panel: `addStrip` no longer refuses a strip just because a rack knob drives that gain, and a `Fader` label can no longer overflow onto its neighbour. | PR #116. Backlog: [`backlog/control-surface.md`](backlog/control-surface.md) |
| **Control surface v0.1** — a performable bottom panel (SURFACE, beside ROLL / PITCH): channel strips, analog VU gauges, faders/knobs, mute-solo-arm lamps, a named clip list, N×M pad grids whose LEDs breathe with the meter bus, and homage templates (AKAI LPD8, Mixer). One-click recipes fill it from the open project; a bind picker points any inserted widget at a track gain/pan/mute, a MIDI clip or a plugin param, and a pad-grid cell at a clip. Host chrome, not a plugin: the layout lives in localStorage per project dir and every widget emits an EXISTING mix/launch/plugin command, on the same gesture tail as `TrackHeader` so one drag is one undo entry. | PR #113. Backlog: [`backlog/control-surface.md`](backlog/control-surface.md); design: [`2026-08-26-control-surface-design.md`](superpowers/specs/2026-08-26-control-surface-design.md); research: [`research/12-control-surfaces.md`](research/12-control-surfaces.md) |
| **RT engine primitives + a Cranelift-fused fader kernel** (`aura-engine/`, a STANDALONE crate — nothing in `src-tauri/` changed, the frozen manifest stays frozen). `sync::TripleBuffer<T>` (wait-free latest-value publish, allocates only in `new`, recycles instead of freeing — which is what lets `JITModule::free_memory`'s unsafe precondition be discharged); `metrics::AllocCounter`, a counting global allocator that turns dossier 10's gap 19 into a test; `strip::plan`, which cuts a block into straight-line stretches; and `jit`, a Cranelift kernel fusing gain·ramp·pan·mute plus the meter fold, two frames per iteration. Bit-identical to the scalar plan, and to `mixer::apply_fader_into` in the flat case — within 1e-5 once a ramp moves, so **no bounce byte-comparison can validate it**. New CI job `engine`. **The audit is the other half of the deliverable**: three of the brief's six items already existed, and the Carla bridge is declined. **NOT wired into the app, and §8 of the audit recommends the JIT never is** — cranelift 0.134 has no `MAP_JIT` path, so macOS notarization would need the broad `allow-unsigned-executable-memory` entitlement, and the JIT is worth ~0.2% of one core over the plain-Rust plan. Treat the crate as a proving ground, not a shipped component. The track is not ear-checkable: no code path reaches it. | PR #112. Audit: [`GAP_ANALYSIS.md`](GAP_ANALYSIS.md). Track: [`backlog/jit-engine.md`](backlog/jit-engine.md). Brief: `jit.md` |
| **The param panel follows automation** (Track D ruling 2's recorded consequence). Plugin-param automation is driven host-only, so the panel painted the document value while the plugin moved. The engine now publishes its driver's own writes on the meter frame (`MeterFrame.drivenParams`, an upsert set — `tick` suppresses unchanged values, so deltas alone would blank a held param 60×/s) and the row paints them: chip, toggle, enum and fader, a magenta fill, and an AUTO flag. Cleared on transport stop and on rebuild. Display only; ruling 2 is unchanged. | PR #108. Backlog: [`backlog/history-and-automation.md`](backlog/history-and-automation.md); handoff: [`PHASE4-PLAN.md`](PHASE4-PLAN.md) "Track D handoff" |
| **`Undo to here`** — guarded linear walk back to a retained revision: `ControlPlane::undo_to` validates the target against the live undo path and the caller's observed `(epoch, head rev)`, then repeats the ordinary undo step under one `history_gate` hold; additive `history_undo_to` command, `onUndoPath`/`epoch`/`headRev` on the overview, and the action in the HISTORY dock | PR #107. Contract: `PHASE4-PLAN.md` "Plan F handoff" carry-forward (e). Plan: [`2026-08-23-undo-to-here.md`](superpowers/plans/2026-08-23-undo-to-here.md) |
| **Extract melody to MIDI** — segmenting audio clip pitch frames to editable MIDI clip, auto-creating/targeting MIDI tracks with undo, and selecting as Pitch Coach reference track | PR #91. Product doc: [`pitch-track.md`](pitch-track.md) |
| **Pitch analysis action on clip view** — analyse clip button on selected audio clips, persisted APTF cache rebuild, teardown/generation guards, and PitchCoach report cache invalidation | PR #87 `25af6ae`. |
| **Write / Touch / Latch automation modes** — Off / Read / Write / Touch / Latch per track, real-time control-thread point recorder, single-op commit on stop/release with undo | PR #85 `d496903`. Design spec: [`2026-08-18-automation-write-touch-latch-design.md`](superpowers/specs/2026-08-18-automation-write-touch-latch-design.md). Plan: [`2026-08-18-automation-write-touch-latch.md`](superpowers/plans/2026-08-18-automation-write-touch-latch.md) |
| **MIDI output — per-track and per-clip patchbay routing** | PR #84 `cbbc240`. Handoff: [`midi-output.md`](midi-output.md) |
| **Undo / Version graph read-only browser** | PR #82 `b741251`. |
| **DOM test environment (jsdom)** — mounted component tests for `AutomationTrackRow` | PR #80 `ca67b7d`. |
| **CI: apt no longer hangs the Rust job, and the Rust cache now exists.** The job was ~27 min of which the 1231 tests were 20 s; `apt-get update` had stalled 23.5 min and been cancelled. apt is now 3 min (`--no-install-recommends` + bounded retries), and `push: branches: [main]` finally lets `Swatinem/rust-cache` save a cache a PR can restore — without it every PR compiled ~600 crates from scratch. | PR #78 `2b00e91` |
| **MIDI output — eight fixes** (re-cue storm, per-note channel, routed track no longer doubles its internal synth, undo-after-delete silence, forced-channel persistence, piano-roll note channel, ALSA re-address, live port list). An owner ear-check on a real drum machine is still owed. | PR #77 `55257e8`. Handoff: [`midi-out-note-channel.md`](handoff/midi-out-note-channel.md) |
| G1 Task 6 — PDC (`DelayLine` + `compile_pdc`) | PR #71 `9e0b884`. Handoff: [`g1-insert-fx.md`](handoff/g1-insert-fx.md). Known gap for Task 7: [`backlog/insert-fx-pdc.md`](backlog/insert-fx-pdc.md) |
| Clip Delete/Backspace after a plain pointer-click | PR #70 `7011e81`. Pointer-select never focused the clip element, so its own keydown handler never fired; a window-level fallback now reads `clipSelection` directly. Single-clip only — batch-deleting a multi-selection is still open. |
| G1 Task 5 — mixer strip: source-sum → inserts REPLACE → shared fader (`InsertNode`/`compile_inserts`) | PR #66 `c99293b`. Handoff: [`g1-insert-fx.md`](handoff/g1-insert-fx.md) |
| **The Composer, Plan H1** — a pure music-theory library, the harmony document in the core, five generators (progression, voice-led chords, bass, melody, groove), the COMPOSER panel, and a piano roll that tints its keys by what each note does over the chord. Owner ear-checked 2026-08-18: works. **Composer is deprioritized — do not start H2+ unless asked.** | PR [#65](https://github.com/knobo/aura-daw/pull/65) `63cb7fa`. Product doc: [`composer-assistant.md`](backlog/composer-assistant.md). Plan + rulings H-1…H-12: [`2026-08-17-plan-h-composer.md`](superpowers/plans/2026-08-17-plan-h-composer.md). Handoff: [`composer-h1.md`](handoff/composer-h1.md). ARCHITECTURE §16 |
| Theme system — token contract, eight built-in themes, user themes from JSON | PR #63 `46df20d`. User docs: [`themes.md`](themes.md). |
| Pitch Coach **phase 3** — per-note scoring, stored pitch curve, take report | PR #61 `c14916d`. [`pitch-coach-PROGRESS.md`](superpowers/plans/2026-08-16-pitch-coach-PROGRESS.md) |
| Lanes UX — rename, fold (lane + group), group, drag-reorder, and the timeline scroll/alignment fix | PR #60 `5f891cb`. Handoff: [`lanes-ux.md`](handoff/lanes-ux.md) |
| Pitch Coach **phase 2** — panel, frame bus, lane geometry, prefs | PR #58 `7c3cb87`. Adds `pitch_unsubscribe` (additive). |
| G1 Tasks 2–4 — insert ops, commands, `HostRole::Effect`, HostForward restore | PR #55 `118ae23`. Handoff: [`g1-insert-fx.md`](handoff/g1-insert-fx.md) |
| G1 Task 1 — `InsertSlot` on `TrackState.inserts` | PR #52 `5c338ff` |
| Pitch Coach phase 1 | PR #49 `84b0313` + PR #54 `f451a5a` (detection off the RT callback, listen mid-take, `pitch_check`). [`pitch-coach-PROGRESS.md`](superpowers/plans/2026-08-16-pitch-coach-PROGRESS.md) |
| MIDI launch v0.1 | PR #42 + #50. Hardware GATE / sustain still open: [`backlog/midi-launch.md`](backlog/midi-launch.md) |
| External audio editor | PR #48 |
| Gesture tokens | PR #47 |
| Tracks A–F / Plan F | log: [`handoff/landed-tracks.md`](handoff/landed-tracks.md). Rulings: [`PHASE4-PLAN.md`](PHASE4-PLAN.md) |
