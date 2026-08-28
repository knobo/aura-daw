//! Keep native plugin editor windows above AURA (INTERFACE pref
//! `pluginGuiOnTop`). CLAP gets `set_transient`; Zyn's ext-gui process
//! gets WM_TRANSIENT_FOR + `_NET_WM_STATE_ABOVE`.
//!
//! `_NET_WM_STATE` must change via an EWMH ClientMessage to the root
//! window. `xprop -set` / `xprop -remove` are ignored by Mutter, which is
//! why toggling the pref used to need a restart (the flag was live, the
//! already-mapped window was not).
//!
//! No extra crates (Cargo.toml is frozen): we dlopen libX11 at runtime
//! (Linux) and shell out to xdotool for window discovery. Missing tools
//! = no-op, never a failed show.
//!
//! Window discovery and restacking are two separate round trips, so an
//! id `xdotool` reported can be gone by the time our request reaches the
//! server. Xlib's DEFAULT error handler calls `exit(1)` for that, which
//! would take a DAW down — with the session's unsaved work — because a
//! plugin editor closed a millisecond early. `x11ewmh::Display` installs
//! its own handler for as long as it lives; see the comment there for
//! the two things that make it delicate.

use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

static ON_TOP: AtomicBool = AtomicBool::new(true);

pub fn set_flag(on: bool) {
    ON_TOP.store(on, Ordering::Relaxed);
}

pub fn set_on_top(on: bool) {
    set_flag(on);
    apply_to_open_plugin_windows(on);
}

pub fn on_top() -> bool {
    ON_TOP.load(Ordering::Relaxed)
}

/// X11 window id of the AURA host window, if we can find it.
pub fn aura_x11_id() -> Option<u64> {
    let pid = std::process::id();
    windows_of_pid(pid)
        .into_iter()
        .find(|wid| {
            let name = window_name(*wid).unwrap_or_default();
            // Plugin titles we set are "AURA — <plugin>"; the host is "AURA".
            name == "AURA" || name.starts_with("AURA") && !name.contains('\u{2014}')
        })
        .or_else(|| windows_of_pid(pid).into_iter().next())
}

/// Raise / pin every top-level window owned by `pid`. Returns true if at
/// least one window was found (so callers can stop retrying).
pub fn apply_on_top_to_pid(pid: u32) -> bool {
    let wids = windows_of_pid(pid);
    if wids.is_empty() {
        return false;
    }
    restack_windows(&wids, on_top());
    true
}

/// Live apply: already-open CLAP editors + Zyn ext-gui windows follow
/// the toggle without a restart.
pub fn apply_to_open_plugin_windows(on: bool) {
    let mut wids = clap_plugin_window_ids();
    for pid in ext_gui_pids() {
        wids.extend(windows_of_pid(pid));
    }
    wids.sort_unstable();
    wids.dedup();
    restack_windows(&wids, on);
}

fn restack_windows(wids: &[u64], on: bool) {
    if wids.is_empty() {
        return;
    }
    let parent = if on { aura_x11_id() } else { None };
    log::info!(
        "wm_stack: {} {} plugin window(s)",
        if on { "pin" } else { "unpin" },
        wids.len()
    );
    #[cfg(target_os = "linux")]
    if let Some(x) = x11ewmh::Display::open() {
        for &wid in wids {
            if on {
                x.pin(wid, parent);
            } else {
                x.unpin(wid);
            }
        }
        x.flush();
        return;
    }
    for &wid in wids {
        if on {
            pin_window_xprop(wid, parent);
        } else {
            unpin_window_xprop(wid);
        }
    }
}

