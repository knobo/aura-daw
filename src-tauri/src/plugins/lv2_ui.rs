//! LV2 floating UI host (ARCHITECTURE §15.3 / SCALABILITY steg 3).
//!
//! v1 opens plugin-owned windows through `ui:showInterface` and drives
//! `ui:idleInterface` on the shared plugin main thread. No XEmbed, no
//! Gtk parent, no Wayland embed. The generic param panel stays in Tauri.

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::process::{Child, Command, Stdio};
use std::ptr;

use parking_lot::Mutex;

const RTLD_NOW: c_int = 2;

#[link(name = "dl")]
extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
}

const SHOW_URI: &CStr = c"http://lv2plug.in/ns/extensions/ui#showInterface";
const IDLE_URI: &CStr = c"http://lv2plug.in/ns/extensions/ui#idleInterface";
const MAP_URI: &CStr = c"http://lv2plug.in/ns/ext/urid#map";
const OPTIONS_URI: &CStr = c"http://lv2plug.in/ns/ext/options#options";
const INSTANCE_ACCESS_URI: &CStr = c"http://lv2plug.in/ns/ext/instance-access";
const ATOM_FLOAT_URI: &CStr = c"http://lv2plug.in/ns/ext/atom#Float";
const SAMPLE_RATE_URI: &CStr = c"http://lv2plug.in/ns/ext/parameters#sampleRate";

#[repr(C)]
struct Lv2Feature {
    uri: *const c_char,
    data: *mut c_void,
}

#[repr(C)]
struct Lv2UridMap {
    handle: *mut c_void,
    map: Option<unsafe extern "C" fn(*mut c_void, *const c_char) -> u32>,
}

#[repr(C)]
struct Lv2Option {
    context: u32,
    subject: u32,
    key: u32,
    size: u32,
    type_: u32,
    value: *const c_void,
}

#[repr(C)]
struct Lv2uiDescriptor {
    uri: *const c_char,
    instantiate: Option<
        unsafe extern "C" fn(
            descriptor: *const Lv2uiDescriptor,
            plugin_uri: *const c_char,
            bundle_path: *const c_char,
            write_function: Option<Lv2uiWriteFn>,
            controller: *mut c_void,
            widget: *mut *mut c_void,
            features: *const *const Lv2Feature,
        ) -> *mut c_void,
    >,
    cleanup: Option<unsafe extern "C" fn(*mut c_void)>,
    port_event: Option<
        unsafe extern "C" fn(*mut c_void, u32, u32, u32, *const c_void),
    >,
    extension_data: Option<unsafe extern "C" fn(*const c_char) -> *const c_void>,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Lv2uiShowInterface {
    show: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
    hide: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Lv2uiIdleInterface {
    idle: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
}

type Lv2uiWriteFn =
    unsafe extern "C" fn(*mut c_void, u32, u32, u32, *const c_void);
type Lv2uiDescriptorFn = unsafe extern "C" fn(u32) -> *const Lv2uiDescriptor;

struct UridMapInner {
    uris: Mutex<Vec<CString>>,
}

unsafe extern "C" fn map_cb(handle: *mut c_void, uri: *const c_char) -> u32 {
    if handle.is_null() || uri.is_null() {
        return 0;
    }
    let inner = &*(handle as *mut UridMapInner);
    let Ok(s) = CStr::from_ptr(uri).to_str() else {
        return 0;
    };
    let mut uris = inner.uris.lock();
    if let Some(i) = uris.iter().position(|u| u.to_str() == Ok(s)) {
        return (i + 1) as u32;
    }
    let Ok(owned) = CString::new(s) else {
        return 0;
    };
    uris.push(owned);
    uris.len() as u32
}

unsafe extern "C" fn ui_write(
    controller: *mut c_void,
    port_index: u32,
    buffer_size: u32,
    port_protocol: u32,
    buffer: *const c_void,
) {
    if controller.is_null() || buffer.is_null() {
        return;
    }
    if port_protocol != 0 || buffer_size != 4 {
        return;
    }
    let instance_id = &*(controller as *const String);
    let value = *(buffer as *const f32);
    super::lv2_host::global().set_params(instance_id, vec![(port_index, value)]);
}

struct DlLib(*mut c_void);

impl Drop for DlLib {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { dlclose(self.0) };
        }
    }
}

