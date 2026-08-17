# Themes in AURA

AURA features a full, tokenised theme system supporting eight built-in themes and user-authored JSON themes from disk. Every visual element—from UI chrome, panels, and dialogs to canvas-rendered timeline tracks, piano roll notes, and audio meters—is styled strictly through reactive theme tokens.

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
    "glassAlpha": "1"
  }
}
```

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

`glowScale` is a multiplier rather than a radius because each glow keeps its own designed size — a 22px bloom under a dialog and a 6px rim on a fader thumb are not the same effect — and because a radius animation needs its two ends to stay apart. Set it to `0.5` to halve every glow, or `0` to remove them.

Pair `glassAlpha: "1"` with `glassBlur: "0px"`. A panel that is translucent but unblurred shows the raw timeline grid through its own text, so a theme turning off the frosting wants solid panels; the built-ins all follow this, and a test enforces it.

---

## 6. Exporting Themes

To quickly create a custom theme, navigate to **Preferences** → **Interface** and click **EXPORT CURRENT THEME…**.

This exports the active theme's fully resolved tokens into your user themes directory as `<active-theme>-copy.json` (e.g. `aura-dark-copy.json`). You can then edit that file in your text editor.

Exporting never overwrites: if that name is taken, the next free one is used (`aura-dark-copy-2.json`, `-3`, …). The export exists to be edited, so a second click always gives you a fresh file rather than discarding the edits you made to the first. The toast names the exact path written.

---

## 7. Reloading & Error Handling

- **Discovery**: When AURA launches, it scans the themes directory and registers all valid user themes. Adding or deleting a `.json` file requires restarting AURA to discover the file.
- **Robust Validation**: The theme parser never crashes. If a file contains invalid JSON or lacks a `name`, it is skipped, and a toast notification informs you of the failure. If an individual token has an invalid color or unknown name, only that key is ignored while the rest of the theme loads safely against its base.
