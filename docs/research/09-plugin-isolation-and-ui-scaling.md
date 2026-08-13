# AURA — Research: Plugin Isolation and UI Scaling

Two adjacent problems that share one property: both are decided by
architecture we have not written yet, and both are ruinous to retrofit.

> **Status: research dossier, 2026-08-13.** Owned by the architecture
> round. Not normative — nothing here binds code until it is promoted into
> `docs/ARCHITECTURE.md` or `docs/SCALABILITY.md`. Companion to the debt
> register (D-11, out-of-process plugin hosting, open half) and to
> ARCHITECTURE §10.11–13 (frontend constraints).

---

## Why this document exists

AURA hosts CLAP and LV2 plugins **in-process** today. `docs/SCALABILITY.md`
D-11 records the scan half as PAID (the sacrificial `AURA_SCAN_WORKER=1`
subprocess) and the runtime half as OPEN. `docs/synth-compatibility.md`
gives the empirical case: padthv1 and Yoshimi render correctly in an
isolated probe and corrupt the heap or SIGSEGV when co-hosted. Six of eight
testable synths survive; the two that don't fail *because* they share our
address space.

Separately, AURA's entire UI is a WebView. That was a reasonable prototype
decision and it is now a strategic exposure, because the Tauri maintainers
have gone on record advising against the stack for Linux-serious projects
(§7.1), and because no shipping desktop DAW has ever used a WebView for its
whole UI (§7.5) — so there is no prior art to inherit and no benchmark to
compare against.

The two topics are in this one document because they **collide**: the most
attractive way to show a plugin's own GUI inside a WebView-based host is to
composite a native window over the WebView, and §4 concludes that doing so
destroys the crash-isolation property that motivated the sandbox in the
first place.

### Provenance and completeness

Research was performed by a delegated agent against primary sources —
vendor documentation, specification text, and source code fetched directly.

> **Read this before citing anything below.** The agent's base
> recommendation for both topics arrived as an **addendum** to a report
> whose first half was not delivered. What follows is therefore complete
> and quotable for every claim that carries a marker, but the connective
> reasoning behind the two headline recommendations (§5, §9) is
> **summarised rather than reconstructed in full**. Where a section is thin
> because of this, it says so. Do not read absence of detail as absence of
> evidence, and do not read presence of a heading as presence of a verified
> claim — check the marker.

Marker key, used throughout:

| Marker | Meaning |
|---|---|
| **[F]** | Verified this session by fetching the cited vendor/spec document |
| **[C]** | Verified by reading the cited source code |
| **[D]** | Developer or practitioner statement (forum, issue, gist) — attributable but not vendor documentation |
| **[J]** | Engineering judgment. Not sourced. |
| **[U]** | Could not verify. Do not repeat as fact. |

---

# PART A — Out-of-process plugin hosting

## 1. How the field isolates plugins

### 1.1 Bitwig — five modes, and the grouping is the interesting part

