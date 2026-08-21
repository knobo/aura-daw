# Themes in AURA

AURA features a full, tokenised theme system supporting eleven built-in themes and user-authored JSON themes from disk. Every visual element—from UI chrome, panels, and dialogs to canvas-rendered timeline tracks, piano roll notes, and audio meters—is styled strictly through reactive theme tokens.

A theme controls two independent things. Its **palette** says what colour a
surface is. Its **material** says what that surface is made of — milled
aluminium, moulded plastic, or flat vector — and that is what makes one
palette shippable as both a flat theme and a hardware theme. See §6.

---

## 1. Selecting a Theme

Open **Preferences** (`Ctrl+,` / `Cmd+,` or via the top menu) and navigate to the **Interface** section. The **Theme** dropdown contains two groups:
- **BUILT-IN**: Curated themes shipping with AURA.
- **CUSTOM**: User-created themes loaded from your local themes directory.

Theme switching is instantaneous: tokens update reactively across the entire application without needing to reload or repaint by hand.

---

## 2. Built-in Themes

AURA ships with eight built-in themes:

1. **AURA Dark** (`aura-dark`): The default house dark theme with deep obsidian backgrounds, signature cyan and magenta highlights, subtle glassmorphism, and glow affordances.
2. **AURA Light** (`aura-light`): The house palette adapted for daytime and bright environments, using crisp light surfaces and darkened accent tones that clear WCAG AA contrast.
3. **High Contrast Dark** (`high-contrast-dark`): Purpose-built accessibility theme. Pure black backgrounds (`#000000`), maximum-contrast text and saturated accents clearing WCAG AAA (≥ 7:1 for body text, ≥ 4.5:1 for secondary text), thickened 2px borders, 3px focus rings, with all blur and glow effects disabled and panels fully opaque.
4. **High Contrast Light** (`high-contrast-light`): Pure white ground (`#ffffff`) with deep black text and high-contrast accents, clearing WCAG AAA with disabled blurs and thick affordances.
5. **Solarized Dark** (`solarized-dark`): Ethan Schoonover's classic low-contrast dark palette, carefully tuned for ≥ 4.5:1 readability.
6. **Solarized Light** (`solarized-light`): Warm paper-like light palette with solarized accents and high-legibility text.
7. **Nord** (`nord`): Arctic north-bluish palette built from Polar Night, Snow Storm, Frost, and Aurora hues.
8. **Gruvbox Dark** (`gruvbox-dark`): Retro warm groove palette with earthy backgrounds and pastel accents.
9. **Console Noir** (`console-noir`): The flagship material theme — a machined outboard rack unit in bead-blasted graphite, with an amber lamp as the primary interactive colour instead of a neon glow. Solid panels, deep bevels, heavy grain.
10. **Rack Slate** (`rack-slate`): Cool steel modules on a dark gutter, lit by one orange lamp — the front-panel idiom, where the window is a grid of separate module blocks rather than one flat surface. The theme the `.module` layout language (§7) was tuned against.
11. **Studio Ivory** (`studio-ivory`): The light half of the material pair — cream injection-moulded plastic with softer corners, a satin sheen, and a warm brown shadow rather than a black one.

---

## 3. Themes Directory Paths

User themes are stored as `.json` files in the platform-specific configuration directory:

