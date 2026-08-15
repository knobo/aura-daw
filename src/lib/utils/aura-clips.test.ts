import { describe, expect, it } from "vitest";
import { encodeAuraClips, parseAuraClips } from "./aura-clips";
import type { AuraClipsPayload } from "../types/ipc";

const payload: AuraClipsPayload = {
  mime: "application/x-aura-clips",
  schemaVersion: 1,
  anchorSamples: 48000,
  anchorTicks: 960,
  ppq: 960,
  sourceProjectDir: "/home/u/Songs/Demo.aura",
  clips: [
    {
      kind: "midi",
      name: "riff",
      sourceTrackId: "t1",
      sourceTrackName: "Lead",
      offsetFromAnchorTicks: 0,
      lengthTicks: 1920,
      contentLengthTicks: null,
      notes: [{ tick: 0, lengthTicks: 480, key: 60, velocity: 100, channel: 0, noteId: 0 }],
    },
  ],
};

describe("aura-clips envelope", () => {
  it("round-trips through encode/parse", () => {
    expect(parseAuraClips(encodeAuraClips(payload))).toEqual(payload);
  });

  it("starts with a magic line, so a human pasting it elsewhere sees what it is", () => {
    expect(encodeAuraClips(payload).startsWith("AURA-CLIPS/1\n")).toBe(true);
  });

  it("returns null for unrelated clipboard text instead of throwing", () => {
    expect(parseAuraClips("just some text a user copied")).toBeNull();
    expect(parseAuraClips("")).toBeNull();
    expect(parseAuraClips("AURA-CLIPS/1\n{not json")).toBeNull();
  });

  it("returns null for JSON that is not an AURA payload", () => {
    expect(parseAuraClips('AURA-CLIPS/1\n{"mime":"text/plain","clips":[]}')).toBeNull();
  });

  it("accepts a payload whose schemaVersion is older", () => {
    const older = { ...payload, schemaVersion: 1 };
    expect(parseAuraClips(encodeAuraClips(older))?.schemaVersion).toBe(1);
  });

  it("still parses a NEWER schema version — the backend, not the codec, decides", () => {
    // the backend returns a clear "schema is newer than this build" error;
    // the codec's job is only to recognise our own envelope.
    const newer = { ...payload, schemaVersion: 99 };
    expect(parseAuraClips(encodeAuraClips(newer))?.schemaVersion).toBe(99);
  });

  it("tolerates CRLF and a leading BOM before the magic line — the human round trip the module doc promises", () => {
    const crlf = `AURA-CLIPS/1\r\n${JSON.stringify(payload)}`;
    expect(parseAuraClips(crlf)).toEqual(payload);
    const bom = `﻿AURA-CLIPS/1\n${JSON.stringify(payload)}`;
    expect(parseAuraClips(bom)).toEqual(payload);
  });
});
