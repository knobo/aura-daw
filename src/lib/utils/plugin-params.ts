/**
 * Shared plugin-parameter naming and formatting (design §6.1-6.3): the
 * automation matrix, pinned chips and the generic param panel all need to
 * render the SAME text for the same param/value or the chip and the fader
 * disagree. Extracted, byte-identical in behaviour, from
 * `PluginParamPanel.svelte`'s private `fmt`/`unitOf`/group-splitting.
 */
import type { PluginParamInfo } from "../types/ipc";

/** "Filter / Cutoff" → "Cutoff"; a name with no " / " is returned as is. */
export function shortParamName(name: string): string {
  const cut = name.indexOf(" / ");
  return cut > 0 ? name.slice(cut + 3) : name;
}

/** The group half of a "Group / Name" param name, or "parameters". */
export function paramGroupName(name: string): string {
  const cut = name.indexOf(" / ");
  return cut > 0 ? name.slice(0, cut) : "parameters";
}

export function formatParamValue(p: PluginParamInfo, value = p.value): string {
  const name = p.name.toLowerCase();
  if ((p.steps ?? 0) > 0) return String(Math.round(value));
  if (name.includes("cutoff") || name.includes("freq")) {
    return value >= 1000 ? `${(value / 1000).toFixed(2)}k` : `${Math.round(value)}`;
  }
  if (Math.abs(p.max) <= 1 && Math.abs(p.min) <= 1) return value.toFixed(2);
  return Math.abs(value) >= 100 ? value.toFixed(0) : value.toFixed(1);
}

export function paramUnit(p: PluginParamInfo): string {
  const name = p.name.toLowerCase();
  if ((p.steps ?? 0) > 0) return "";
  if (name.includes("cutoff") || name.includes("freq")) return "hz";
  if (name.includes("detune")) return "ct";
  if (name.includes("attack") || name.includes("release") || name.includes("decay")) return "s";
  if (name.includes("pan")) return "";
  if (p.min === 0 && p.max === 1) return "";
  return "";
}

/** `formatParamValue` + `paramUnit`, the single string a chip shows. */
export function formatParamDisplay(p: PluginParamInfo, value = p.value): string {
  return `${formatParamValue(p, value)}${paramUnit(p)}`;
}

/**
 * Why this parameter must not get an automation lane, or `null` when it may.
 *
 * A plugin can declare that CHANGING a parameter is expensive
 * (`pprops:expensive`) or that it must not be automated at all
 * (`kx:NonAutomatable`); the backend folds both into
 * `PluginParamInfo.nonAutomatable`. ZamVerb's "Room" carries both, because
 * the value selects the convolution impulse response — a lane sweeping it
 * would reload an IR per block and the user hears that as crackling.
 *
 * The wording lives here, not at the three surfaces that mint lanes (the
 * param panel's `A` button, the pinned chips, the automation matrix), so
 * they cannot drift.
 */
export function nonAutomatableRefusal(p: PluginParamInfo | undefined): string | null {
  if (!p?.nonAutomatable) return null;
  return `${shortParamName(p.name)} — the plugin says changing this one is expensive (it reloads something), so a lane would crackle. Set it by hand.`;
}

/** 0..1 position of `value` in the param's range; 0 when min === max. */
export function paramNormalized(p: PluginParamInfo, value = p.value): number {
  return p.max === p.min ? 0 : (value - p.min) / (p.max - p.min);
}