| Platform | Themes Directory |
|---|---|
| **Linux** | `~/.config/aura/themes/` |
| **macOS** | `~/Library/Application Support/aura/themes/` |
| **Windows** | `%APPDATA%\aura\themes\` |

The exact path on your machine is displayed directly under the Theme picker in the Preferences dialog.

---

## 4. File Format and Worked Example

User themes are standard JSON files. The filename stem (e.g. `midnight-neon.json`) becomes the theme ID (`midnight-neon`).

```json
{
  "name": "Midnight Neon",
  "extends": "aura-dark",
  "tokens": {
    "cyan": "#00f0ff",
    "magenta": "#ff0077",
    "bg0": "#05060a",
    "bg1": "#0b0e17",
    "borderWidth": "2px",
    "glassBlur": "0px",
    "glassAlpha": "1",
    "bevel": "0.9",
    "relief": "0.8",
    "sheen": "0.6",
    "grain": "0.3",
    "ctrlRadius": "3px",
    "panelAlpha": "1"
  }
}
```

The last five keys are the material layer (§6). Because material is
independent of palette, that block alone turns any theme it is pasted into
from a flat one into a hardware one — the colours need not change at all.

### Properties
- `name` *(required, string)*: Human-readable display name shown in the preferences dropdown.
- `extends` *(optional, string)*: The built-in theme base to inherit default values from (e.g. `"aura-dark"`, `"solarized-light"`). Defaults to `"aura-dark"`. User themes cannot extend other user themes.
- `tokens` *(optional, object)*: Key-value map of token overrides. Omitted tokens inherit their values from `extends`.

### Accepted Colour Spellings
Colour tokens take `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`, or a numeric `rgb()`/`rgba()` — comma- or space-separated, with an optional alpha (`rgb(82 229 255)`, `rgb(82 229 255 / 0.4)`, `rgba(82, 229, 255, 0.4)`).

Percentage channels (`rgb(80% 50% 20%)`), `none`, and the CSS named colours are **not** accepted: AURA re-derives each colour into `r g b` triples for canvas drawing and alpha variants, and those forms cannot be read back. A token spelled that way is dropped with a toast naming it, and the base theme's value stands.

---

## 5. Token Reference

### Surface & Background Tokens
| Token | Description |
|---|---|
| `bgVoid` | Deepest background void, backdrop scrims |
| `bg0` | App background ground level |
| `bgSunken` | Sunken containers, transport bar, header wells |
| `bg1` | Standard panel background, dialog surfaces, track lanes |
| `bg2` | Secondary cards, buttons, nested groups |
| `bg3` | Elevated popups, context menus, subtle borders |
| `glass` | Base surface color for translucent glass elements |

### Boundary & Line Tokens
| Token | Description |
|---|---|
| `line` | Hairline dividers, grid subdivisions, ruler ticks |
| `edge` | Prominent borders, container edges, hovered lines |

### Accent Colors
| Token | Description |
|---|---|
| `cyan` | Primary interactive color, playheads, selections, active tabs |
| `cyanBright` | Glow highlights, active badges |
| `cyanDeep` | Recessed cyan fills |
| `magenta` | Secondary accent, record states, pitch indicators |
| `amber` | Warnings, notifications, automation alerts |
| `amberSunken` | Recessed amber wells |
| `red` | Error states, delete buttons, recording pulses |
| `redSoft` | Subtle danger backgrounds |
| `violet` | Plugin accents, auxiliary selections |
| `green` | Success indicators, level meters, play states |
| `orange` | Extra track & modifier highlights |

### Text & Contrast Tokens
| Token | Description |
|---|---|
| `text` | Primary body text and headers |
| `textMid` | Intermediate metadata text |
| `textDim` | Secondary text, inactive tab labels, units |
| `textFaint` | Tertiary text, hotkey hints, faint badges |
| `textOnAccent` | Text rendered on top of filled accent backgrounds |
| `shadow` | Base shadow tint |

### Track Palette
| Token | Description |
|---|---|
| `trackPalette` | Array of exactly 6 hex colors for audio/MIDI track default coloring |

### Affordance Tokens
| Token | Description | Default (AURA Dark) | High Contrast |
|---|---|---|---|
| `borderWidth` | Border stroke width for UI controls | `1px` | `2px` |
| `focusWidth` | Focus ring outline width | `1px` | `3px` |
| `glassBlur` | Backdrop blur radius for frosted surfaces. Modal scrims take a fraction of it, so lowering this thins those too | `18px` | `0px` |
| `glassAlpha` | Opacity of the glass panel fill, `0`–`1` | `0.62` | `1` |
| `glowScale` | Multiplier on every glow radius; `0` removes all glow | `1` | `0` |
| `bodyGlow` | Opacity of body atmospheric radial gradient | `0.05` | `0` |
| `panelAlpha` | Opacity of the CHROME panels — the right dock. Distinct from `glassAlpha`, which governs floating glass | `0.88` | `1` |

`panelAlpha` is separate from `glassAlpha` because the two answer different
questions. A dialog is temporary and *should* let you see what it covers; a
side panel you work in for an hour should not — a translucent one puts the
timeline grid behind its own text. It is also what makes docking possible:
something you can see through reads as floating over the layout rather than
as part of it, which is why the **Side panel** preference (Preferences →
Interface) offers *float over* and *dock beside*. Docking suits an opaque
theme; with a see-through one, floating costs you nothing.

The blur behind the panel is **derived**, not declared: at `panelAlpha: 1`
the engine emits `--panel-blur: 0px` on its own. A `backdrop-filter` behind a
fully opaque surface blurs pixels nobody can see, on the largest always-on
surface in the app — so unlike the `glassBlur`/`glassAlpha` pairing, which a
theme can still get wrong and which a test has to police, this one cannot be
got wrong at all.

The five **material** tokens (`bevel`, `relief`, `sheen`, `grain`,
`ctrlRadius`) validate on this same path but are documented on their own
in §6, since they are tuned as a set rather than one at a time.

`glowScale` is a multiplier rather than a radius because each glow keeps its own designed size — a 22px bloom under a dialog and a 6px rim on a fader thumb are not the same effect — and because a radius animation needs its two ends to stay apart. Set it to `0.5` to halve every glow, or `0` to remove them.

Pair `glassAlpha: "1"` with `glassBlur: "0px"`. A panel that is translucent but unblurred shows the raw timeline grid through its own text, so a theme turning off the frosting wants solid panels; the built-ins all follow this, and a test enforces it.

---

## 6. Material Tokens

The five material tokens describe how a surface catches light. They are
deliberately orthogonal to the palette: changing them restyles every control
in the app without touching a single colour, and setting all four strengths
to `0` gives exactly the flat look AURA had before they existed.

The virtual key light is fixed at top-centre — the angle every real front
panel is photographed under — so a raised face is lit along its top edge and
casts downward, and a recessed well is that same edge inverted.

| Token | Range | Description |
|---|---|---|
| `bevel` | `0`–`1` | Strength of the lit top edge and shadowed bottom edge on a raised face. This is the token that reads as "moulded": it is the edge itself, not the shadow under it. |
| `relief` | `0`–`1` | Depth of the shadow a raised element CASTS on the panel behind it. Separate from `bevel` because a thick-edged button lying flat on a panel and a thin card floating above it are different objects. |
| `sheen` | `0`–`1` | Strength of the specular gradient down a face — the sweep of reflected light that makes a knob cap read as domed rather than as a circle. |
| `grain` | `0`–`1` | Opacity of the micro-texture overlay: the fine speckle of bead-blasted metal or moulded plastic. The single cue that most separates "photograph of hardware" from "rectangle with a gradient". |
| `ctrlRadius` | a length | Corner radius of controls. A length rather than a strength, because hard-edged rack gear and soft-cornered consumer plastic differ here and nowhere else. |

### Built-in material at a glance

| Theme | `bevel` | `relief` | `sheen` | `grain` | `ctrlRadius` |
|---|---|---|---|---|---|
| AURA Dark | 0.4 | 0.55 | 0.3 | 0.1 | 6px |
| AURA Light | 0.35 | 0.4 | 0.35 | 0.08 | 6px |
| Console Noir | 0.95 | 0.9 | 0.7 | 0.38 | 3px |
| Studio Ivory | 0.85 | 0.65 | 0.55 | 0.28 | 9px |
| High Contrast (both) | 0 | 0 | 0 | 0 | 4px |
| Solarized (both) | 0.3 | 0.35–0.4 | 0.25–0.3 | 0.06 | 5px |
| Nord | 0.35 | 0.45 | 0.3 | 0.08 | 6px |
| Gruvbox Dark | 0.45 | 0.5 | 0.28 | 0.16 | 4px |

### Writing CSS against the material

Components never read the five scalars directly. `src/app.css` derives a set
of ready-made composites from them, and that is the only place the "what does
a raised thing look like" decision is made:

| Variable | Use |
|---|---|
| `--bevel-raised` / `--bevel-inset` | The lit/shadowed lips of a face, and the same inverted for a well. |
| `--relief-1` / `--relief-2` / `--relief-3` | Cast shadow at three heights: sitting on, lifted off, floating above. |
| `--sheen-face` / `--sheen-dome` | The specular sweep down a flat face, and across a dome. |
| `--grain-tex` | The tiling speckle texture, applied at `opacity: var(--grain)`. |

So a control writes:

```css
.my-button {
  border-radius: var(--ctrl-radius);
  background-image: var(--sheen-face);
  box-shadow: var(--bevel-raised), var(--relief-2);
}
.my-button:active {
  box-shadow: var(--bevel-inset), var(--relief-1);
  transform: translateY(calc(1px * var(--relief)));
}
```

and inherits every theme's material for free. `app.css` also ships `.raised`,
`.inset` and `.grain` utility classes for markup written against the system
from the start — but note that a Svelte component's scoped selectors outrank
a bare global class, so an existing component opts in through the composite
variables instead.

> **Naming caution.** This project imports Tailwind, which ships bare
> utilities such as `.ring`. A plain `class="ring"` inside a component picks
> that utility up and Svelte's scoping does not prevent it — the element still
> carries the bare class name. Prefix component class names.

### Accessibility

The high-contrast themes zero all four strengths, and a test enforces it. A
bevel is a low-contrast cue by construction and grain is literal noise across
text, so they belong to the same family of decisions as `glassBlur` and
`glowScale`; a theme that flattens one and not the other is half-done.

---

## 7. Module Blocks

The material tokens say what a *surface* is made of. `.module` says how a
**panel is laid out**: not one flat plane, but a grid of separate blocks,
each a top-lit face sitting in a darker gutter, each wearing its name on a
tab at its top-left. It is the idiom every hardware-style plugin uses, and
the plugin browser is built from it.

```html
<div class="module-rack">
  <div class="module lit">
    <div class="module-head">CLAP  Surge XT</div>
    <div class="module-body"> … controls … </div>
  </div>