/// CLAP editors live in-process. Prefer the title we set (`AURA — …`);
/// also catch editors that ignored `suggest_title` but are already
/// transient-for the host (so unpin can still find them).
fn clap_plugin_window_ids() -> Vec<u64> {
    let aura = aura_x11_id();
    windows_of_pid(std::process::id())
        .into_iter()
        .filter(|wid| Some(*wid) != aura)
        .filter(|wid| {
            let name = window_name(*wid).unwrap_or_default();
            name.contains('\u{2014}') || aura.is_some_and(|p| is_transient_for(*wid, p))
        })
        .collect()
}

fn ext_gui_pids() -> Vec<u32> {
    let out = Command::new("pgrep").args(["-f", "zynaddsubfx-ext-gui"]).output();
    let Ok(out) = out else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

fn windows_of_pid(pid: u32) -> Vec<u64> {
    let out = Command::new("xdotool")
        .args(["search", "--onlyvisible", "--pid", &pid.to_string()])
        .output();
    let Ok(out) = out else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

fn window_name(wid: u64) -> Option<String> {
    let out = Command::new("xdotool")
        .args(["getwindowname", &wid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn is_transient_for(wid: u64, parent: u64) -> bool {
    let out = Command::new("xprop")
        .args(["-id", &wid.to_string(), "WM_TRANSIENT_FOR"])
        .output();
    let Ok(out) = out else {
        return false;
    };
    let s = String::from_utf8_lossy(&out.stdout);
    s.contains(&format!("0x{parent:x}")) || s.contains(&parent.to_string())
}

fn pin_window_xprop(wid: u64, parent: Option<u64>) {
    let id = wid.to_string();
    let _ = Command::new("xdotool").args(["windowraise", &id]).status();
    if let Some(p) = parent {
        if p != wid {
            let _ = Command::new("xprop")
                .args([
                    "-id",
                    &id,
                    "-f",
                    "WM_TRANSIENT_FOR",
                    "32c",
                    "-set",
                    "WM_TRANSIENT_FOR",
                    &p.to_string(),
                ])
                .status();
        }
    }
}

fn unpin_window_xprop(wid: u64) {
    let id = wid.to_string();
    let _ = Command::new("xprop")
        .args(["-id", &id, "-remove", "WM_TRANSIENT_FOR"])
        .status();
}

/// EWMH `_NET_WM_STATE` add/remove via ClientMessage. Spec values
/// (EWMH 1.5 §5.1.2): 0 = REMOVE, 1 = ADD, 2 = TOGGLE.
const NET_WM_STATE_REMOVE: i64 = 0;
const NET_WM_STATE_ADD: i64 = 1;

#[cfg(target_os = "linux")]
mod x11ewmh {
    use super::{NET_WM_STATE_ADD, NET_WM_STATE_REMOVE};
    use std::ffi::CString;
    use std::os::raw::{c_char, c_int, c_long, c_ulong, c_void};
    use std::ptr;
    use std::sync::atomic::{AtomicPtr, Ordering};
    use std::sync::{Mutex, MutexGuard};

    const RTLD_NOW: c_int = 2;
    const CLIENT_MESSAGE: c_int = 33;
    const SUBSTRUCTURE_NOTIFY: c_long = 1 << 20;
    const SUBSTRUCTURE_REDIRECT: c_long = 1 << 19;
    /// EWMH: source indication 1 = application.
    const SOURCE_APPLICATION: i64 = 1;

    #[link(name = "dl")]
    extern "C" {
        fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }

    #[repr(C)]
    struct XEvent {
        pad: [c_long; 24],
    }

    #[repr(C)]
    struct XClientMessageEvent {
        type_: c_int,
        serial: c_ulong,
        send_event: c_int,
        display: *mut c_void,
        window: c_ulong,
        message_type: c_ulong,
        format: c_int,
        data: [c_long; 5],
    }

    /// Field order is Xlib's, not the one the man page prints: the
    /// resource id comes BEFORE the serial (`/usr/include/X11/Xlib.h`).
    #[repr(C)]
    struct XErrorEvent {
        type_: c_int,
        display: *mut c_void,
        resourceid: c_ulong,
        serial: c_ulong,
        error_code: u8,
        request_code: u8,
        minor_code: u8,
    }

    /// The 17 core error codes (`/usr/include/X11/X.h`). Named here
    /// rather than through `XGetErrorText`, which is Xlib re-entry from
    /// inside an Xlib callback.
    #[cfg(test)]
    pub fn error_name_for_test(code: u8) -> &'static str {
        error_name(code)
    }

    fn error_name(code: u8) -> &'static str {
        const NAMES: [&str; 17] = [
            "BadRequest", "BadValue", "BadWindow", "BadPixmap", "BadAtom",
            "BadCursor", "BadFont", "BadMatch", "BadDrawable", "BadAccess",
            "BadAlloc", "BadColor", "BadGC", "BadIDChoice", "BadName",
            "BadLength", "BadImplementation",
        ];
        NAMES.get(code.wrapping_sub(1) as usize).copied().unwrap_or("extension error")
    }

    /// Only one `Display` at a time: the error handler is process-global,
    /// so nested installs would restore in the wrong order.
    static X11_LOCK: Mutex<()> = Mutex::new(());
    static PREV_HANDLER: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
    static OUR_DPY: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

    /// Swallow errors on OUR connection; hand every other one back to
    /// whoever was handling errors before us (GTK/GDK installs its own,
    /// and it is not ours to override for the rest of the process).
    unsafe extern "C" fn swallow_error(dpy: *mut c_void, err: *mut XErrorEvent) -> c_int {
        if dpy != OUR_DPY.load(Ordering::Acquire) {
            let prev = PREV_HANDLER.load(Ordering::Acquire);
            if !prev.is_null() {
                let f = std::mem::transmute::<*mut c_void, XErrorHandlerFn>(prev);
                return f(dpy, err);
            }
            return 0;
        }
        let e = &*err;
        log::warn!(
            "wm_stack: ignoring X {} on window 0x{:x} (request {}.{}, serial {}) \
             — the window most likely closed under us",
            error_name(e.error_code),
            e.resourceid,
            e.request_code,
            e.minor_code,
            e.serial
        );
        0
    }

    type XOpenDisplayFn = unsafe extern "C" fn(*const c_char) -> *mut c_void;
    type XCloseDisplayFn = unsafe extern "C" fn(*mut c_void) -> c_int;
    type XDefaultScreenFn = unsafe extern "C" fn(*mut c_void) -> c_int;
    type XRootWindowFn = unsafe extern "C" fn(*mut c_void, c_int) -> c_ulong;
    type XInternAtomFn = unsafe extern "C" fn(*mut c_void, *const c_char, c_int) -> c_ulong;
    type XSendEventFn = unsafe extern "C" fn(*mut c_void, c_ulong, c_int, c_long, *mut XEvent) -> c_int;
    type XFlushFn = unsafe extern "C" fn(*mut c_void) -> c_int;
    type XRaiseWindowFn = unsafe extern "C" fn(*mut c_void, c_ulong) -> c_int;
    type XSetTransientForHintFn = unsafe extern "C" fn(*mut c_void, c_ulong, c_ulong) -> c_int;
    type XDeletePropertyFn = unsafe extern "C" fn(*mut c_void, c_ulong, c_ulong) -> c_int;
    type XSyncFn = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
    type XErrorHandlerFn = unsafe extern "C" fn(*mut c_void, *mut XErrorEvent) -> c_int;
    type XSetErrorHandlerFn =
        unsafe extern "C" fn(Option<XErrorHandlerFn>) -> Option<XErrorHandlerFn>;

    pub struct Display {
        dpy: *mut c_void,
        close: XCloseDisplayFn,
        intern: XInternAtomFn,
        send: XSendEventFn,
        flush: XFlushFn,
        raise: XRaiseWindowFn,
        set_transient: XSetTransientForHintFn,
        delete_prop: XDeletePropertyFn,
        sync: XSyncFn,
        set_err: XSetErrorHandlerFn,
        root: c_ulong,
        _lock: MutexGuard<'static, ()>,
    }

    impl Display {
        /// `None` means the caller falls back to the `xdotool`/`xprop`
        /// path, which is the safer one anyway: a stale id kills a child
        /// process there, not us. So every symbol below is required —
        /// including the two the error handler needs.
        pub fn open() -> Option<Self> {
            let lock = X11_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            unsafe {
                let lib = dlopen(c"libX11.so.6".as_ptr(), RTLD_NOW);
                if lib.is_null() {
                    return None;
                }
                let open: XOpenDisplayFn = sym(lib, "XOpenDisplay")?;
                let close: XCloseDisplayFn = sym(lib, "XCloseDisplay")?;
                let default_screen: XDefaultScreenFn = sym(lib, "XDefaultScreen")?;
                let root_window: XRootWindowFn = sym(lib, "XRootWindow")?;
                let intern: XInternAtomFn = sym(lib, "XInternAtom")?;
                let send: XSendEventFn = sym(lib, "XSendEvent")?;
                let flush: XFlushFn = sym(lib, "XFlush")?;
                let raise: XRaiseWindowFn = sym(lib, "XRaiseWindow")?;
                let set_transient: XSetTransientForHintFn = sym(lib, "XSetTransientForHint")?;
                let delete_prop: XDeletePropertyFn = sym(lib, "XDeleteProperty")?;
                let sync: XSyncFn = sym(lib, "XSync")?;
                let set_err: XSetErrorHandlerFn = sym(lib, "XSetErrorHandler")?;
                let dpy = open(ptr::null());
                if dpy.is_null() {
                    return None;
                }
                // Order matters: the handler compares against OUR_DPY, so
                // that has to be readable before the handler can run.
                OUR_DPY.store(dpy, Ordering::Release);
                let prev = set_err(Some(swallow_error));
                // `Option<fn>` is niche-optimised to one pointer, so this is
                // a pointer-to-pointer cast; the atomic is what lets the
                // handler read it without taking the lock it runs under.
                PREV_HANDLER.store(
                    std::mem::transmute::<Option<XErrorHandlerFn>, *mut c_void>(prev),
                    Ordering::Release,
                );
                let root = root_window(dpy, default_screen(dpy));
                Some(Self {
                    dpy,
                    close,
                    intern,
                    send,
                    flush,
                    raise,
                    set_transient,
                    delete_prop,
                    sync,
                    set_err,
                    root,
                    _lock: lock,
                })
            }
        }

        pub fn pin(&self, wid: u64, parent: Option<u64>) {
            if let Some(p) = parent {
                if p != wid {
                    unsafe { (self.set_transient)(self.dpy, wid, p) };
                }
            }
            self.net_wm_state(wid, NET_WM_STATE_ADD);
            unsafe { (self.raise)(self.dpy, wid) };
        }

        pub fn unpin(&self, wid: u64) {
            self.net_wm_state(wid, NET_WM_STATE_REMOVE);
            let atom = self.atom("WM_TRANSIENT_FOR");
            if atom != 0 {
                unsafe { (self.delete_prop)(self.dpy, wid, atom) };
            }
        }

        pub fn flush(&self) {
            unsafe { (self.flush)(self.dpy) };
        }

        fn atom(&self, name: &str) -> c_ulong {
            let Ok(c) = CString::new(name) else {
                return 0;
            };
            unsafe { (self.intern)(self.dpy, c.as_ptr(), 0) }
        }

        fn net_wm_state(&self, wid: u64, action: i64) {
            let state = self.atom("_NET_WM_STATE");
            let above = self.atom("_NET_WM_STATE_ABOVE");
            if state == 0 || above == 0 {
                return;
            }
            let mut ev = unsafe { std::mem::zeroed::<XEvent>() };
            {
                let cm = &mut ev as *mut XEvent as *mut XClientMessageEvent;
                unsafe {
                    (*cm).type_ = CLIENT_MESSAGE;
                    (*cm).serial = 0;
                    (*cm).send_event = 1;
                    (*cm).display = self.dpy;
                    (*cm).window = wid;
                    (*cm).message_type = state;
                    (*cm).format = 32;
                    (*cm).data = [action, above as c_long, 0, SOURCE_APPLICATION, 0];
                }
            }
            let mask = SUBSTRUCTURE_NOTIFY | SUBSTRUCTURE_REDIRECT;
            unsafe { (self.send)(self.dpy, self.root, 0, mask, &mut ev) };
        }
    }

    impl Drop for Display {
        /// X errors are ASYNCHRONOUS, so the sync is the whole point:
        /// restoring the handler first and syncing after leaves the
        /// deferred error to Xlib's default handler, i.e. `exit(1)`.
        /// Measured both ways — see the PR.
        fn drop(&mut self) {
            unsafe {
                (self.sync)(self.dpy, 0);
                // XCloseDisplay syncs again, so it stays inside the guard.
                (self.close)(self.dpy);
                let prev = std::mem::transmute::<*mut c_void, Option<XErrorHandlerFn>>(
                    PREV_HANDLER.load(Ordering::Acquire),
                );
                (self.set_err)(prev);
                OUR_DPY.store(ptr::null_mut(), Ordering::Release);
            }
        }
    }

    unsafe fn sym<T>(lib: *mut c_void, name: &str) -> Option<T> {
        let c = CString::new(name).ok()?;
        let p = dlsym(lib, c.as_ptr());
        if p.is_null() {
            None
        } else {
            Some(std::mem::transmute_copy(&p))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_top_flag_roundtrips() {
        let prev = on_top();
        set_on_top(false);
        assert!(!on_top());
        set_on_top(true);
        assert!(on_top());
        set_on_top(prev);
    }

    #[test]
    fn missing_pid_is_not_found_yet() {
        // A pid that cannot own an X window. Must not panic.
        assert!(!apply_on_top_to_pid(1) || !on_top() || std::env::var_os("DISPLAY").is_none());
    }

    #[test]
    fn ewmh_actions_match_spec() {
        assert_eq!(NET_WM_STATE_REMOVE, 0);
        assert_eq!(NET_WM_STATE_ADD, 1);
    }

    /// The regression this module exists to prevent. `xdotool` finds a
    /// window id, the window closes, we restack it — and Xlib's default
    /// error handler calls `exit(1)`.
    ///
    /// Note how this test fails if it regresses: not with a panic and an
    /// assertion line, but with the whole test binary gone at rc=1 after
    /// printing `X Error of failed request: BadWindow`. That is exactly
    /// what it was doing to the DAW.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_dead_window_id_does_not_take_the_process_with_it() {
        if std::env::var_os("DISPLAY").is_none() {
            return; // no X server; the whole path is a no-op
        }
        // An id no server hands out: high enough to be outside every
        // client's allocated range on a fresh session.
        let dead = [0x00ff_ffffu64];
        restack_windows(&dead, true); // XSetTransientForHint + XRaiseWindow
        restack_windows(&dead, false); // XDeleteProperty
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn core_error_codes_are_named_and_the_rest_do_not_panic() {
        assert_eq!(x11ewmh::error_name_for_test(3), "BadWindow");
        assert_eq!(x11ewmh::error_name_for_test(1), "BadRequest");
        assert_eq!(x11ewmh::error_name_for_test(17), "BadImplementation");
        // 0 is "no error" and >17 belongs to an extension; both must be
        // survivable, because this runs inside the error handler.
        assert_eq!(x11ewmh::error_name_for_test(0), "extension error");
        assert_eq!(x11ewmh::error_name_for_test(200), "extension error");
    }
}
