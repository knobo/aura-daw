# G1 Tasks 1–4 — insert FX channel + host

Landed 2026-08-16. **Do not restart.** Next implementer starts at
**Task 5** (mixer strip) in
`docs/superpowers/plans/2026-08-16-plan-g1-insert-fx-pdc.md`.

| Slice | Pointer |
|---|---|
| Task 1 `InsertSlot` on `TrackState.inserts` | PR #52 `5c338ff` |
| Tasks 2–4 ops, commands, `HostRole::Effect`, HostForward restore | PR #55 `118ae23` |

What exists now: document slots, four insert ops + undo, Tauri
`insert_add` / `insert_remove` / `insert_reorder` / `insert_set_bypass`,
effect instantiate gate (G-6/G-7), `HostRole::Effect` + Replace IO +
`latency_samples()`, and restore via insert membership (undo / journal
replay / project-open).

What does **not** exist yet: mixer walk, PDC delay lines, rebuild
compile, frontend IPC types, insert-chain UI. Adding an insert today
writes the document and hosts the plugin; **the mix does not hear it**
until Task 5.

## Future work (plan order)

5. Mixer strip: source sum → inserts in order → existing fader.
6. PDC `DelayLine` + `compile_pdc`.
7. Rebuild / offline bounce / latency-change recompile.
8. IPC types, backend adapter, plugin-binding.
9. `InsertChain.svelte`.
10. Handoff + dated counts.

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
