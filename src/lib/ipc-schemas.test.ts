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
