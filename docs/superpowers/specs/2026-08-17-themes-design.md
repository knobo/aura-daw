# Themes — a token contract, eight built-in themes, and user themes from disk

**Status:** design, pending owner approval (2026-08-17).

Rulings R1–R5 in §2 are the owner's, recorded from the brainstorming round.
Everything else is the implementer's, taken under R5.

Supersedes nothing. Adds one frontend subsystem (`src/lib/theme/`), one
backend module (`src-tauri/src/theme.rs`) with two commands, one preference,
and one new preference-schema kind. It changes no existing command, event, or
project schema, and does not alter the default look of the app.

---

## 1. Context

### 1.1 Why this feature exists

AURA's chrome is a committed dark cyberpunk console: a near-black void, glass
panels behind an 18px blur, duotone cyan/magenta accents, and 1px hairlines at
opacities down to 0.045. It is a strong look, and for some people it is
close to unreadable. Buttons sit on translucent panels at low contrast,
state indicators are distinguished by a glow rather than a shape, and the
"dim" and "faint" text ramps land at contrast ratios far below WCAG AA.

The interface-zoom preference already addresses *size*. Nothing addresses
*contrast*. That is what this feature is for. Themes are the shipping
vehicle — Solarized and Nord because people want them, high-contrast because
somebody cannot use the app without it.

### 1.2 What already exists

`src/app.css` declares a token set on `:root` — `--bg-0/1/2`, `--glass`,
`--glass-border`, `--grid-line`, `--cyan`, `--magenta`, `--red`, `--amber`,
`--violet`, `--text`, `--text-dim`, `--text-faint`, plus fonts and chrome
metrics. Components reference them ~610 times. There is no Tailwind colour
utility anywhere in the codebase; every component styles itself with scoped
CSS. That is a good foundation.

It is also incomplete. **433 hardcoded colour literals remain across 41
`.svelte` files** (Timeline 62, TransportBar 32, PianoRoll 31, HumPanel 25,
TrackHeader 21 …). Canvas code sets `fillStyle` from the same literals. Until
those are converted, no theme can work.

The good news is how *few distinct colours* those 433 literals are: **20
unique RGB triples**, and 14 of them are the existing tokens with an alpha
applied. The sweep is mechanical, not creative. §8 is the full inventory.

### 1.3 Prior art in this repo to follow

- `src/lib/prefs/schema.ts` — preferences are declared once as pure data;
  the store gets persistence and the dialog renders the control from the
  declaration. A theme picker should be one entry, not a bespoke dialog.
- `src/lib/utils/prefs.ts` — the stated philosophy for untrusted disk state:
  every failure mode degrades to "no preference", never to a crash.
- `src-tauri/src/midi_out/persist.rs` — per-machine config lives at
  `dirs::config_dir()/aura/<file>.json`, never in the project.
- `src/lib/tauri.ts` — every backend call goes through the `Backend`
  interface, so the plain-browser demo stays live.

---

## 2. Owner's rulings

**R1 — A theme owns colours *and* affordances.** Not colours alone. A theme
may also set border thickness, focus-ring thickness, glass blur, and glow
radius, so a high-contrast theme can remove the frosted glass and thicken
every edge rather than merely recolouring them. A theme may **not** set fonts
or font sizes; that would duplicate the existing interface-zoom preference.

**R2 — User themes are JSON files in a scanned directory.** Not a raw CSS
override file. `dirs::config_dir()/aura/themes/*.json`, one file per theme,
each declaring `name`, an optional `extends`, and a `tokens` map, so a user
overrides only what they care about. Files are read at startup; editing one
requires an app restart. Invalid input degrades, it does not crash.

**R3 — The full sweep lands in this PR.** All 433 literals across all 41
components, canvas drawing included. A half-converted app would show a dark
timeline inside a light shell, which fails the people this feature is for.

**R4 — Eight built-in themes.** AURA Dark (the current look, unchanged, and
the default), AURA Light, Solarized Dark, Solarized Light, High Contrast
Dark, High Contrast Light, Nord, Gruvbox Dark.

