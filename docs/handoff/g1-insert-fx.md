# G1 Tasks 1–5 — insert FX channel, host, and mixer strip

Landed 2026-08-18. **Do not restart.** Next implementer starts at
**Task 6** (PDC) in
`docs/superpowers/plans/2026-08-16-plan-g1-insert-fx-pdc.md`.

| Slice | Pointer |
|---|---|
| Task 1 `InsertSlot` on `TrackState.inserts` | PR #52 `5c338ff` |
| Tasks 2–4 ops, commands, `HostRole::Effect`, HostForward restore | PR #55 `118ae23` |
| Task 5 mixer strip: source-sum → inserts REPLACE → shared fader | PR #66 `c99293b` |

What exists now: document slots, four insert ops + undo, Tauri
`insert_add` / `insert_remove` / `insert_reorder` / `insert_set_bypass`,
effect instantiate gate (G-6/G-7), `HostRole::Effect` + Replace IO +
`latency_samples()`, restore via insert membership (undo / journal
replay / project-open), and the unified RT mixer strip
(`audio/insert.rs`: `InsertNode`/`InsertNodeCell`/`InsertNodeRegistry`/
`compile_inserts`; `audio/mixer.rs`: `render_impl` and
`render_live_input_only` walk clips → live → inserts REPLACE → the one
shared `apply_fader`). `compile_inserts` does not instantiate CLAP/LV2
and is not yet wired into `engine::rebuild` — that plus real hosting is
Task 7. Bypass is true bypass (skips `process()`). PDC is a hook only:
`RtTrack.pdc: Option<()>`, always `None`.

What does **not** exist yet: PDC delay lines, rebuild compile (wiring
`compile_inserts` into `engine::rebuild`), offline-bounce compile,
frontend IPC types, insert-chain UI. Adding an insert today writes the
document and hosts the plugin, and Task 5's strip *would* run it
through the chain — but nothing calls `compile_inserts` from
production code yet, so **the mix still does not hear it** until
Task 7 wires rebuild through.

## Future work (plan order)

6. PDC `DelayLine` + `compile_pdc`.
7. Rebuild / offline bounce / latency-change recompile — wires
   `compile_inserts` into `engine::rebuild` for real; this is where an
   insert first becomes audible end-to-end.
8. IPC types, backend adapter, plugin-binding.
9. `InsertChain.svelte`.
10. Handoff + dated counts.

## Task 5 deferred minors (do not block Task 6)

From `/code-review high` on PR #66. Parked, not load-bearing:

- `InsertNodeCell`/`InsertNodeRegistry` duplicate `LiveNodeCell`
  (`audio/rt.rs`) / `LiveNodeRegistry` (`midi/playback.rs`) near-verbatim
  — the plan's own interface spec calls for this shape, but a shared
  generic (`RtCell<T>`, `NodeRegistry<T>`) is a clean follow-up once a
  third RCU-cell consumer shows up.
- `process_inserts` runs the full insert chain unconditionally on
  muted/solo-excluded tracks (the `on` flag is only applied afterward
  in `apply_fader`) — plausibly deliberate (keeps plugin tails warm for
  click-free unmute) but undocumented either way; confirm the intent
  before "optimizing" it away.
- `prime_live` / `render_live_into` / `live_all_notes_off` each repeat
  the same `let Some(live) = &tr.live else { return }` + `unsafe {
  live.node.rt_mut() }` boilerplate — a shared accessor would cut it to
  one place.

G2 bus+sends / G3 sidechain / G4 envelope-follower wait. **PDC before
sends.** No stock FX. `OP_FORMAT_VERSION` stays 2.

## Deferred minors (do not block Task 5)

From per-task reviews of #55. Parked, not load-bearing:

- Dead `InsertReorder` empty-list branch; extra G-7 `apply_raw` tests
  (empty id / duplicate `instance_id`); `changeset_from_insert_ops`
  only samples `InsertAdd`; `track_heap` ignores `InsertSlot` strings.
- Duplicated effect-as-instrument gate (`audio/mod.rs` vs
  `ControlPlane`); G-7 insert-membership reject untested in isolation;
  process-global scan seed in a unit test.
- LV2 Effect allows `audio_inputs > 2`; latency name `contains()`
  fallback is broader than "named latency"; CLAP Replace peak pin is
  plugin-dependent; main-out error still says "instruments only";
  `latency_of` comment vs host-map probe.

## How to verify (no UI yet)

```
timeout 180 cargo test --manifest-path src-tauri/Cargo.toml --lib -- \
  insert_ instance_is_insert plugin_add_of_an_insert_rehosts \
  reactivate_restored_hosts instantiate_effect_accepts \
  clap_node_replace lv2_node_replace
```

Owner ear-check of an insert is Task 5+.
