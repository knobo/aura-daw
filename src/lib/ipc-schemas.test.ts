/**
 * Smoke tests for the three docs/ipc-schemas/*.json files touched by Task
 * 15's schema-doc-honesty work: they must parse as JSON, and a few
 * load-bearing fields must be exactly where persist.rs actually puts them
 * (schemaVersion 3, the content/placements/lanes split). No JSON-Schema
 * validator runs anywhere in this repo (grep turned up none) — this is
 * deliberately just "does it parse and are the pinned fields there", per
 * the existing docs/ipc-schemas convention of hand-reviewed, not
 * machine-validated, schema files.
 */
import { describe, expect, it } from "vitest";
import projectV3Schema from "../../docs/ipc-schemas/project-v3.schema.json";
import projectV2Schema from "../../docs/ipc-schemas/project-v2.schema.json";
import midiClipSchema from "../../docs/ipc-schemas/midi-clip.schema.json";
import harmonySchema from "../../docs/ipc-schemas/harmony.schema.json";
import composerSchema from "../../docs/ipc-schemas/composer.schema.json";

describe("docs/ipc-schemas/project-v3.schema.json", () => {
  const schema = projectV3Schema as Record<string, unknown>;

  it("parses and pins schemaVersion to the const 3", () => {
    const props = schema.properties as Record<string, { const?: number }>;
    expect(props.schemaVersion.const).toBe(3);
  });

  it("describes the v3 content/placements/lanes split, not v2's midiClips as a write target", () => {
    const props = schema.properties as Record<string, unknown>;
    expect(props.content).toBeDefined();
    expect(props.placements).toBeDefined();
    expect(props.lanes).toBeDefined();
    expect(props.meterMap).toBeDefined();
    expect(props.sectionTableRuleVersion).toBeDefined();
    // tempoMap is period-shaped at v3, not the v2 {tick,bpm} shape.
    const tempoPeriodEvent = (schema.$defs as Record<string, { properties?: Record<string, unknown> }>)
      .tempoPeriodEvent;
    expect(tempoPeriodEvent?.properties?.periodStart).toBeDefined();
    expect(tempoPeriodEvent?.properties?.periodEnd).toBeDefined();
  });

  it("cites persist.rs as the source of truth", () => {
    expect(String(schema.description)).toContain("persist.rs");
  });
});

describe("docs/ipc-schemas/project-v2.schema.json", () => {
  const schema = projectV2Schema as Record<string, unknown>;

  it("parses and marks itself superseded by v3", () => {
    expect(String(schema.description)).toContain("SUPERSEDED");
    expect(String(schema.description)).toContain("project-v3.schema.json");
  });
});

describe("docs/ipc-schemas/midi-clip.schema.json", () => {
  const schema = midiClipSchema as Record<string, unknown>;

  it("parses and splits the v3 persisted row into content/placement/lane defs", () => {
    const defs = schema.$defs as Record<string, unknown>;
    expect(defs.persistedContentRow).toBeDefined();
    expect(defs.persistedPlacementRow).toBeDefined();
    expect(defs.persistedLaneRow).toBeDefined();
    // The v2 shape stays, renamed and marked legacy — not deleted.
    expect(defs.persistedClipLegacy).toBeDefined();
  });
});

describe("docs/ipc-schemas/harmony.schema.json (the Composer, Plan H1)", () => {
  const schema = harmonySchema as Record<string, unknown>;

  it("parses and describes the map PAIR, in ticks", () => {
    const props = schema.properties as Record<string, unknown>;
    expect(props.keys).toBeDefined();
    expect(props.chords).toBeDefined();
    const defs = schema.$defs as Record<string, { properties?: Record<string, { type?: unknown }> }>;
    expect(defs.keySpan?.properties?.tick?.type).toBe("integer");
    expect(defs.chordSpan?.properties?.lengthTicks?.type).toBe("integer");
  });

  it("pins the string wire form for keys and chords", () => {
    const defs = schema.$defs as Record<string, { properties?: Record<string, { type?: unknown }> }>;
    // Spelled strings, not integers: the enharmonic distinction is
    // load-bearing and a persisted format has to keep it.
    expect(defs.keySpan?.properties?.key?.type).toBe("string");
    expect(defs.chordSpan?.properties?.chord?.type).toBe("string");
  });

  it("cites the module that owns it, and says the key is written only when used", () => {
    expect(String(schema.description)).toContain("theory/harmony.rs");
    expect(String(schema.description)).toContain("non-empty");
  });
});

describe("docs/ipc-schemas/composer.schema.json (the Composer, Plan H1)", () => {
  const schema = composerSchema as Record<string, unknown>;
  const defs = schema.$defs as Record<string, Record<string, unknown>>;

  it("parses and covers every command's payload", () => {
    for (const key of [
      "harmonyView",
      "paletteView",
      "suggestion",
      "generateRequest",
      "generateReply",
    ]) {
      expect(defs[key], key).toBeDefined();
    }
  });

  it("enumerates the nine note roles, avoid among them", () => {
    const role = (defs.noteClass.properties as Record<string, { enum?: string[] }>).role;
    expect(role.enum).toHaveLength(9);
    expect(role.enum).toContain("avoid");
    expect(role.enum).toContain("extension");
  });

  it("says out loud that annotations are not document state", () => {
    expect(String(defs.annotation.description)).toContain("never persisted");
    expect(String(schema.description)).toContain("no generated-clip type");
  });

  it("cites control/composer.rs as the source of truth", () => {
    expect(String(schema.description)).toContain("control/composer.rs");
  });
});