/// One open `ui:showInterface` window, owned by the plugin main thread.
pub struct OpenLv2Gui {
    _lib: DlLib,
    descriptor: *const Lv2uiDescriptor,
    /// Resolved ONCE at open, not per idle tick. `extension_data` is a
    /// pure function of the descriptor — the LV2 spec has it return a
    /// static pointer for a given URI — so re-querying it 66 times a
    /// second bought nothing and cost a readable log: Carla prints a line
    /// per call, and a ~40 minute session with one editor open produced
    /// 155k of them, burying every other message in the dev log.
    show_iface: Option<Lv2uiShowInterface>,
    idle_iface: Option<Lv2uiIdleInterface>,
    handle: *mut c_void,
    /// Kept alive so `ui_write` can read the instance id.
    #[allow(dead_code)]
    controller: Box<String>,
    _urid_inner: Box<UridMapInner>,
    _map: Box<Lv2UridMap>,
    _sample_rate: Box<f32>,
    _options: Box<[Lv2Option; 2]>,
    _features: Box<[Lv2Feature; 5]>,
    _feature_ptrs: Box<[*const Lv2Feature; 5]>,
    visible: bool,
    /// DPF idle() returns non-zero both when the user closed the window AND
    /// before it has mapped. Only treat non-zero as close after a 0.
    seen_idle_ok: bool,
    /// Zyn (DPF ExternalWindow) draws nothing itself; it expects the host
    /// to drive `zynaddsubfx-ext-gui` against the DSP's OSC port. We spawn
    /// that process without `--embed` (no XEmbed parent in v1).
    ext_child: Option<Child>,
    on_top_applied: bool,
}

// SAFETY: the GUI is only ever touched on the plugin main thread.
unsafe impl Send for OpenLv2Gui {}

impl OpenLv2Gui {
    pub fn idle(&mut self) -> bool {
        if let Some(child) = self.ext_child.as_mut() {
            if matches!(child.try_wait(), Ok(Some(_))) {
                self.ext_child = None;
                self.hide();
                return false;
            }
            let want = super::wm_stack::on_top();
            if self.on_top_applied != want {
                if super::wm_stack::apply_on_top_to_pid(child.id()) {
                    self.on_top_applied = want;
                }
            }
        }
        let Some(idle) = self.idle_iface else {
            return self.visible || self.ext_child.is_some();
        };
        let rc = unsafe { idle.idle.map(|f| f(self.handle)).unwrap_or(0) };
        if rc == 0 {
            self.seen_idle_ok = true;
            return true;
        }
        if self.seen_idle_ok && self.ext_child.is_none() {
            self.hide();
            false
        } else {
            true
        }
    }

    pub fn show(&mut self) -> Result<(), String> {
        let Some(show) = self.show_iface else {
            return Err("ui has no ui:showInterface".into());
        };
        let status = unsafe { show.show.map(|f| f(self.handle)).unwrap_or(-1) };
        if status != 0 {
            return Err(format!("ui:showInterface::show failed ({status})"));
        }
        self.visible = true;
        Ok(())
    }

    fn hide(&mut self) {
        if let Some(show) = self.show_iface {
            unsafe { show.hide.map(|f| f(self.handle)) };
        }
        self.kill_ext();
        self.visible = false;
    }

