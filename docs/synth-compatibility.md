# AURA synth compatibility sweep — wave 1C (2026-08-12)

Every synth below was driven through the same acceptance harness the
ZynAddSubFX gate uses (ARCHITECTURE §15, PHASE3-PLAN contract 7):
instantiate through the shared `aura-plugin-host` thread → build an RT node
→ queue a note → render headlessly → assert non-silence (plus f0 pitch where
the synth is tonal). Machine: Ubuntu 24.04 (noble), AURA LV2 host = livi 0.7
/ liblilv, CLAP host = clack 0.1.x.

Crash-prone plugins are probed in a **subprocess** (`plugins/patches.rs`
tests `sweep_*_probed_in_subprocess`, scan-worker pattern: the test binary
re-executes itself with an `AURA_SYNTH_PROBE` env guard). A plugin segfault
is therefore a recorded verdict, never a dead test suite.

## Results

| Synth | Version | Format | Install source | Instantiate | Render (note → audio) | Params | Stability in-process | Notes |
|---|---|---|---|---|---|---|---|---|
| ZynAddSubFX | 3.0.6 | LV2 | apt `zynaddsubfx-lv2` (+ `zynaddsubfx-data` banks) | OK | OK — pitched (f0 ≈ 440 Hz gate) | OK (control ports) | **Stable** (gating acceptance + state round-trip green) | Bank-patch loading works via state-XML splice (`plugins/patches.rs`); demo song v2 uses it |
| mda EPiano | 1.2.10 | LV2 | apt `mda-lv2` (pre-existing) | OK | OK — audible | OK (12 ports) | Stable | Pre-existing param-contract test |
| synthv1 | 0.9.34 | LV2 | apt `synthv1-lv2` | OK | OK — pitched (f0 ≈ 440 Hz) | OK | **Stable** | Default patch sounds immediately; permanent gated test |
| geonkick | 2.10.2 | LV2 | apt `geonkick` | OK | OK — kick renders (non-silence gate; pitch n/a) | OK | **Stable** (needs ~1.5 s warm-up: percussion is synthesized asynchronously after instantiation) | Permanent gated test with warm-up |
| Surge XT | 1.3.4 | **CLAP** | official deb (`surge-synthesizer/releases-xt`) | OK | OK — audible (rms ≈ 0.08) | OK (params ext) | Stable in the isolated probe | First third-party CLAP instrument verified through the clack host |
| padthv1 | 0.9.34 | LV2 | apt `padthv1-lv2` | OK | OK in an **isolated subprocess** (rms ≈ 0.09) | OK | **UNSTABLE co-hosted**: aborts with heap corruption (`free(): corrupted unsorted chunks`) when rendered in the same process as other plugins/tests | Subprocess-contained probe; needs the out-of-process bridge (D-11) before it can be offered |
| Yoshimi | 2.3.2 | LV2 | apt `yoshimi` (+ `yoshimi-data`) | OK (engine boots asynchronously, scans banks) | OK in an **isolated subprocess** (rms ≈ 0.05, needs ~3 s warm-up) | OK | **UNSTABLE co-hosted / FLAKY**: silent when driven immediately; **intermittent SIGSEGV** when warm-up render blocks run in a process hosting other plugins (observed killing the whole test binary across parallel agent runs — the trigger for the subprocess containment) | Subprocess-contained probe; same D-11 story as padthv1 |
| Cardinal (Synth) | 26.02 | CLAP | official tarball (`DISTRHO/Cardinal`, `CardinalSynth.clap` installed to `/usr/lib/clap/Cardinal.clap/`) | OK (uid-parse bug fixed 2026-08-12: `parse_clap_uid` now `split_once('#')`) | **Silent** (rms 0 even with an 8 s warm-up + 3 s render) | not evaluated (no audio to verify against) | Stable in the isolated probe (instantiates, activates, renders, exits cleanly — no crash) | Verdict (integration re-probe): hosting works, but CardinalSynth renders silence headlessly. Expected cause: the bundle was installed **without Cardinal's resource dir**, so the default patch/rack cannot load and the rack is empty — no modules, no sound. Re-test with full resources installed before offering it |
| amsynth | 1.13.2 | — | apt `amsynth` | n/a | n/a | n/a | n/a | Noble package ships **DSSI + VST2 only** (the LV2 metainfo file is a leftover; no `.lv2` bundle installed). AURA hosts LV2/CLAP only — cannot be tested |
| sfizz | — | — | not packaged | n/a | n/a | n/a | n/a | **Not in Ubuntu 24.04 (noble) repos** (removed from Debian/Ubuntu); official project distributes source / OBS repos only, no plain deb or extractable binary — skipped per "straightforward install only" rule |
| Helm / OB-Xd | — | — | not packaged | n/a | n/a | n/a | n/a | Not in noble repos; no official apt-installable artifact |

**Score: 6 of 8 testable synths render audio through AURA's hosts**
(Zyn, EPiano, synthv1, geonkick, Surge XT stable in-process; padthv1 +
Yoshimi render only in isolation), 1 hosts cleanly but renders silence
headlessly (Cardinal — uid-parse bug fixed; silence attributed to missing
resource files), 2 untestable on this distro (amsynth: no LV2/CLAP build;
sfizz: unpackaged).

## Takeaways

* **The Zyn-class path is solid**: LV2 instruments with synchronous engines
  (Zyn, synthv1, mda) pass the full pitched-render acceptance in-process.
* **Asynchronous boot needs warm-up**: geonkick (percussion synthesis) and
  Yoshimi (engine start) render silence if driven immediately after
  instantiation. The harness warms nodes up with silent render blocks; a
  future UX nicety is treating the first ~2 s after instantiation as
  "loading".
* **In-process co-hosting is the real risk (D-11 confirmed empirically)**:
  padthv1 and Yoshimi both work in a clean single-plugin process and both
  corrupt/crash when co-hosted. The out-of-process bridge planned in
  ARCHITECTURE §15.3 / SCALABILITY §2 is what unlocks them — the
  `LiveNodeCell` seam is the insertion point.
* **CLAP uid encoding bug (FIXED 2026-08-12)**: `clap:<path>#<id>` was
  ambiguous because DPF derives clap ids like
  `studio.kx.distrho.cardinal#synth`. `parse_clap_uid`
  (src-tauri/src/plugins/clap_host.rs) now splits at the FIRST `#` (paths
  with `#` are far rarer than DPF ids with `#`). With the fix Cardinal
  instantiates and activates cleanly; it renders silence headlessly, most
  plausibly because the install lacks Cardinal's resource dir (default
  rack cannot load → empty rack).

## System packages installed for this sweep (apt)

`zynaddsubfx-data`, `yoshimi`, `yoshimi-data`, `amsynth`, `geonkick`,
`synthv1-lv2`, `padthv1-lv2` — plus, from official upstream releases:
`surge-xt` 1.3.4 (deb, dpkg -i) and Cardinal 26.02 CLAP binaries copied to
`/usr/lib/clap/Cardinal.clap/` (`Cardinal.clap`, `CardinalFX.clap`,
`CardinalSynth.clap`; resources not installed — headless probe only).
