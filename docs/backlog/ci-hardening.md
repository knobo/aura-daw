# CI hardening — plugin-gated and real-model tests

**Status:** backlog. First version of `.github/workflows/tests.yml` runs
`cargo test` and the frontend suite (vitest, svelte-check, build) on every
PR, but two categories of tests are currently skipped rather than exercised:

1. **Plugin-gated tests** (Zyn acceptance + state round-trip, CLAP lifecycle,
   LV2 params). These need `zynaddsubfx-lv2`, `dpf-plugins-clap`/
   `dpf-plugins-lv2`, `mda-lv2` installed (see README quickstart). The CI
   `rust` job does not install them, so these tests skip cleanly (by design —
   see CONTRIBUTING.md) instead of actually running.
2. **Real-model integration tests** (`src-tauri/tests/real_models.rs`). These
   need `AURA_REAL_MODELS=1` plus a real `.venv-sidecars` Python stack
   (Demucs/ACE-Step etc.) and are not run in CI at all.

## Why deferred

Getting a first green CI pipeline landed mattered more than full coverage.
Installing plugin packages and building `.venv-sidecars` in CI both add
non-trivial runtime/complexity to the workflow, so they were left out of v1.

## Next steps (when picked up)

- Add `zynaddsubfx-lv2 dpf-plugins-clap dpf-plugins-lv2 mda-lv2` to the apt
  install step in the `rust` job so the plugin-gated tests actually run
  instead of skipping.
- Decide whether real-model tests belong in the PR-blocking workflow at all
  (they're heavy) — likely a separate, manually-triggered or nightly workflow
  that sets up `.venv-sidecars` and runs `AURA_REAL_MODELS=1 cargo test
  --test real_models`.