**R5 — TS-first architecture.** Themes are TypeScript objects. The store
writes CSS custom properties to `document.documentElement.style` and exposes
the same values as reactive state for canvas code. Built-in themes and user
themes travel one code path, not two.

---

## 3. The token contract

### 3.1 Shape

`ThemeTokens` is a flat, fully-populated record. Every theme must define every
key — enforced by the type for built-ins, and by a runtime test that guards
against a key added to the interface but forgotten in a theme file. User
themes are always merged over a complete base, so they can never be partial
at the point of use.

**Surfaces** (opaque, a six-step ramp from deepest to most raised, plus the
base colour the translucent panel fill is mixed from):
`bgVoid`, `bg0`, `bgSunken`, `bg1`, `bg2`, `bg3`, `glass`

**Lines** (base colours, always consumed with an alpha):
`line` — hairlines, grids, rulers; `edge` — panel borders, glass edges

**Accents:** `cyan`, `cyanBright`, `cyanDeep`, `magenta`, `amber`,
`amberSunken`, `red`, `redSoft`, `violet`, `green`, `orange`

**Text:** `text`, `textMid`, `textDim`, `textFaint`, `textOnAccent`

**Shadow:** `shadow` — the base for drop shadows and inner glows

**Track palette:** `trackPalette` — exactly six colours, the clip and track
identity ramp. A light theme needs its own; the dark palette is invisible on
a light lane.

**Affordances** (R1), five scalars: `borderWidth`, `focusWidth`, `glassBlur`
and `glowBlur` are CSS lengths; `bodyGlow` is a unitless alpha in `0..1`.

`glowBlur: 0px` removes every glow with no CSS special-casing — a `box-shadow`
with zero offset, zero blur, and zero spread renders nothing. `bodyGlow` is
the alpha of the two radial washes on `body`; at `0` the background is flat.

### 3.2 Emission

Each colour token is emitted **twice**:

```
--cyan: #52e5ff;
--cyan-rgb: 82 229 255;
```

Components needing an alpha variant write `rgb(var(--cyan-rgb) / 0.4)`.

This is deliberate, and it is the alternative to CSS relative colour syntax
(`rgb(from var(--cyan) r g b / 0.4)`). Relative colour syntax is newer than
the WebKitGTK baseline we can assume on Linux; the space-separated triple
works in every webview AURA runs in.

Affordance tokens emit once, as-is: `--border-width: 2px`.

Derived convenience tokens that `app.css` already exposes — `--grid-line`,
`--grid-line-strong`, `--glass-border`, `--glass`, `--cyan-dim`,
`--magenta-dim` — are emitted too, computed from the base tokens and their
established alphas, so existing `var()` call sites keep working unchanged.

### 3.3 The pre-JS baseline

`app.css` keeps AURA Dark's values on `:root`. The first paint is therefore
correct before any JavaScript runs, and the store's write is a no-op for the
default theme. §6.3 covers the case where the persisted theme is *not* the
default.

---

## 4. Module layout and data flow

```
src/lib/theme/
  tokens.ts          ThemeTokens, TOKEN_KEYS, toCssVars(), alpha()
  builtins/
    aura-dark.ts     … one file per theme (8)
    index.ts         BUILTIN_THEMES registry, ordered for the picker
  parse.ts           untrusted JSON → Theme | ThemeParseError[]
  theme.svelte.ts    the store
  tokens.test.ts
  parse.test.ts
  theme.test.ts
  contrast.test.ts
```

### 4.1 The store

`theme.svelte.ts` holds:

- `register` — built-ins, then user themes once they arrive from the backend
- `activeId` — mirrors the `theme` preference
- `tokens` — `$state<ThemeTokens>`, the resolved token set

`apply(id)` resolves `extends` (built-ins only as bases; a cycle or a missing
base falls back to `aura-dark` and reports), merges, assigns `tokens`, and
writes the CSS custom properties onto `document.documentElement.style`.

An unknown `activeId` falls back to the default rather than leaving the app
unstyled — the same "degrade, never crash" contract `coercePref` follows.

### 4.2 Canvas

