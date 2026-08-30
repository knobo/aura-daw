// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Sacrificial plugin-scan worker (PHASE3-PLAN contract 8): when re-exec'd
    // with AURA_SCAN_WORKER=1 this process only reads CLAP descriptors and
    // speaks NDJSON on stdout — a crashing bundle kills this worker, not AURA.
    if aura_lib::plugins::scan_worker::is_worker_invocation() {
        std::process::exit(aura_lib::plugins::scan_worker::worker_main());
    }

    // Linux/WebKitGTK: under heavy GPU load (ACE-Step inference) the DMA-BUF
    // renderer can white-flash or kill the webview's web process, reloading
    // the UI mid-session. Fall back to WebKit's non-DMA-BUF path — the
    // accepted Tauri workaround (tauri-apps/tauri#9304). Set BEFORE the
    // webview initializes, and only when the user hasn't chosen a value
    // (export WEBKIT_DISABLE_DMABUF_RENDERER=0 to opt back in).
    //
    // This was removed in #138 on the strength of what it costs — idle
    // web-process CPU of 101% of a core against 4.8% with the renderer
    // enabled, because disabling it denies the web process a GL context and
    // Skia rasters every frame in software (docs/backlog/webview-rendering.md
    // has the matrix). It went straight back in #139: the app did not work
    // properly without it in ordinary use, not merely under GPU load. Do not
    // delete this block again on the CPU numbers alone; that experiment has
    // been run.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    aura_lib::run()
}