Bitwig exposes isolation granularity as a user setting, and the wording is
worth reading because the modes are a *scheduling* decision dressed as a
preference **[F]**
([user guide](https://www.bitwig.com/userguide/latest/vst_plug-in_handling_and_options/)):

| Mode | Verbatim |
|---|---|
| **Within Bitwig** | "hosts plug-ins **along with Bitwig Studio's audio engine** … one plug-in crashing would also crash the audio engine" |
| **Together** (default) | "hosts all plug-ins, well, together but does it **separately from the audio engine**" |
| **By manufacturer** | "groups based on their manufacturer. This can be particularly useful when a software creator intends for their various plug-ins to communicate with one another" |
| **By plug-in** | "hosts **each instance of the same plug-in together**" |
| **Individually** | "hosts **every plug-in instance by itself** … This will require more computing resources, but that is the trade-off" |

The modes are "progressive with those on the left potentially using less RAM
and those toward the right offering greater safety", with a per-plugin
override list and non-retroactive semantics ("only new plug-ins added will
follow the updated setting") **[F]**.

Crash behaviour, verbatim **[F]**: "**In many cases, a plug-in crash will
happen discreetly, allowing audio to continue playback seamlessly**"; the
device UI is replaced by a **Reload Plug-in** / **Reload All Plug-ins**
notification.

The overall process story **[F]**
([Modern Foundations](https://www.bitwig.com/modern-foundations/)):
"Bitwig's architecture is unusual among DAWs, keeping its **application,
audio engine, and plug-ins in separate** [processes] … If your **audio
engine crashes, it doesn't bring down the program** … **Projects will load
immediately, with plug-ins following after** … [you can] **open multiple
projects at once**."

Note the discrepancy: that marketing page says "threads"; the user guide and
the support page say "In Bitwig Studio, **plug-ins run in a separate
process**"
([support](https://www.bitwig.com/support/technical_support/what-is-plug-in-crash-protection-26/)).
Trust "processes" **[F]**.

**The transport is pooled shared memory, not per-instance.** Bitwig 2.5
release notes **[F]**
([2.5](https://downloads.bitwig.com/stable/2.5/Release-Notes-2.5.html)):
"Reduce the OS resources we allocate for each plug-in (**we no longer
allocate a new shared memory area for each plug-in**)."

**[J]** That single line explains why *By manufacturer* and *By plug-in* are
cheaper than *Individually*: N instances inside one host process share one
mapped region and one wake/sleep synchronisation. The mode list is a
partitioning policy exposed to the user.

Three payoffs that are **not** crash protection, and each would justify the
work alone **[F]** ([4.0](https://downloads.bitwig.com/stable/4.0/Release-Notes-4.0.html)):

1. Native bit-bridging for 32-bit and 64-bit plug-ins on Windows and Linux —
   "no third-party bridging necessary".
2. Cross-ISA hosting on Apple Silicon: "**Intel VSTs can still be used
   alongside ARM VSTs**".
3. Asynchronous project load: "Projects will load immediately, with plug-ins
   following after."

### 1.2 Ardour — the argument for *not* doing it

Paul Davis's position paper is the strongest published objection **[F]**
([ardour.org/plugins-in-process.html](https://ardour.org/plugins-in-process.html)):

> "the fixed cost of a context switch is on the order of **3usec** … real
> world costs per context switch for audio processing code are between
> **10usec and 300usec** … Let's assume that our real world average is
> **30 usec**."

For 128 tracks × 3 plugins: "**256 to 768 context switches per block**" →
"anywhere from **7.7msec to 23msec** spent doing nothing but context
switches!" — against a 1.3 ms budget at 48 kHz / 64 samples. His conclusion:
"We would need buffer sizes of about **700 - 2000 samples, or roughly 14-40
msec**", and "It may work for 4 track Bitwig session with 12 plugins or
thereabouts but it's not suitable for any large scale work."

### 1.3 The open-source bridges

`yabridge` measures **~5–15 % overhead per instance** in circulation, but
that figure measures **Wine bridging** — a Windows plugin running under Wine
and talking to a Linux host — which is a different and much heavier thing
than same-ABI process isolation. Do not cite it as the cost of sandboxing
**[J]**.

`AudioGridder` is the network-transparent case and is examined in §4.6.

### 1.4 REAPER — gap

REAPER's bridging architecture (32/64-bit bridging, "run as separate
process" per-plugin flags, and the dedicated bridge/firewall modes) was in
the research brief but **the returned addendum does not cover it**. **[U]**
Treat REAPER as an unexamined data point. Its per-plugin "run in separate
process / dedicated process" UI is widely known to exist and would be worth
a targeted pass, because REAPER is the only mainstream DAW that exposes
isolation per instance *and* is famously conservative about CPU.

## 2. The cost argument, and its resolution

Bique — CLAP's author, a Bitwig developer — answers Davis directly, in a
design gist **[D]**
([gist](https://gist.github.com/abique/4c1b9b40f3413f0df1591d2a7c760db4)):

> "**Reducing IPC overhead** — The overhead comes from **context switching
> between processes**, and the smaller the latency is … the higher the
> overhead becomes. The solution is to **bulk process**. … We could create a
> **single plugin process for multiple plugins from the same vendor**. That
> way, the host can **group the processing requests toward multiple plugins
> into one single bulk request, reducing the amount of context switch**."

### 2.1 The synthesis — and it is not a plugin-layer conclusion

**[J]** Davis's arithmetic is correct *per round trip*. Bique's answer does
not dispute the per-round-trip cost; it makes **the number of round trips
independent of the number of plugins**. 768 context switches per block
becomes 4, if four processes each receive one bulk dispatch.

That is only possible if the **graph partitioner** can hand an entire
partition to one process in a single dispatch. And Davis's own objection to
the bulk case names the exact constraint: it "requires that the DAW has no
reason to access the data from each plugin before running the next plugin in
the chain". So a partition must be a **maximal run of plugin nodes with no
host-side work interleaved**.

> **The load-bearing conclusion of Part A: the quality of your sandboxing is
> a property of your graph partitioner, not of your plugin adapter.** It is
> decided when you design the graph compiler, and it cannot be recovered
> later by writing a better bridge.

This is why D-11's open half is not a plugin-module task. It is a
constraint on the schedule compiler that SCALABILITY §1 has not yet been
written to satisfy.

### 2.2 What nobody has published

**No vendor has published latency or CPU-overhead numbers for out-of-process
hosting.** **[F]** The only figures in circulation are Davis's adversarial
model (§1.2) and yabridge's Wine-bridging number (§1.3). If AURA ships this,
publishing real numbers is itself a differentiator — see the same argument
about benchmarks in the Zrythm analysis.

## 3. What crosses the boundary

**[J]** The seam that follows from §2.1:

- **Per block, per partition:** one dispatch carrying the partition's input
  buffers (in shared memory, written in place), the event lists, and the
  transport struct; one wake; one completion signal. Not one message per
  plugin.
- **Never per block:** parameter *descriptors*, state blobs, GUI traffic,
  and anything that would make the dispatch variable-size.
- **Stays host-side:** the schedule, PDC delay lines, the mix graph, and all
  buffer ownership. The sandbox process renders into buffers it does not
  own the lifetime of.
- **Out of band:** state save/load, parameter list enumeration, and GUI
  window handles, over an ordinary request/response channel that may block.

CLAP is markedly friendlier to this than VST3, for reasons the spec itself
makes explicit: every function is annotated with its thread context
(`[main-thread]`, `[audio-thread & active & processing]`), so the split
between the RT dispatch and the out-of-band channel is *already drawn by the
ABI* rather than being something the host has to infer.

## 4. The plugin GUI across a process boundary

This is the hard half, and the honest answer is worse than the ecosystem
folklore suggests.

### 4.1 The native-overlay option is downgraded to **AVOID**

The attractive design — render the plugin's native window as an overlay
composited above the WebView, so the plugin editor appears *inside* our UI —
was rated as one of three viable options in an earlier pass. Three findings
push it to "avoid".

**(a) On Linux it can abort the process, not panic.**
[wry #1808](https://github.com/tauri-apps/wry/issues/1808), open. Someone
inserted a `gtk::Overlay` to composite native content over the WebView —
exactly this plan — and got a process abort **[F]**:

> "`tauri-runtime-wry`'s `undecorated_resizing.rs` … hardcode[s] a fixed
> two-level ancestor assumption … If anything reparents the webview widget
> so its parent's parent is no longer the GtkWindow itself … the downcast
> returns Err and `.unwrap()` panics inside a GTK signal callback. GTK
> signal dispatch can't unwind across the C FFI boundary, so this aborts the
> whole process."
>
> "This reproduced 100% of the time on the first real button-1 click
> reaching the webview."

And the structural obstacle underneath it: Tauri's default Linux widget tree
is `GtkApplicationWindow → GtkBox(vertical) → WebKitWebView` **[C]**
(`tao/src/platform_impl/linux/window.rs:165`). **A `GtkBox` stacks; it does
not overlay.** You would need `GtkFixed`, which is what Tauri switches to
only under the `unstable` multi-webview feature.

**(b) On Windows, cross-process `SetParent` silently rewrites the child
process's DPI awareness.** From the
[`SetParent` documentation](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setparent)
**[F]**:

> "**Unexpected behavior or errors may occur if *hWndNewParent* and
> *hWndChild* are running in different DPI awareness modes.**"

and its own table: `SetParent` **in-process** on Windows 10 1703+ → **Fail
(ERROR_INVALID_STATE)**; **cross-process** → "**Forced reset (of child
window's process)**". The documented mitigation,
[`SetThreadDpiHostingBehavior`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setthreaddpihostingbehavior),
is explicitly scoped **[F]**: "**This is only necessary if your app needs to
host child windows from plugins and third-party components that do not
support per-monitor-aware context**" — and it does not cover per-monitor-aware
children.

**(c) A hung plugin GUI hangs our input.** Cross-thread parent/child
attachment joins the two threads' input queues. Raymond Chen, on
[AttachThreadInput](https://devblogs.microsoft.com/oldnewthing/20130619-00/?p=4043)
**[D]**:

> "Attaching input queues is not a Get Out of Jail Free card. It's a **Get
> Into the Same Jail** card."

Microsoft's own
[windowed-vs-visual hosting page](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/windowed-vs-visual-hosting)
enumerates the failure set from experience, listing as benefits of *not*
using a cross-process child HWND **[F]**:

> "won't potentially cause each other to hang due to **attached window input
> queues**" · "can have **different DPI awareness**" · "can have **different
> integrity levels**" · "changing monitor scale won't potentially cause the
> app to hang"

> **The conclusion that must go into ARCHITECTURE:** the crash-isolation
> story and the embedded-GUI story are **in tension**. The moment we attach
> input queues to embed a plugin editor, the promise "a hung or crashed
> plugin cannot take AURA down" becomes **false** — we have re-coupled the
> two processes through the window manager after carefully decoupling them
> through the scheduler. **[J]**

**Revised recommendation.** A **separate top-level window per plugin editor**
is the default and the only mode we should ship first. Native overlay becomes
a platform-specific, opt-in enhancement behind a feature flag, X11-and-Windows
only, and on Windows it must be paired with matched DPI awareness across both
processes plus a watchdog. **[J]**

### 4.2 The XEmbed focus proxy — steal this even if we never implement XEmbed

The [XEmbed specification, §3.2](https://specifications.freedesktop.org/xembed-spec/latest/rationale.html)
explains the "my plugin window ate all keyboard input" bug and gives a fix
that requires **no cooperation from the plugin** **[F]**:

> "if the mouse pointer is within the embedded window, the outer toolkit
> doesn't see any key events, even if the logical keyboard focus is
> elsewhere"
>
> "the topmost embedder creates a not-visible X Window to hold the focus,
> **the focus proxy**. (It might be a 1x1 child window of toplevel located
> at -1,-1.)"
>
> "**This also makes it possible to use this part of XEmbed with clients
> that do not support the protocol at all**, without breaking keyboard input
> for the embedding application."

**[J]** This is the principled version of the mouse-enter/leave focus
heuristic that yabridge and others hand-roll. It costs one invisible window.

### 4.3 Why JUCE reparents instead of XEmbedding — and the shim-window pattern

`juce_VSTPluginFormat.cpp`, around line 705 **[C]**:

> "We're not XEmbedding the client VST window directly, because **it's not
> clear that VST hosts & clients expect to use the XEmbed protocol**."

What JUCE does instead: it inserts a **stable shim X window** whose lifetime
is tied to the plugin-window component, XEmbeds *that* (JUCE↔JUCE, so both
ends cooperate), and hands its XID to the plugin as a plain parent. **The
plugin never speaks XEmbed.**

The reason is structural, not stylistic: VST2, VST3 and CLAP all say "here
is a parent window handle", and XEmbed requires the **client** to set
`_XEMBED_INFO` — which no plugin does. Note also that JUCE must call
`xMapRaised` on the plugin window on **every resize** **[C]**, i.e. mapping
and stacking of the foreign window do not propagate the way they would for a
cooperating client.

**[J] Adopt the shim-window pattern.** Besides making XEmbed usable at all,
it solves handle-lifetime churn when an editor is shown and hidden
repeatedly: the plugin's parent handle never changes even though our own
widget tree does.

### 4.4 `GtkSocket`/`GtkPlug` do not exist in GTK4

[`GtkSocket`](https://docs.gtk.org/gtk3/class.Socket.html) is documented as
"only available when GTK+ is compiled for the X11 platform", and there is no
`class.Socket.html` under `docs.gtk.org/gtk4` at all **[F]**. wry's move to
gtk4-rs / webkit6 ([wry #1474](https://github.com/tauri-apps/wry/issues/1474))
deletes the GTK-level embedding path entirely.

**Do not build on `GtkSocket`.** **[J]**

### 4.5 What wry already does, and its ceiling

There is working precedent for a native child window inside a Tauri window —
inside our own dependency. `wry/src/webkitgtk/mod.rs:196`,
`create_container_x11_window()` → `XCreateSimpleWindow` + `XMapWindow`,
wrapped as a foreign `GdkWindow` **[C]**. And from `wry/src/lib.rs` on
`build_as_child` **[C]**:

> "**Linux**: This will create the webview as a child window of the `parent`
> window. **Only X11 is supported. This method won't work on Wayland.**"

Good news and bad news in one paragraph: the mechanism exists and is
maintained; it has the same X11-only ceiling as everything else in this
section.

### 4.6 macOS — the asymmetry that kills the "just host the layer" idea

- `CARemoteLayerServer` is **public but Apple-deprecated in prose** **[F]**
  ([docs](https://developer.apple.com/documentation/quartzcore/caremotelayerserver)):
  "**`CARemoteLaterServer` is a legacy class for cross-process rendering.**
  `IOSurfaceCreateMachPort(_:)` and `IOSurfaceCreateXPCObject(_:)` … offer an
  improved way." (Apple's typo, preserved.)
- `CAContext` and `CALayerHost` are **private** — both Apple documentation
  endpoints 404, and the only real-world declaration is Chromium
  **redeclaring them itself** in
  [`ui/base/cocoa/remote_layer_api.h`](https://raw.githubusercontent.com/chromium/chromium/main/ui/base/cocoa/remote_layer_api.h),
  guarded at runtime by `ui::RemoteLayerAPISupported()` **[F]**.

> **So: publishing a layer from another process is public API; *hosting* one
> is private.**

Apple's sanctioned alternative — IOSurface plus a mach port — hands you a
**pixel buffer, not a live layer**. You then own compositing, input
forwarding, hit-testing and tear-free presentation yourself. Chromium's
redeclaration includes `createFencePort`/`setFencePort`, which is precisely
the tearing problem you would inherit **[F]**.

**AUv3's out-of-process UI is not a reusable technique; it is an
entitlement.** `AUViewController` "does conform to the
`NSExtensionRequestHandling` protocol" **[F]**
([docs](https://developer.apple.com/documentation/coreaudiokit/auviewcontroller)),
i.e. it rides the private extension remote-view-controller machinery, which
you can only enter by *being an App Extension*. Also worth knowing, from
`AudioComponent.h` **[C]**: for AUv3, out-of-process "**is the default
behavior**", in-process requires a separate bundle, and the options "are just
requests to the implementation. It may fail and fall back to the default."

### 4.7 AudioGridder — pixel streaming, and what it does not give us

From source **[C]**: `Server/Source/ScreenRecorder.hpp` includes
`libavcodec`, `libavdevice` and `swscale`, with `enum EncoderMode { WEBP,
MJPEG }` and three quality levels; `ScreenWorker.cpp` carries a second JPEG
path with **dirty-rect diffing** (`ImageDiff::getDelta`, full frame once per
second). The server additionally sandboxes each plugin in a child process
(`Sandbox.cpp`, `ProcessorClient.cpp`) — so it isolates **twice**.

Two facts that matter for us **[F]** ([README](https://github.com/apohl79/audiogridder)):

1. **The server runs on macOS and Windows only; only the client plugin runs
   on Linux.** So pixel-streaming as a Wayland escape hatch has **no
   off-the-shelf implementation**.
2. **No latency, frame-rate or bandwidth numbers are published anywhere in
   the repository.**

### 4.8 WebView2 visual hosting — the one first-class answer, and Tauri hides it

`CreateCoreWebView2CompositionController` puts the WebView's output into
**your** `IDCompositionVisual` tree, giving genuine z-order control against
native content. The price, verbatim **[F]**:

> "**no spatial inputs (such as mouse, touch, or pen) are sent to the
> WebView2 control, unless the app manages such input.** … the app is
> responsible for forwarding this spatial input"
>
> "because the WebView2 isn't scaling its own contents, **they're blurry**"
> (addressed via the Rasterization Scale APIs)

Tauri's `PlatformWebview::controller()` returns `ICoreWebView2Controller` —
the **windowed** controller. Getting visual hosting means patching wry
**[C/J]**.

### 4.9 The settled negative result

Across NPAPI windowed plugins, WebView2, Electron
([`BrowserView` z-ordering, #15899](https://github.com/electron/electron/issues/15899),
closed; no native-over-web API) and Apple's remote layers:

> **No shipping product interleaves native content with DOM z-order.** Every
> mechanism is **rectangle-granular**. Either you stack two rectangles, or
> you turn one into a texture. **[F/J]**

Electron's
[offscreen rendering](https://www.electronjs.org/docs/latest/tutorial/offscreen-rendering)
is the dual of the same trade: pull the web surface into a texture you
composite yourself.

**This retires the question.** We are choosing between two rectangles and a
separate window — not designing a compositing strategy.

---

# PART B — Rendering and interaction at DAW scale in a web stack

## 5. What the recommendation is

**[J]** The shape is unchanged from the earlier pass: **Canvas2D as the
baseline with a WebGL2 upgrade path, Perfetto's dual-backend structure, five
compositing layers, and per-lane canvases.** What has changed is the risk
profile: it is materially worse on Linux than previously stated.

> This section is the one most affected by the missing base report (see
> "Provenance"). The five-layer decomposition and the Perfetto comparison
> were argued in the undelivered half. Treat §5 as a **pointer to a
> recommendation**, not as the recommendation's justification.

## 6. Why this is not the same problem as a normal web app

Figma states the specific mismatch better than anyone, and the sentence is
worth carving somewhere permanent **[F]**
([Building a professional design tool on the web](https://www.figma.com/blog/building-a-professional-design-tool-on-the-web/)):

> "**These are usually optimized for scrolling, not zooming, and geometry is
> often re-tessellated after every scale change.**"

That is the DAW timeline problem exactly. **An arranger is a zoom-first
surface, and browser compositing layers are built scroll-first.** **[J]**

Three more from the same source **[F]**:

> "The 2D canvas API is an immediate mode API instead of a retained mode API
> so all geometry has to be re-uploaded to the graphics card every frame.
> This is needlessly wasteful."
>
> "Internally our code looks a lot like a browser inside a browser; we have
> our own DOM, our own compositor, our own text layout engine."
>
> "All C++ objects are just reserved ranges in a pre-allocated typed array
> so **the JavaScript GC is never involved**."

And a correction to a claim that circulates: **Figma's Rust work is
server-side only.** They say so explicitly — "these are server-side
performance improvements, not client-side" **[F]**. Do not cite Figma as
evidence for Rust-in-the-UI.

## 7. The platform floor

### 7.1 The Tauri maintainers will not vouch for Linux

[tauri #14963](https://github.com/tauri-apps/tauri/issues/14963), open, 207
👍. @FabianLars, May 2024 **[D]**:

> "We're aware of the situation and **completely stopped defending webkitgtk
> ourselves.** … Nonetheless, **we do not see a future with webkitgtk.**"
>
> "**using Tauri, or any similar framework using webkitgtk, is not a good
> idea for projects where Linux is a serious target, to a degree where i
> actively warn people about this** (before the never ending
> webkitgtk-nvidia fiasco began i restricted this warning to performance
> intensive frontends but sadly we're past that now)."

On whether GTK4 / WebKitGTK 6.0 helps: "we were told that it's better, but
**the difference seems to be tiny.** 🤏" The WebView2-for-Linux escape hatch
was
[officially discontinued in July 2024](https://github.com/MicrosoftEdge/WebView2Feedback/issues/1314#issuecomment-2211683486);
CEF is the plan and is un-shipped **[F]**.

### 7.2 Measured Linux rendering numbers

| Report | Finding |
|---|---|
| [tauri #5761](https://github.com/tauri-apps/tauri/issues/5761) **[F]** | Canvas animation: **~5 FPS** in Tauri vs **17–30 FPS** for identical code in Brave/Chromium (Ubuntu 22.04, i5-8250U). Reproduced in Epiphany → the fault is WebKitGTK, not Tauri. Closed "nothing we can do." |
| Same issue, maintainer **[D]** | "webkit2gtk published on almost all distros **don't enable WebGL 2.0** and it fallsback to WebGL 1.0" (2022; WebGL2 landed in WebKitGTK 2.40) |
| [tauri #3988](https://github.com/tauri-apps/tauri/issues/3988) **[D]** | ~3000 DOM elements with drag-select: fine in Firefox/Electron/Windows, broken on Linux. Contenteditable: "**paints to take between 70–300 ms, just to enter a single character**… never dropping below 60fps [elsewhere]". "**direct 50% FPS drops**". "the performance was so bad I had to **port it over to Electron instead**." |

### 7.3 You cannot detect the slow path at runtime

[Tauri's own Linux graphics documentation](https://v2.tauri.app/develop/debug/linux-graphics/)
**[F]**:

> "**WebGL and canvas content can silently land on a slow path while the rest
> of the app looks fine.**"

Context creation succeeds even when software-backed, and WebKitGTK masks
renderer strings for fingerprinting protection — so
`WEBGL_debug_renderer_info` will not tell you that you are on llvmpipe. The
documented workaround ladder (`nvidia_drm.modeset=1` →
`__NV_DISABLE_EXPLICIT_SYNC=1` → `WEBKIT_DISABLE_DMABUF_RENDERER=1` →
`WEBKIT_DISABLE_COMPOSITING_MODE=1`) is a support burden we would inherit.

> **Practical consequence [J]: ship a startup micro-benchmark** — draw N
> instanced quads, time it — **and degrade the render path from measured
> throughput, not from feature detection. Feature detection lies here.**

### 7.4 The counterweight: Igalia's Skia work is real, with numbers

[Graphics improvements after the switch to Skia, April 2025](https://blogs.igalia.com/carlosgc/2025/04/21/graphics-improvements-in-webkitgtk-and-wpewebkit-after-the-switch-to-skia/).
MotionMark on a Raspberry Pi 4, July 2024 → April 2025 **[F]**:

| Subtest | Before | After | Change |
|---|---|---|---|
| Paths | 375 | 4,256 | **+1034 %** |
| Canvas arcs | 140 | 828 | +490 % |
| Canvas lines | 1,614 | 3,087 | +91 % |

Plus threaded painting, MSAA ×4, and a real `Damage` class replacing a
32-region cap.

Their own stated remaining limitations, and one of them is directly our
problem **[F]**: damage propagation is **disabled by default**; CPU rendering
still uploads dirty regions on every composite; and "**compositor
synchronization with system vblank and animation sources needs
improvement**."

**[J] That last one is a scrolling-playhead frame-pacing problem, openly
unsolved by the people who own the code.**

One architecture detail worth knowing
([Igalia, 2023](https://blogs.igalia.com/carlosgc/2023/04/03/webkitgtk-accelerated-compositing-rendering/))
**[F]**: on the **GTK3** path that Tauri still uses, "we have to manually
download the buffer to CPU and paint normally using Cairo" — a GPU→CPU→GPU
round trip per frame.

### 7.5 There is no shipping desktop DAW with a WebView UI

Confirmed by absence: **awesome-tauri contains no DAW or music-production
app at all** — its entire Audio & Video section is players, taggers,
downloaders and one MIDI trainer **[F]**.

The closest counterexample is Rekordbox 6, where the **helper agent** is
Electron, not the UI
([teardown](https://web.archive.org/web/2021/https://rekord.cloud/blog/technical-inspection-of-rekordbox-6-and-its-new-internals))
— and even that forced an unrelated migration from DeviceSQL to SQLite,
because the Electron process needed database access **[F]**.

> **We would be first. That is a strategic fact, not a technical one, but it
> belongs in the decision.** **[J]**

## 8. The functional blocker nobody benchmarks

From a JUCE-forum practitioner report on WebView plugin UIs **[D]**:

> "**you cannot actually track the mouse as soon as it leaves the window**
> (some hacks get around this, but none are great)."

**[J] For a DAW timeline this is not cosmetic.** Drag-a-clip-past-the-right-edge-
to-autoscroll and lasso-select-beyond-the-viewport are core gestures, not
polish. This must be verified with `setPointerCapture()` on WebKitGTK and
WebView2 specifically, at the **window** boundary (not the element boundary),
before we commit to WebView-hosted arranger interaction. If drag-past-edge
does not work, timeline editing in a WebView has a **functional hole, not a
performance problem**.

## 9. Prior art — and the negative result

The literature does not exist, and that is itself the finding.

**BandLab, Amped Studio, Soundation, Splice, Output Arcade and Ableton's web
projects have published nothing about UI rendering architecture.** **[F]**
Audiotool has no engineering blog; André Michelle's substantive material is
the openDAW source itself.

The single peer-reviewed exception is Lind & MacPherson, *"Soundtrap: A
collaborative music studio with Web Audio"*, **Web Audio Conference 2017**
([PDF, CC BY 4.0](https://qmro.qmul.ac.uk/xmlui/bitstream/handle/123456789/26162/29.pdf?sequence=1))
**[F]**:

> "auto-detecting basic performance characteristics on startup and during
> studio sessions, and **modifying the Web Audio graph as necessary**"
>
> "**freezing finished tracks** within a project to ease the runtime CPU
> load, doing some **processing server-side** where possible, and using
> **libvorbis through emscripten**"
>
> "challenges encountered around issues like audio latency, streaming, disk
> usage, and greater access to multiple CPU cores… developers are
> **contributing code towards lower audio latencies in the Chromium
> browser**"

> **Note what it does not contain: a single word about UI rendering.** Their
> published pain is entirely audio-side. **You are not missing a body of
> literature.**

### 9.1 The highest-value unwatched source

Julian Storer — the author of JUCE — ADC 2024, *"Javascript, WebViews and
C++ — 'If You Can't Beat Them, Join Them'"*
([abstract](https://conference.audio.dev/javascript-webviews-and-c-if-you-cant-beat-them-join-them-julian-storer-adc-2024/),
[video](https://www.youtube.com/watch?v=NBRO7EdZ4g0)) **[F]**:

> "After 30 years of writing UIs (and UI frameworks) in C++, **I've spent the
> last couple of years migrating to WebViews in several projects.**"
>
> "I'll cover the essential best-practices … lessons learned, gotchas,
> **benchmarks**, top tips, and all the pros and cons…"

**The talk contains benchmarks the research pass could not extract (YouTube
blocked caption retrieval). This is the single highest-value 50 minutes
available for this decision and it should be watched before the render plan
is finalised.** **[J]**

Supporting evidence in the same direction: **JUCE 8 ships WebView UIs as a
first-class feature**
([announcement](https://juce.com/blog/juce-8-feature-overview-webview-uis/))
— and note the platform matrix: macOS→WebKit, Windows→Edge/Chromium,
**Linux→"GTK WebKit2"**. The WebKitGTK problem lands on plugin developers
too, which means it will get attention from a much larger constituency than
ours **[F]**. Cmajor's `PatchWebView` does the same via `choc::ui::WebView`.

Nick Thompson (Elementary Audio) states the split we have already chosen
([HN 30992556](https://news.ycombinator.com/item?id=30992556)) **[D]**: "all
of the actual handling of audio is done natively with high quality realtime
constraints… we're only using JavaScript for … the lightweight virtual
representation of the underlying engine state, and for that role JavaScript
is plenty fast enough."

But the practitioner caveat is exactly our `ops_apply` risk
([JUCE forum](https://forum.juce.com/t/electron-js-app-talking-to-juce-audio-engine/40559),
from someone four years shipped with 180 k downloads) **[D]**: "**you will
always pay a price in terms of synchronicity between webapp and audio
engine.**"

For budgeting: WebView2 ≈ "100 mb for the WebView2 Manager process" plus
35–80 MB per instance **[D]**.

## 10. Latency — no measurement exists, and the proxy has a floor

**There is no published WebView-vs-native pointer or drag latency
measurement, in audio or anywhere.** **[F]**

The best available proxy is Pavel Fatin's
[Typing with Pleasure](https://pavelfatin.com/typing-with-pleasure/) (OS
input event → screen capture) **[F]**:

| Editor | Avg | Max | **Min** |
|---|---|---|---|
| GVim | 0.9 ms | 1.2 | 0.2 |
| Sublime Text | 8.2 ms | 35.2 | 6.2 |
| IDEA (default) | 24.7 ms | 83.7 | 0.1 |
| **Atom (Electron)** | **49.4 ms** | 85.5 | **29.2** |

**The minimum is the interesting number.** Native editors have sub-millisecond
minima and occasional spikes; the Electron one has a hard **29.2 ms floor** —
it never gets fast. And that is Chromium, i.e. the **best-case** WebView.

Two honest caveats: this is Atom in 2015 (VS Code is materially better
today), and it measures a full DOM/layout/paint path that a canvas app
skips.

**[J]** A canvas/WebGL arranger should sit far below this, because it
bypasses DOM layout entirely — **but it cannot bypass the compositor, and on
WebKitGTK the compositor is precisely what is unsynchronised to vblank
(§7.4).**

### 10.1 Methodology warning — instrument to paint

[palette.dev on Notion](https://palette.dev/blog/improving-notion-typing-performance)
**[F]**: Notion found "**typing_lag was almost 10x lower than the true
perceived latency**" until they switched from measuring keypress→React-render
to keypress→browser-paint.

> **Instrument to paint, not to state update — otherwise our own numbers will
> lie by an order of magnitude.**

## 11. The pattern nobody deviates from

Every team that shipped a serious dense web editor wrote the hot path in a
compiled or typed language and cross-compiled **[F]**:

| Product | Hot path |
|---|---|
| **Figma** | C++ → asm.js → wasm |
| **Soundtrap** | Dart → JS (named in the [Dart 1.0 announcement](https://news.ycombinator.com/item?id=6733376)) |
| **Audiotool** | ActionScript → JS via [defrac](https://www.defrac.com/audiotool.sketch/) |

> **Nobody hand-wrote it in JavaScript.** **[J]**

**[J]** This raises a live design question for AURA that was not researched
and should not be answered without measurement: whether the arranger's
**geometry and hit-test layer** should also be Rust — compiled to wasm for
the UI side, sharing types with the engine — rather than TypeScript. That
would let us use interval-tree crates directly and share one `TimeObject`
definition across the boundary. **wasm↔canvas throughput for this shape was
not researched; do not adopt without measuring.**

---

## 12. Revised priority of next actions

Ordered by how much each can invalidate the plan.

1. **Watch Storer's ADC 2024 talk** (§9.1). It has benchmarks, from the
   person who wrote JUCE, on exactly this decision.
2. **Runtime-verify WebGL2 on our actual Linux targets** — and add the
   startup micro-benchmark, because feature detection cannot distinguish GPU
   from llvmpipe (§7.3).
3. **Verify `setPointerCapture()` past the window edge** on WebKitGTK and
   WebView2 (§8). If drag-past-edge does not work, this is a functional hole,
   not a performance one.
4. **Measure keydown/pointer→paint on WebKitGTK with a canvas-only surface**
   (§10). Nobody has published this; we need our own number, and it must be
   measured **to paint**.
5. **Then the plugin seam**, which is comparatively de-risked: CLAP + shared
   memory + futex, **one wakeup per partition** (§2.1), editor in the sandbox
   process in its **own top-level window** (§4.1).

---

## 13. What this means for AURA

Three things follow immediately, and one is a decision already taken.

**The plugin seam is a scheduler decision, not a plugin-module decision.**
D-11's open half cannot be paid inside `src-tauri/src/plugins/`. It requires
the graph compiler to emit **maximal partitions of plugin nodes with no
host-side work interleaved**, so that one process receives one dispatch per
block (§2.1). If the schedule compiler is designed without that constraint,
out-of-process hosting arrives later at Davis's cost rather than Bique's, and
we will conclude — wrongly, and with numbers to back it up — that sandboxing
is impractical.

**Plugin editors get their own top-level windows.** Not composited over the
WebView. The overlay design is refused on three independent grounds (§4.1),
and the deciding one is that it silently converts our crash-isolation
guarantee into a lie by joining the two processes' input queues. The
shim-window pattern (§4.3) and the focus proxy (§4.2) are adopted; `GtkSocket`
is not (§4.4).

**The Linux WebView risk is real, unmeasured, and now scheduled to be
measured.** The project owner has approved running three measurements before
the render architecture is locked:

1. **`setPointerCapture()` past the window edge** — on WebKitGTK and
   WebView2, at the *window* boundary, not the element boundary. This is a
   pass/fail functional gate for WebView-hosted arranger interaction (§8).
2. **WebGL2 on real hardware, with a startup micro-benchmark** — verifying
   availability is not sufficient, because a software-backed context reports
   success and WebKitGTK masks the renderer string. The render path must
   degrade from **measured throughput** (§7.3).
3. **keydown-to-paint latency on a canvas-only surface** — instrumented to
   paint, never to state update (§10.1). No such number has ever been
   published; we will have the first one.

Until those three land, no decision in `docs/SCALABILITY.md` §5 (IPC & UI at
scale) should be treated as settled.