Canvas drawing lives inside `$effect`s in `Timeline`, `PianoRoll`, `Meter`,
`ModulationLaneView`, `ClipEnvelopeLane`, `AutomationTrackRow` and
`MidiClipView`. Those effects read `theme.tokens.line` (etc.) directly, so a
theme change re-runs them and repaints with no extra plumbing, no
`getComputedStyle`, and nothing to invalidate by hand.

`src/lib/render/canvas2d.ts` and `webgpu-painter.ts` already take their
colours as parameters and hold no literals of their own. They need no change:
their callers are the components above, which will pass token values.

`alpha(token, a)` in `tokens.ts` turns a token value into an `rgb(r g b / a)`
string for `fillStyle`. The two existing `getComputedStyle(...)
.getPropertyValue("--font-mono")` calls in Timeline and PianoRoll stay as
they are; fonts are outside the theme contract (R1).

---

## 5. User themes

### 5.1 File format

```json
{
  "name": "My theme",
  "extends": "solarized-dark",
  "tokens": {
    "cyan": "#268bd2",
    "text": "#fdf6e3",
    "borderWidth": "2px",
    "glassBlur": "0px"
  }
}
```

`name` is required and non-empty. `extends` is optional and defaults to
`aura-dark`; it must name a **built-in** — user themes cannot extend each
other, which removes cycle-resolution across untrusted files entirely.
`tokens` is optional; an empty or missing map yields a copy of the base.

The theme's id is derived from the filename stem (`my-theme.json` →
`my-theme`), so two files can share a `name` without colliding. A user file
whose stem collides with a built-in id is rejected with a named error rather
than shadowing the built-in.

### 5.2 Validation

`parse.ts` validates untrusted input and **never throws**:

- Non-object, or missing/blank `name` → the whole file is rejected.
- `extends` naming an unknown built-in → rejected.
- An unknown key inside `tokens` → that key is dropped; the theme still loads.
- A malformed value → that key is dropped; the theme still loads.
  Colours accept `#rgb`, `#rrggbb`, `#rrggbbaa`, and `rgb()`/`rgba()`.
  Affordances accept a CSS length (`px`/`rem`/`em`, or bare `0`).
  `trackPalette` must be an array of exactly six valid colours.

Rejections and dropped keys are collected and surfaced as one toast per file,
naming the file and the reason, using the existing `Toasts` component. A bad
theme file is a thing the user can fix; silence would leave them guessing.

### 5.3 Backend

`src-tauri/src/theme.rs`:

- `list_user_themes() -> Vec<UserThemeFile>` — scans
  `dirs::config_dir()/aura/themes/`, reads every `*.json` (non-recursive),
  and returns `{ id, raw }` where `raw` is the **unparsed** JSON text. A
  missing directory yields an empty list, not an error. Unreadable files are
  skipped with a log line.
- `write_user_theme(id, json) -> String` — writes `<themes>/<id>.json`,
  creating the directory, and returns the absolute path. Backs the EXPORT
  button in §6.2. `id` is sanitised to `[a-z0-9-]` and must not be empty;
  the command refuses to write outside the themes directory.
- `user_themes_dir() -> String` — the absolute path, shown in the dialog.

Validation stays entirely on the frontend, in one function, because the
frontend must validate anyway (`utils/prefs.ts` philosophy) and duplicating
the rules in Rust would let them drift.

`DemoBackend` returns an empty list, the current theme's own JSON path as a
stub, and rejects writes — the browser demo keeps its eight built-ins.

---

## 6. Preferences integration

### 6.1 A new schema kind

Existing `enum` defs carry a static `options` array. The theme list is not
static: user files are discovered at runtime. Rather than smuggle mutable
state into a module that is deliberately pure data, add a fourth kind:

```ts
export type ChoiceDef = BaseDef & {
  kind: "choice";
  default: string;
  /** Named runtime catalog the dialog resolves options from. */
  catalog: "themes";
};
```

The schema still holds only data — a string tag. The dialog resolves
`catalog` to a live option list. `coercePref` accepts any non-empty string
for `choice`; existence is checked at apply time, where the fallback lives
(§4.1). This keeps `schema.ts` free of reactive imports and import cycles,
which its header comment calls out as a deliberate property.

