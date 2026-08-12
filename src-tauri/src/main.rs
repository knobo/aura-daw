// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Sacrificial plugin-scan worker (PHASE3-PLAN contract 8): when re-exec'd
    // with AURA_SCAN_WORKER=1 this process only reads CLAP descriptors and
    // speaks NDJSON on stdout — a crashing bundle kills this worker, not AURA.
    if aura_lib::plugins::scan_worker::is_worker_invocation() {
        std::process::exit(aura_lib::plugins::scan_worker::worker_main());
    }
    aura_lib::run()
}