</div>
```

| Class | Role |
|---|---|
| `.module-rack` | The gutter the blocks sit in. Paints `--bg-0`. |
| `.module` | One block: a `--bg-2` face with the theme's sheen, bevel and cast shadow. |
| `.module-head` | The name tab. `align-self: flex-start`, so it takes only its label's width and the face continues past it — the detail that reads as a legend plate rather than a card header. |
| `.module.lit` | The block's function is switched ON: the **tab** lights, not the face. On real gear the legend carries the state, and lighting the whole panel leaves nowhere for selection to show. |
| `.module-body` | The contents. |
| `.module-controls` | A row of controls across the face, wrapping. |

Two things make this read as hardware rather than as cards, and both are
easy to get wrong:

1. **The gutter must be darker than the face.** Cards on a page share their
   parent's background and are separated by whitespace; objects on a panel
   are separated by shadow. A rack whose ground matches its modules reads as
   boxes drawn on paper no matter how much bevel they carry. A test holds
   Rack Slate's `bg0` clearly below its `bg1` for this reason.
2. **The face gradient does more work than the bevel.** `--sheen-face` is the
   cue that sells it — a flat fill with a crisp lit edge still looks like a
   `div`. Rack Slate runs `sheen` at 0.85, the highest of any built-in, and a
   test keeps it there.

The surface ramp matters too: a module is `--bg-2` and its tab `--bg-3`, so
each step stands proud of the panel *and* of the block beneath it. Set a
module to `--bg-1` and on most themes it goes level with the dock behind it
and vanishes.

Because every value is a token, the same markup is painted steel under Rack
Slate, milled aluminium under Console Noir, and plain flat boxes under either
high-contrast theme — no variants, no per-theme markup.

---

## 8. Exporting Themes

To quickly create a custom theme, navigate to **Preferences** → **Interface** and click **EXPORT CURRENT THEME…**.

This exports the active theme's fully resolved tokens into your user themes directory as `<active-theme>-copy.json` (e.g. `aura-dark-copy.json`). You can then edit that file in your text editor.

Exporting never overwrites: if that name is taken, the next free one is used (`aura-dark-copy-2.json`, `-3`, …). The export exists to be edited, so a second click always gives you a fresh file rather than discarding the edits you made to the first. The toast names the exact path written.

---

## 9. Reloading & Error Handling

- **Discovery**: When AURA launches, it scans the themes directory and registers all valid user themes. Adding or deleting a `.json` file requires restarting AURA to discover the file.
- **Robust Validation**: The theme parser never crashes. If a file contains invalid JSON or lacks a `name`, it is skipped, and a toast notification informs you of the failure. If an individual token has an invalid color or unknown name, only that key is ignored while the rest of the theme loads safely against its base.
