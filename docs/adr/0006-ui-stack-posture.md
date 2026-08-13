# ADR 0006 — UI stack posture: Tauri now, thin renderer, one texture interface

## Status

**Accepted** (2026-08-13, project owner). The owner's framing, which is
binding: the WebView-based arranger continues, and the frontend must never
grow thick enough that switching becomes a problem if we finally *must* —
and it must be thin anyway to avoid performance problems. Exit insurance
and performance point the same way; the thin-renderer rule serves both.
(Distilled from `docs/CORE-REDESIGN-ROUND-2.md` §9; accepted on the
measured evidence in §10.1 / `benches/ui-probe/`.)

## Context

Dossier 09 is blunt about the Linux webview: the Tauri maintainers no longer
defend WebKitGTK for Linux-serious projects; measured canvas paths have run
at ~5 FPS vs 17–30 in Chromium; the software-rasterized slow path cannot be
feature-detected; no shipping desktop DAW has ever used a WebView for its
whole UI. A dedicated platform pass (2026-08, round-2 §9.4) verified: the 2D
gap is narrowing on a funded cadence (Skia in 2.46 through Cairo removal in
2.54), but **WebGPU does not exist as a WebKitGTK work item** — absent from
every 2025–2026 release note, "nobody is working on it" per the webkit-gtk
list. Tauri has no near-term escape hatch (Verso archived, Servo unfunded,
CEF unreleased). No Rust-native stack has ever shipped a timeline/arranger
at scale (Meadowlark tried iced and Vizia and documented why both failed).

## Decision

- **Tauri now.** The failure modes are performance-shaped, not
  correctness-shaped, they are measurable, and pausing the product for a
  UI-stack rewrite before the core exists is strictly worse (round-2 §9.3).
- **The thin-renderer rule is binding**: no new authoritative state, business
  logic, or time math lands frontend-side. The UI renders pushed state and
  emits ops/gestures (round-2 §9.2; this is ADR 0003's discipline restated).
- **One texture-targeting renderer interface**: all hot rendering (arranger,
  piano roll, meters) goes behind a single interface that targets a
  texture/canvas — **WebGL2 today, WebGPU-capable by design** for other
  platforms/futures — and Svelte keeps chrome only (menus, inspectors,
  dialogs). The migration blueprint is Figma/1Password (swap a renderer
  backend / shell around a UI-agnostic core), not Zed (build a framework)
  (round-2 §9.4).
- **WebGL2 is the ceiling on Linux** and is treated as such: ship the
  WebKitGTK env-var workaround ladder and a startup performance self-test
  regardless (the WebGL context reports success even when
  software-rasterized).
- **Three measurements gated the render architecture — all three now run
  and PASSED** (2026-08-13, `benches/ui-probe/RESULTS.md`, wry 0.55.1 /
  WebKitGTK 2.52.3): `setPointerCapture` past the window edge PASS 3/3
  (moves stream 683 px beyond the window, `pointerup` delivered outside);
  WebGL2 on real hardware with order-of-magnitude throughput margin (10k
  instanced quads 2–5 ms; renderer string is a hardcoded lie — detection
  must be timing-based); keydown→paint-commit p50 22 ms / p95 30 ms,
  frame-scheduling-bound. One real risk found: default DMA-BUF frame
  pacing jitter on a dual-GPU box, mitigated by
  `WEBKIT_DISABLE_DMABUF_RENDERER=1` — hence the runtime pacing
  self-check above (round-2 §9.1, §10.1).

## Consequences

- The exit to a native shell stays affordable: what would move is one
  renderer backend and some chrome, not the product.
- WebKitGTK improvements are upside, never plan-of-record.
- Prototyping the renderer interface once against wgpu (hostable by both
  egui and gpui, the ranked fallback shells) is cheap insurance.
- Any frontend code that grows authoritative state or time math is an
  architecture violation, not a style issue.
