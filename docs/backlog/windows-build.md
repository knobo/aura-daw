# Backlog — Windows build (status UNCLEAR, verify before acting)

Recorded 2026-08-17, updated 2026-08-21. **Nothing here has been
verified beyond reading a diff — treat every item as a lead, not a
verdict.**

A review once found uncommitted Windows-build work in the main checkout,
on branch `docs/pitch-coach-handoff`: an `lv2` Cargo feature with an
`lv2_host_stub`, Windows CLAP search paths, platform-aware ffmpeg hints,
a `release-windows.yml`, and `bundle.active: true`. The main checkout has
since moved to `main` and is clean, with none of those files present or
tracked. **It is not known whether that work was committed elsewhere,
intentionally dropped, or is sitting in a stash or another checkout —
check before assuming it is gone.** It was never in a PR.

Five things the owner had not seen, recorded here because they exist
nowhere else:

1. **`src-tauri/icons/icon.ico` is untracked** while `tauri.conf.json` (a
   tracked modification) references it. `git commit -am` lands the
   reference without the asset and the first `v*` tag fails at the bundle
   step. `.github/workflows/release-windows.yml` is untracked too.
2. **`bundle.active: true` with `targets: "all"`** enables bundling on
   every platform, so a local Linux `tauri build` runs the AppImage
   bundler (which downloads `linuxdeploy` mid-build) and emits deb/rpm
   with no `depends` and no sidecar resources. Scope it via
   `tauri.windows.conf.json` or `"targets": ["nsis", "msi"]`.
3. **`control/export.rs:291`** still hard-codes `apt install ffmpeg` in
   `capabilities()`, one function away from the new platform-aware `HOW`.
4. **`control/export.rs:880`/`:894`** assert on `"apt install ffmpeg"`,
   so `cargo test` is red on Windows — the platform being added. Assert
   on `"requires ffmpeg"` instead.
5. **`.github/workflows/tests.yml:67`** only exercises the `lv2` axis on
   ubuntu; nothing in CI compiles the new `#[cfg(target_os = "windows")]`
   branches, which is exactly the rot the step's own comment warns about.