One type-level detail, because it is easy to get wrong: `DefFor<V>` currently
routes every `string` value to `EnumDef<V>`. A theme id is the *widest*
string, not a literal union, so the new arm must test for exactly that —
`string extends V ? ChoiceDef : EnumDef<V>` — placed before the existing
bare-string arm. Literal-union preferences like `countInBars` keep resolving
to `EnumDef`; only a plain `string` resolves to `ChoiceDef`.

New preference:

```ts
theme: string;   // category "interface", default "aura-dark"
```

### 6.2 The control

`PreferencesDialog` renders `choice` as a `<select>` with two `<optgroup>`s,
BUILT-IN and CUSTOM. The existing segmented-button treatment for `enum` does
not scale past four options, and eight-plus is the normal case here.

Below the control: the themes directory path as selectable text, and an
**EXPORT CURRENT THEME…** button. The button serialises the resolved active
theme to the file format in §5.1 — `extends` set to the active theme's
built-in base (itself, when a built-in is active) and the full resolved token
map inlined — and writes it via `write_user_theme` under a non-colliding id,
then
toasts the path. Without it, a user's only route to a custom theme is finding
the format in the docs; with it, the starting point is one click away.

The dialog does not need a live-reload affordance: switching themes is
instant, and *editing a file* requires a restart by R2. The blurb says so.

### 6.3 Boot order

`prefs.init()` is synchronous from localStorage; user themes arrive
asynchronously from the backend. So:

1. `main.ts` applies the persisted theme id synchronously. If it names a
   built-in — the common case — the app paints correct on the first frame.
2. If it names a user theme, the store applies a **cached** token blob
   persisted under `aura.theme.cache` on every successful apply. Still
   synchronous, still first-frame-correct.
3. When `list_user_themes()` resolves, the register is populated and the
   active theme re-applied from the real file, refreshing the cache.

Without step 2, a user on a light custom theme gets a dark flash on every
launch — precisely the population this feature exists for. The cache is
disposable: a miss simply falls through to the default for one frame.

---

## 7. The sweep and the guard

All 433 literals convert via the §8 mapping. Two forms:

- CSS: `rgba(82, 229, 255, 0.4)` → `rgb(var(--cyan-rgb) / 0.4)`;
  `#5cf2b8` → `var(--green)`.
- Canvas: `ctx.fillStyle = "rgba(96,130,190,0.4)"` →
  `ctx.fillStyle = alpha(theme.tokens.line, 0.4)`.

Opacities are preserved exactly. AURA Dark's token values are exactly today's
values, so **the default look is pixel-identical after the sweep**. That is
the review criterion for this part of the PR.

A vitest globs `src/lib/components/**/*.svelte` and fails on any colour
literal, with a documented `/* theme-exempt: <reason> */` comment as the
escape hatch for the few places a fixed colour is genuinely right. Without
this test the sweep decays within a month.

---

## 8. Colour inventory (the mapping table)

Every literal in the codebase today, with its count and target token.

| Literal | Count | Token |
|---|---:|---|
| `82,229,255` / `#52e5ff` | 78 | `cyan` |
| `255,200,87` / `#ffc857` | 49 | `amber` |
| `96,130,190` | 39 | `line` |
| `5,7,13` | 38 | `bg0` |
| `255,79,216` / `#ff4fd8` | 33 | `magenta` |
| `92,242,184` / `#5cf2b8` | 48 | `green` |
| `255,65,82` / `#ff4152` | 29 | `red` |
| `157,123,255` / `#9d7bff` | 25 | `violet` |
| `122,160,220` | 16 | `edge` |
| `10,13,23` / `#0a0d17` | 17 | `bg1` |
| `8,10,19` | 13 | `bgSunken` |
| `16,20,42` | 13 | `bg2` |
| `216,227,242` / `#d8e3f2` | 7 | `text` |
| `3,4,8` | 4 | `bgVoid` |
| `0,0,0` | 4 | `shadow` |
| `95,108,133` / `#5f6c85` | 3 | `textDim` |
| `255,255,255` / `#fff` | 4 | `textOnAccent` |
| `27,35,64` / `#1b2340` | 2 | `bg3` |
| `255,120,130` / `#ff8b96` | 2 | `redSoft` |
| `13,17,30` | 1 | `glass` |
| `#39435c` | 2 | `textFaint` |
| `#ff8b5c` | 2 | `orange` |
| `#8fa3c4` | 1 | `textMid` |
| `#8ef0ff` | 1 | `cyanBright` |
| `#1e7f95` | 1 | `cyanDeep` |
| `#1a1408` | 1 | `amberSunken` |

