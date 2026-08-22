# Backlog — Windows build

**Status: open in [PR #62](https://github.com/knobo/aura-daw/pull/62)**,
branch `feat/windows-release`, opened 2026-08-17 and untouched since.
Verified against the branch head `b2816c4` on 2026-08-22.

This page previously said the work's whereabouts were unknown — a review
had found it uncommitted in the main checkout on 2026-08-17, and the
checkout had since moved to `main` and gone clean. It was not lost: it
was committed and pushed as PR #62 the same day. The `lv2` Cargo feature
the build depends on is separate and landed earlier, in PR #57.

## What the PR does

Builds a Windows x64 installer on every `v*` tag and on demand (Actions →
*Release (Windows)* → *Run workflow*). Tag runs attach the NSIS
`-setup.exe` and the MSI to a **draft** GitHub release; manual runs leave
them as workflow artifacts. Nothing publishes on its own.

The blocker it works around: `livi 0.7` → `lilv-sys 0.2.1` has no build
script, just `#[link(name = "lilv-0")]`, so it expects a *system* lilv at
link time and there is none on Windows. Windows therefore compiles
`--no-default-features` and hosts CLAP only, with `plugins::lv2_host`
resolving to its stub.

Nine files: `release-windows.yml`, `tests.yml`, `tauri.conf.json`,
`icon.ico`, `control/export.rs`, `plugins/scan.rs`, `plugins/state.rs`,
README and CONTRIBUTING.

## Before it merges

**The branch is 41 commits behind `origin/main`** and has not been
touched since 2026-08-17. Merge `origin/main` in and re-run the gates
before reviewing anything below.

Four of the five review points raised on 2026-08-17 are still live on the
branch head. Line numbers verified 2026-08-22.

1. ~~`src-tauri/icons/icon.ico` is untracked while `tauri.conf.json`
   references it.~~ **Resolved** — the icon is committed in the PR.
2. **`bundle.active: true` with `"targets": "all"`** enables bundling on
   every platform, so a local Linux `tauri build` now runs the AppImage
   bundler (which downloads `linuxdeploy` mid-build) and emits deb/rpm
   with no `depends` and no sidecar resources. Scope it via
   `tauri.windows.conf.json` or `"targets": ["nsis", "msi"]`.
3. **`control/export.rs:291`** still hard-codes ``apt install ffmpeg`` in
   `capabilities()` — one function away from the new platform-aware `HOW`
   at `:268`, which the PR did add.
4. **`control/export.rs:880` and `:894`** assert on `"apt install
   ffmpeg"`. Because `missing_ffmpeg_error` is now platform-aware, those
   assertions pass on Linux and fail on Windows — the platform being
   added. Assert on `"requires ffmpeg"` instead.
5. **CI still does not compile the `#[cfg(target_os = "windows")]`
   branches.** The PR adds `cargo check --locked --no-default-features
   --all-targets` to `tests.yml`, which covers the *feature* axis the
   Windows build uses — but a check running on ubuntu compiles no
   `target_os = "windows"` code, so the new cfg branches in `export.rs`
   and `scan.rs` are still unbuilt by CI, which is exactly the rot the
   step's own comment warns about.