    /// Launch `zynaddsubfx-ext-gui` against the hosted DSP's OSC server.
    /// Port 0 means the instance has not published `osc_port` yet.
    pub fn attach_osc_gui(&mut self, port: u32) -> Result<(), String> {
        if port == 0 {
            return Err("dsp osc_port is 0 — lo server not up yet".into());
        }
        self.kill_ext();
        let uri = format!("osc.udp://localhost:{port}/");
        let child = Command::new("zynaddsubfx-ext-gui")
            .arg(&uri)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| {
                format!(
                    "failed to start zynaddsubfx-ext-gui for {uri} ({e}); \
                     is /usr/bin/zynaddsubfx-ext-gui on PATH?"
                )
            })?;
        log::info!("lv2 ui: spawned zynaddsubfx-ext-gui {uri} pid={}", child.id());
        self.ext_child = Some(child);
        Ok(())
    }

    fn kill_ext(&mut self) {
        if let Some(mut child) = self.ext_child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for OpenLv2Gui {
    fn drop(&mut self) {
        self.hide();
        if !self.handle.is_null() {
            if let Some(cleanup) = unsafe { (*self.descriptor).cleanup } {
                unsafe { cleanup(self.handle) };
            }
            self.handle = ptr::null_mut();
        }
    }
}

unsafe fn show_idle_ifaces(
    descriptor: *const Lv2uiDescriptor,
) -> (Option<Lv2uiShowInterface>, Option<Lv2uiIdleInterface>) {
    let ext = (*descriptor).extension_data;
    let Some(ext) = ext else {
        return (None, None);
    };
    let show = ext(SHOW_URI.as_ptr()).cast::<Lv2uiShowInterface>();
    let idle = ext(IDLE_URI.as_ptr()).cast::<Lv2uiIdleInterface>();
    (
        if show.is_null() { None } else { Some(*show) },
        if idle.is_null() { None } else { Some(*idle) },
    )
}

/// True when a lilv plugin has a UI binary that exports `ui:showInterface`.
pub fn plugin_has_show_ui(plugin: &livi::Plugin) -> bool {
    let Some(uis) = plugin.raw().uis() else {
        return false;
    };
    for ui in uis {
        if ui_exports_show(&ui) {
            return true;
        }
    }
    false
}

fn ui_exports_show(ui: &livi::lilv::ui::UI) -> bool {
    let Some(path) = ui_binary_path(ui) else {
        return false;
    };
    let Ok(lib) = dlopen_path(&path) else {
        return false;
    };
    let Ok(desc_fn) = ui_descriptor_fn(lib.0) else {
        return false;
    };
    let want = ui.uri().as_uri().map(str::to_owned);
    for i in 0..16u32 {
        let d = unsafe { desc_fn(i) };
        if d.is_null() {
            break;
        }
        let uri = unsafe { CStr::from_ptr((*d).uri) }.to_string_lossy();
        let matches = want.as_deref().is_none_or(|w| w == uri.as_ref());
        if !matches {
            continue;
        }
        let Some(ext) = (unsafe { (*d).extension_data }) else {
            continue;
        };
        let show = unsafe { ext(SHOW_URI.as_ptr()) };
        return !show.is_null();
    }
    false
}

fn ui_binary_path(ui: &livi::lilv::ui::UI) -> Option<String> {
    ui.binary_uri()?.path().map(|(_, p)| p)
}

fn ui_bundle_path(ui: &livi::lilv::ui::UI) -> Option<String> {
    ui.bundle_uri()?.path().map(|(_, p)| p)
}

fn dlopen_path(path: &str) -> Result<DlLib, String> {
    let c = CString::new(path).map_err(|_| format!("ui binary path contains NUL: {path}"))?;
    let h = unsafe { dlopen(c.as_ptr(), RTLD_NOW) };
    if h.is_null() {
        return Err(format!("dlopen failed: {path}"));
    }
    Ok(DlLib(h))
}

fn ui_descriptor_fn(lib: *mut c_void) -> Result<Lv2uiDescriptorFn, String> {
    let sym = unsafe { dlsym(lib, c"lv2ui_descriptor".as_ptr()) };
    if sym.is_null() {
        return Err("ui binary has no lv2ui_descriptor".into());
    }
    Ok(unsafe { std::mem::transmute(sym) })
}

/// Instantiate + `show()` the first `ui:showInterface` UI for `plugin`.
///
/// `plugin_handle` is the LV2_Handle of the hosted instance (shadow or RT)
/// passed as `instance-access` so DPF UIs (Zyn) attach to that instance.
pub fn open_show_interface(
    plugin: &livi::Plugin,
    instance_id: &str,
    plugin_handle: *mut c_void,
    sample_rate: f32,
) -> Result<OpenLv2Gui, String> {
    let uis = plugin
        .raw()
        .uis()
        .ok_or_else(|| format!("{} has no LV2 UI", plugin.uri()))?;
    let mut last_err = "no ui:showInterface UI".to_string();
    for ui in uis {
        match try_open_ui(&ui, plugin, instance_id, plugin_handle, sample_rate) {
            Ok(gui) => return Ok(gui),
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

fn try_open_ui(
    ui: &livi::lilv::ui::UI,
    plugin: &livi::Plugin,
    instance_id: &str,
    plugin_handle: *mut c_void,
    sample_rate: f32,
) -> Result<OpenLv2Gui, String> {
    let path = ui_binary_path(ui).ok_or("ui has no binary")?;
    let bundle = ui_bundle_path(ui).unwrap_or_else(|| {
        std::path::Path::new(&path)
            .parent()
            .map(|p| {
                let mut s = p.to_string_lossy().into_owned();
                if !s.ends_with('/') {
                    s.push('/');
                }
                s
            })
            .unwrap_or_else(|| path.clone())
    });
    let bundle = if bundle.ends_with('/') {
        bundle
    } else {
        format!("{bundle}/")
    };
    let lib = dlopen_path(&path)?;
    let desc_fn = ui_descriptor_fn(lib.0)?;
    let want = ui.uri().as_uri().map(str::to_owned);
    let mut descriptor = ptr::null();
    for i in 0..16u32 {
        let d = unsafe { desc_fn(i) };
        if d.is_null() {
            break;
        }
        let uri = unsafe { CStr::from_ptr((*d).uri) }.to_string_lossy();
        if want.as_deref().is_none_or(|w| w == uri.as_ref()) {
            descriptor = d;
            break;
        }
    }
    if descriptor.is_null() {
        return Err(format!("lv2ui_descriptor did not list {}", want.unwrap_or(path)));
    }
    let (show_iface, idle_iface) = unsafe { show_idle_ifaces(descriptor) };
    if show_iface.is_none() {
        return Err("ui exports no ui:showInterface".into());
    }

    let mut urid_inner = Box::new(UridMapInner { uris: Mutex::new(Vec::new()) });
    let mut map = Box::new(Lv2UridMap {
        handle: (&mut *urid_inner) as *mut UridMapInner as *mut c_void,
        map: Some(map_cb),
    });
    let float_urid = unsafe { map_cb(map.handle, ATOM_FLOAT_URI.as_ptr()) };
    let sr_key = unsafe { map_cb(map.handle, SAMPLE_RATE_URI.as_ptr()) };
    let sample_rate = Box::new(sample_rate);
    let mut options = Box::new([
        Lv2Option {
            context: 0,
            subject: 0,
            key: sr_key,
            size: 4,
            type_: float_urid,
            value: (&*sample_rate as *const f32).cast(),
        },
        Lv2Option {
            context: 0,
            subject: 0,
            key: 0,
            size: 0,
            type_: 0,
            value: ptr::null(),
        },
    ]);

    let mut features = Box::new([
        Lv2Feature { uri: MAP_URI.as_ptr(), data: (&mut *map as *mut Lv2UridMap).cast() },
        Lv2Feature { uri: OPTIONS_URI.as_ptr(), data: options.as_mut_ptr().cast() },
        Lv2Feature { uri: IDLE_URI.as_ptr(), data: ptr::null_mut() },
        Lv2Feature {
            uri: INSTANCE_ACCESS_URI.as_ptr(),
            data: plugin_handle,
        },
        Lv2Feature { uri: ptr::null(), data: ptr::null_mut() },
    ]);
    let feature_ptrs = Box::new([
        &features[0] as *const Lv2Feature,
        &features[1] as *const Lv2Feature,
        &features[2] as *const Lv2Feature,
        &features[3] as *const Lv2Feature,
        ptr::null(),
    ]);
    let _ = &mut features; // keep the box alive; pointers alias it

    let plugin_uri = CString::new(plugin.uri()).map_err(|_| "plugin uri contains NUL")?;
    let bundle_c = CString::new(bundle).map_err(|_| "bundle path contains NUL")?;
    let mut controller = Box::new(instance_id.to_string());
    let mut widget = ptr::null_mut();
    let instantiate = unsafe { (*descriptor).instantiate }
        .ok_or("ui descriptor has no instantiate")?;
    let handle = unsafe {
        instantiate(
            descriptor,
            plugin_uri.as_ptr(),
            bundle_c.as_ptr(),
            Some(ui_write),
            (&mut *controller as *mut String).cast(),
            &mut widget,
            feature_ptrs.as_ptr(),
        )
    };
    if handle.is_null() {
        return Err(format!("ui instantiate failed for {}", plugin.uri()));
    }

    let mut gui = OpenLv2Gui {
        _lib: lib,
        descriptor,
        show_iface,
        idle_iface,
        handle,
        controller,
        _urid_inner: urid_inner,
        _map: map,
        _sample_rate: sample_rate,
        _options: options,
        _features: features,
        _feature_ptrs: feature_ptrs,
        visible: false,
        seen_idle_ok: false,
        ext_child: None,
        on_top_applied: false,
    };
    gui.show()?;
    Ok(gui)
}


