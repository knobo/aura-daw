# Backlog — Linux packaging

**Status: backlog, nothing built yet.** `bundle.active` is `false` in
`src-tauri/tauri.conf.json:29` on `main`, so `tauri build` today produces a
binary and no package at all. Findings below verified against `main` at
`76d1a39` on 2026-08-22.

Sibling track: [`windows-build.md`](windows-build.md) (open in PR #62).
That PR flips `bundle.active` to `true` — read its review point 2 before
touching anything here, because the two tracks collide on exactly that
line.

## Decision: ship a `.deb`, not an AppImage

An AppImage was the obvious first idea and it is the wrong fit. AURA
depends on system-installed content at runtime that a bundle cannot
carry:

- **The user's own LV2/CLAP plugins.** These are the point of the app.
  They live in system and home plugin directories and are found by lilv's
  path scanning — user content by definition, never bundleable.
- **ffmpeg**, for every non-WAV import and export.
- **ALSA/JACK.** Bundling `libasound` is a known AppImage hazard: the
  bundled library meets the system's plugin modules and audio breaks in
  ways that are hard to diagnose.
- **The Python sidecar stack.** torch plus ACE-Step is gigabytes. It is
  not going inside any package we ship.

So an AppImage would promise self-containment, fail to deliver it, and
still require a paragraph of "install these five things first" in the
release notes. A `.deb` states the same requirements honestly in
`Depends:` and lets apt resolve them — which also matches the project's
existing apt-centric setup story (`scripts/setup.sh`, README quickstart).

Practical argument on top of the principled one: Tauri emits `.deb` with
no extra tooling, while its AppImage bundler downloads `linuxdeploy`
mid-build and needs FUSE.

`rpm` is close to free once `.deb` works (same Tauri bundler, different
target) and can follow.

## Blockers

**1. Sidecar scripts are not in the package.** `sidecars/*.py` is not
listed in `bundle.resources`, so a packaged AURA would ship with no AI
workers at all. `resolve_script()`
(`src-tauri/src/sidecars/jobs.rs:551`) searches next to the executable
and up to three parents, which matches the dev layout
(`src-tauri/target/{debug,release}`) but not an installed `.deb`, where
the binary is in `/usr/bin` and resources are not. The hook already
exists: `AURA_SIDECAR_DIR` is checked first (`jobs.rs:553`), and the
not-found error already names it (`jobs.rs:586`). Point it at the
installed resource directory.

**2. Icons are incomplete.** `src-tauri/icons/` holds `32x32.png`,
`128x128.png` and a 512×512 `icon.png`, and `bundle.icon`
(`tauri.conf.json:31`) references only the last of those. Regenerate the
full set with `npx tauri icon src-tauri/icons/icon.png`.

**3. `"targets": "all"` is too broad.** With bundling enabled it runs
every platform's bundler, including the AppImage path this track has
decided against. Scope it — per-platform config files, or an explicit
`["deb"]`.

**4. `Depends:` has to be written by hand.** Tauri does not infer the
runtime dependencies above. Without them the package installs and then
fails at first use.

**5. Linux builds *with* the `lv2` feature** — the opposite of Windows.
`livi` → `lilv-sys` links the system lilv, which exists here
(`liblilv-dev`) and is why LV2 hosting is Linux-only for now. Do not copy
the `--no-default-features` flag from the Windows job.

**6. glibc floor.** The package will require the glibc of whatever runner
builds it. GitHub's `ubuntu-22.04` runner is on its way out, so 24.04
(glibc 2.39) is the realistic floor. Say so in the release notes rather
than discovering it through issues.

## What stays the user's responsibility

This is a feature, not a gap, and the code already handles it well:

- **ffmpeg** — `missing_ffmpeg_error()`
  (`src-tauri/src/control/export.rs:253`) returns an actionable message
  naming the package instead of crashing. WAV import/export works
  without it.
- **Plugins** — user content, found by scanning.
- **Python / models** — document `AURA_SIDECAR_SIMULATE=1` prominently.
  It makes every AI worker emit deterministic placeholder output, so the
  whole generate → import → hear-it loop works on a machine with no model
  stack. `scripts/setup.sh --python` / `--models` builds the real one for
  those who want it.

## Note on verification

None of this can be verified on the maintainer's current machine: it is
missing `libwebkit2gtk-4.1-dev`, `libjavascriptcoregtk-4.1-dev`,
`libsoup-3.0-dev`, `libxdo-dev`, `libayatana-appindicator3-dev` and
`liblilv-dev`, and sudo needs a password there. The first green Linux
package has to come from CI, so expect the first PR on this track to take
a couple of runs to land.

## Next steps (when picked up)

Land PR #62 first — it owns `bundle.active` and the `tauri.conf.json`
bundle block, and doing Linux packaging in parallel guarantees a
conflict. Then:

- Scope `targets` per platform and add a Linux job to the release
  workflow, reusing the pattern already in `release-windows.yml`: tag →
  **draft** release, a version guard that refuses when the tag and
  `tauri.conf.json` disagree, and an explicit "was a package actually
  produced" step (`tauri build` can succeed while bundling nothing).
- Add `sidecars/*.py` to `bundle.resources` and set `AURA_SIDECAR_DIR`
  from the install location.
- Regenerate icons.
- Write `Depends:` and the release-note preamble about ffmpeg, plugins
  and the Python stack.
