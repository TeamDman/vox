import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { parseDurableFile, validateDurableFeatures } from "./durable.ts";

interface Manifest {
  fixture: string;
  sha256: string;
  format: number;
  fileLength: number;
  descriptorOffset: number;
  requiredFeatures: string[];
  optionalFeatures: string[];
  extents: Array<{ kind: number; number: number; offset: number; length: number; schemaId: string | null }>;
  aux: Array<{ feature: string; name: string; number: number }>;
  regionReferences: Array<{ targetRegion: number; targetSchemaId: string }>;
  policy: Record<string, string>;
}

const manifestPath = fileURLToPath(new URL("../../../../conformance/durable-v1.json", import.meta.url));
const manifest = JSON.parse(readFileSync(manifestPath, "utf8")) as Manifest;
const fixturePath = fileURLToPath(new URL(`../../../../conformance/${manifest.fixture}`, import.meta.url));
const bytes = new Uint8Array(readFileSync(fixturePath));

// r[verify compact.file.bootstrap]
// r[verify compact.file.format-version]
// r[verify compact.file.admission]
describe("independent durable file consumer", () => {
  it("matches the committed Rust golden and machine-readable manifest", () => {
    expect(createHash("sha256").update(bytes).digest("hex")).toBe(manifest.sha256);
    const parsed = parseDurableFile(bytes, manifest.regionReferences);
    expect(parsed.format).toBe(manifest.format);
    expect(parsed.fileLength).toBe(manifest.fileLength);
    expect(parsed.descriptorOffset).toBe(manifest.descriptorOffset);
    expect(parsed.requiredFeatures).toEqual(manifest.requiredFeatures);
    expect(parsed.optionalFeatures).toEqual(manifest.optionalFeatures);
    expect(parsed.extents).toEqual(manifest.extents);
    expect(parsed.aux).toEqual(manifest.aux);
  });

  it("rejects corrupt framing, tables, references, and digests independently", () => {
    for (const mutate of [
      (copy: Uint8Array) => copy.fill(0, 0, 8),
      (copy: Uint8Array) => new DataView(copy.buffer).setUint32(8, 2, true),
      (copy: Uint8Array) => new DataView(copy.buffer).setBigUint64(16, BigInt(copy.length + 1), true),
      (copy: Uint8Array) => copy.fill(0xff, 32, 40),
      (copy: Uint8Array) => new DataView(copy.buffer).setUint32(manifest.descriptorOffset + 2 * 72 + 4, 1, true),
      (copy: Uint8Array) => { copy[copy.length - 1] ^= 1; },
    ]) {
      const copy = bytes.slice();
      mutate(copy);
      expect(() => parseDurableFile(copy, manifest.regionReferences)).toThrow();
    }
    expect(() => parseDurableFile(bytes, [{ targetRegion: 1, targetSchemaId: "ba8125876d6388b4" }])).toThrow(/region/);
    expect(() => parseDurableFile(bytes, [{ targetRegion: 0, targetSchemaId: "281c5be4f2ee63b4" }])).toThrow(/schema/);
  });

  it("enforces the required-versus-optional failure matrix", () => {
    const parsed = parseDurableFile(bytes, manifest.regionReferences);
    expect(() => validateDurableFeatures(parsed, new Map())).not.toThrow();

    const required = { ...parsed, requiredFeatures: parsed.optionalFeatures, optionalFeatures: [] };
    expect(() => validateDurableFeatures(required, new Map())).toThrow(/unknown required feature/);

    const invalid = new Map([["org.example.index-v1", () => { throw new Error("invalid index"); }]]);
    expect(() => validateDurableFeatures(required, invalid)).toThrow(/invalid index/);
    expect(validateDurableFeatures(parsed, invalid)).toEqual({
      discardedAux: 1,
      diagnostics: [{ feature: "org.example.index-v1", message: "invalid index" }],
    });
  });
});
