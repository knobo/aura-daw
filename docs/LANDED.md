# Landed — do not restart

Everything in this table is merged to `main`. If a job you are about to
start appears here, you are about to redo it. Open the pointer instead.

Newest first.

## Plugin manager and native floating GUI

Two PRs, one track. Details, scope calls and what is left:
[`backlog/plugin-manager.md`](backlog/plugin-manager.md).

| What | Where |
|---|---|
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
