//! ui-probe: WebKitGTK measurement harness for AURA's render-architecture gate.
//!
//! One binary, test selected by CLI arg:
//!   ui-probe pointer        -> 600x400 window at (100,100), pointer-capture drag probe
//!   ui-probe gfx            -> WebGL2 / Canvas2D throughput microbenchmark (self-driving)
//!   ui-probe latency        -> keydown-to-paint latency, simple paint
//!   ui-probe latency-heavy  -> keydown-to-paint latency, full-canvas gradient paint
//!
//! Optional second arg overrides the output file stem (default = test name).
//! Results are written as JSON to results/<stem>.json (relative to CWD),
//! delivered from page JS via wry's IPC handler. The process exits once the
//! page posts its result, or after a hard 180 s safety timeout.
//!
//! Pinned to wry 0.55.1 + tao 0.35.3 == the exact versions in the product's
//! src-tauri/Cargo.lock, so this measures what Tauri v2 actually ships.

use std::fs;
use std::path::PathBuf;

use tao::dpi::{LogicalPosition, LogicalSize};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

#[cfg(target_os = "linux")]
use tao::platform::unix::WindowExtUnix;
#[cfg(target_os = "linux")]
use wry::WebViewBuilderExtUnix;

const POINTER_HTML: &str = include_str!("../html/pointer.html");
const GFX_HTML: &str = include_str!("../html/gfx.html");
const LATENCY_HTML: &str = include_str!("../html/latency.html");

fn main() -> wry::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let test = args.get(1).map(String::as_str).unwrap_or("gfx").to_string();
    let stem = args.get(2).cloned().unwrap_or_else(|| test.clone());

    let (html, title, size, init_script): (&str, String, LogicalSize<f64>, String) = match test.as_str() {
        "pointer" => (
            POINTER_HTML,
            "ui-probe-pointer".into(),
            LogicalSize::new(600.0, 400.0),
            String::new(),
        ),
        "gfx" => (
            GFX_HTML,
            "ui-probe-gfx".into(),
            LogicalSize::new(1300.0, 760.0),
            String::new(),
        ),
        "latency" => (
            LATENCY_HTML,
            "ui-probe-latency".into(),
            LogicalSize::new(1600.0, 900.0),
            "window.PROBE_CONFIG = { mode: 'simple' };".into(),
        ),
        "latency-heavy" => (
            LATENCY_HTML,
            "ui-probe-latency".into(),
            LogicalSize::new(1600.0, 900.0),
            "window.PROBE_CONFIG = { mode: 'heavy' };".into(),
        ),
        other => {
            eprintln!("unknown test '{other}'; use pointer | gfx | latency | latency-heavy");
            std::process::exit(2);
        }
    };

    let out_dir = PathBuf::from("results");
    fs::create_dir_all(&out_dir).expect("create results dir");
    let out_path = out_dir.join(format!("{stem}.json"));

    // Hard safety timeout so a wedged page can never hang the driver script.
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(180));
        eprintln!("ui-probe: SAFETY TIMEOUT (180 s), exiting 3");
        std::process::exit(3);
    });

    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title(&title)
        .with_inner_size(size)
        .with_position(LogicalPosition::new(100.0, 100.0))
        .build(&event_loop)
        .expect("build window");

    println!(
        "ui-probe: test={test} scale_factor={} inner={:?} outer_pos={:?}",
        window.scale_factor(),
        window.inner_size(),
        window.outer_position().ok()
    );

    let out_path_ipc = out_path.clone();
    let mut builder = WebViewBuilder::new()
        .with_html(html)
        .with_ipc_handler(move |req| {
            let body = req.body();
            if let Err(e) = fs::write(&out_path_ipc, body) {
                eprintln!("ui-probe: FAILED to write {}: {e}", out_path_ipc.display());
                std::process::exit(4);
            }
            println!("ui-probe: RESULT WRITTEN {}", out_path_ipc.display());
            std::process::exit(0);
        });
    if !init_script.is_empty() {
        builder = builder.with_initialization_script(&init_script);
    }

    #[cfg(target_os = "linux")]
    let webview = {
        let vbox = window.default_vbox().expect("gtk default vbox");
        builder.build_gtk(vbox)?
    };
    #[cfg(not(target_os = "linux"))]
    let webview = builder.build(&window)?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        let _keep_alive = (&webview, &window);
        if let Event::WindowEvent { event: WindowEvent::CloseRequested, .. } = event {
            *control_flow = ControlFlow::Exit;
        }
    });
}