`trackPalette` is the existing `TRACK_PALETTE` in `Timeline.svelte`:
`#52e5ff`, `#ff4fd8`, `#ffc857`, `#9d7bff`, `#5cf2b8`, `#ff8b5c`.

---

## 9. The high-contrast themes

These are the reason the feature exists, so their values are specified rather
than left to taste. Both set:

```
borderWidth: 2px    focusWidth:  3px
glassBlur:   0px    glowBlur:    0px    bodyGlow: 0
```

`glass` becomes fully opaque (equal to `bg1`), `line` and `edge` become the
text colour so hairlines read at their stated opacities, and the text ramp
compresses: `textDim` and `textFaint` sit close enough to `text` that no
label falls below AA. The track palette is re-picked for contrast against
the lane background rather than for the duotone look.

Target: `text` on `bg1` at **≥ 7:1**, every accent on its own surface at
**≥ 4.5:1**. §10 tests it rather than trusting it.

---

## 10. Testing

- **`parse.test.ts`** — non-object input; missing and blank `name`; unknown
  `extends`; unknown token key dropped while the theme survives; malformed
  colour dropped; `trackPalette` of the wrong length rejected; id derived
  from filename; collision with a built-in id rejected.
- **`tokens.test.ts`** — `toCssVars` emits both `--x` and `--x-rgb` for every
  colour, affordances emit verbatim, derived tokens (`--grid-line`,
  `--glass-border`, …) are present; `alpha()` handles `#rgb`, `#rrggbb`,
  `#rrggbbaa`, and `rgb()` input.
- **`theme.test.ts`** — every built-in defines every key in `TOKEN_KEYS` (the
  runtime half of §3.1); `apply` on an unknown id falls back to the default;
  merge precedence is override-over-base; the cache round-trips.
- **`contrast.test.ts`** — WCAG relative-luminance ratio for `text` on `bg1`
  ≥ 4.5 in all eight themes, ≥ 7 in the two high-contrast themes; each of the
  six `trackPalette` entries ≥ 3:1 against `bg1`. This test is the feature's
  actual acceptance criterion.
- **Sweep guard** — no colour literal in `src/lib/components/**/*.svelte`
  outside a `theme-exempt` comment.
- **`prefs` tests** — extend `schema.test.ts` for the `choice` kind:
  coercion accepts non-empty strings, rejects empty and non-strings.
- **Rust** — `theme.rs` scan against a tempdir: a valid file is returned raw,
  malformed JSON is still returned raw (the frontend judges it), a non-`.json`
  file is ignored, a missing directory yields an empty list; `write_user_theme`
  sanitises the id and refuses a path-traversal attempt.

---

## 11. Out of scope

A theme editor in the UI. Live file-watching (R2 fixes restart-to-reload).
Per-track colour override UI. Theming for sidecar windows. Fonts and font
sizes (R1). Per-theme waveform or spectrogram colour maps beyond the tokens
already listed.

---

## 12. Risks

**The sweep is large.** 433 edits across 41 files is the bulk of the diff.
Mitigated by the mapping table being closed and mechanical, and by the
"AURA Dark is pixel-identical" review criterion — any visual change in the
default theme is a sweep bug.

**Canvas reactivity.** Making `$effect`s depend on `theme.tokens` adds a
dependency to hot drawing paths. The tokens object is replaced wholesale on
theme change and never mutated in place, so effects re-run on theme change
and at no other time. The playhead rAF loop reads its colours once per
effect run, not per frame.

**Light themes on a dark-tuned layout.** Several components lean on the dark
background for depth (inset shadows, low-alpha washes). Some will need an
affordance token rather than a colour swap to look right in light themes.
Budgeted as part of the sweep, not discovered after it.
