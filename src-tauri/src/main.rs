// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Sacrificial plugin-scan worker (PHASE3-PLAN contract 8): when re-exec'd
    // with AURA_SCAN_WORKER=1 this process only reads CLAP descriptors and
    // speaks NDJSON on stdout — a crashing bundle kills this worker, not AURA.
    if aura_lib::plugins::scan_worker::is_worker_invocation() {
        std::process::exit(aura_lib::plugins::scan_worker::worker_main());
    }

    // Linux/WebKitGTK: AURA used to set WEBKIT_DISABLE_DMABUF_RENDERER=1 here,
    // the accepted Tauri workaround (tauri-apps/tauri#9304) for the DMA-BUF
    // renderer white-flashing or killing the web process when the GPU is
    // saturated — in our case by ACE-Step inference.
    //
    // It is gone because of what it cost. Disabling that renderer denies the
    // web process a GL context, so Skia falls back to its CPU raster pipeline
    // and every frame of every session is rastered in software. Measured on
    // the WebKitGTK 2.52.3 Ubuntu 24.04 ships, idle with the transport
    // stopped: 101% of a core with the workaround, 4.8% without it. The full
    // matrix is in docs/backlog/webview-rendering.md. That price was paid
    // continuously against a bug that only appears under heavy GPU load.
    //
    // Nothing is set here now, so WebKit reads the environment itself and a
    // user who does hit the flash still has the escape hatch:
    // WEBKIT_DISABLE_DMABUF_RENDERER=1 restores the old behaviour, and
    // README's Linux/GPU troubleshooting section says so. If you are about to
    // reinstate this block, measure first — the numbers above are what it
    // has to beat.

    aura_lib::run()
}
