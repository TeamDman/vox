import { blake3 } from "@noble/hashes/blake3.js";

const MAGIC = "PHONFIL1";
const MANIFEST_MAGIC = "PHONFTR1";
const HEADER_SIZE = 32;
const DESCRIPTOR_SIZE = 72;
const ALIGNMENT = 16;
const decoder = new TextDecoder("utf-8", { fatal: true });

export interface DurableExtent { kind: number; number: number; offset: number; length: number; schemaId: string | null }
export interface DurableAux { feature: string; name: string; number: number }
export interface DurableFile { format: number; fileLength: number; descriptorOffset: number; requiredFeatures: string[]; optionalFeatures: string[]; extents: DurableExtent[]; aux: DurableAux[] }
export interface RegionReference { targetRegion: number; targetSchemaId: string }
export type FeatureValidator = (aux: readonly DurableAux[]) => void;

class Cursor {
  private readonly view: DataView;
  private position = 0;
  constructor(private readonly bytes: Uint8Array) { this.view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength) }
  remaining(): number { return this.bytes.length - this.position }
  take(length: number): Uint8Array {
    const end = this.position + length;
    if (!Number.isSafeInteger(end) || end > this.bytes.length) throw new Error("truncated durable file");
    const result = this.bytes.subarray(this.position, end);
    this.position = end;
    return result;
  }
  u32(): number {
    if (this.remaining() < 4) throw new Error("truncated durable file");
    const value = this.view.getUint32(this.position, true);
    this.position += 4;
    return value;
  }
  name(): string {
    const encoded = this.take(this.u32());
    const nested = new Cursor(encoded);
    const value = decoder.decode(nested.take(nested.u32()));
    if (nested.remaining() !== 0 || !isQualifiedName(value)) throw new Error("invalid qualified name");
    return value;
  }
}
function isQualifiedName(value: string): boolean {
  if (new TextEncoder().encode(value).length > 255) return false;
  const labels = value.split(".");
  return labels.length >= 2 && labels.every((label) => /^[a-z][a-z0-9]*(?:-[a-z0-9]+)*$/.test(label));
}
function u32(view: DataView, offset: number): number {
  if (offset < 0 || offset + 4 > view.byteLength) throw new Error("truncated durable file");
  return view.getUint32(offset, true);
}
function u64(view: DataView, offset: number): number {
  if (offset < 0 || offset + 8 > view.byteLength) throw new Error("truncated durable file");
  const value = view.getBigUint64(offset, true);
  if (value > BigInt(Number.MAX_SAFE_INTEGER)) throw new Error("durable integer exceeds host range");
  return Number(value);
}
function sortedUnique(names: readonly string[]): boolean {
  return names.every((name, index) => index === 0 || names[index - 1]! < name);
}
function ascii(bytes: Uint8Array): string { return String.fromCharCode(...bytes) }
function aligned(value: number): number { return Math.ceil(value / ALIGNMENT) * ALIGNMENT }
function schemaHex(view: DataView, offset: number): string | null {
  const value = view.getBigUint64(offset, true);
  return value === 0n ? null : value.toString(16).padStart(16, "0");
}
function parseManifest(bytes: Uint8Array): { required: string[]; optional: string[]; aux: DurableAux[] } {
  if (bytes.length === 0) return { required: [], optional: [], aux: [] };
  const cursor = new Cursor(bytes);
  if (ascii(cursor.take(8)) !== MANIFEST_MAGIC) throw new Error("invalid feature manifest magic");
  const readNames = (): string[] => Array.from({ length: cursor.u32() }, () => cursor.name());
  const required = readNames();
  const optional = readNames();
  const aux = Array.from({ length: cursor.u32() }, () => ({ feature: cursor.name(), name: cursor.name(), number: cursor.u32() }));
  if (cursor.remaining() !== 0) throw new Error("trailing feature manifest bytes");
  if (!sortedUnique(required) || !sortedUnique(optional) || required.some((name) => optional.includes(name))) throw new Error("noncanonical feature lists");
  const declared = new Set([...required, ...optional]);
  const expected = new Map<string, number>();
  for (let index = 0; index < aux.length; index++) {
    const identity = aux[index]!;
    if (!declared.has(identity.feature)) throw new Error("Aux feature is not declared");
    const key = `${identity.feature}\0${identity.name}`;
    const number = expected.get(key) ?? 0;
    if (identity.number !== number) throw new Error("noncanonical Aux numbering");
    expected.set(key, number + 1);
    if (index > 0) {
      const prior = aux[index - 1]!;
      const priorKey = `${prior.feature}\0${prior.name}\0${prior.number.toString().padStart(10, "0")}`;
      const currentKey = `${identity.feature}\0${identity.name}\0${identity.number.toString().padStart(10, "0")}`;
      if (priorKey >= currentKey) throw new Error("noncanonical Aux order");
    }
  }
  return { required, optional, aux };
}

// r[impl compact.file.bootstrap]
// r[impl compact.file.format-version]
// r[impl compact.file.admission]
export function parseDurableFile(bytes: Uint8Array, refs: readonly RegionReference[]): DurableFile {
  if (bytes.length < HEADER_SIZE || ascii(bytes.subarray(0, 8)) !== MAGIC) throw new Error("invalid durable magic");
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const format = u32(view, 8);
  if (format !== 1) throw new Error(`unsupported format ${format}`);
  const count = u32(view, 12);
  const fileLength = u64(view, 16);
  if (fileLength !== bytes.length) throw new Error("durable file length mismatch");
  const descriptorOffset = u64(view, 24);
  if (descriptorOffset < HEADER_SIZE) throw new Error("invalid descriptor offset");
  const descriptorEnd = descriptorOffset + count * DESCRIPTOR_SIZE;
  if (!Number.isSafeInteger(descriptorEnd) || descriptorEnd > bytes.length) throw new Error("truncated descriptors");
  const manifest = parseManifest(bytes.subarray(HEADER_SIZE, descriptorOffset));
  const extents: DurableExtent[] = [];
  let expectedOffset = aligned(descriptorEnd);
  for (let index = 0; index < count; index++) {
    const base = descriptorOffset + index * DESCRIPTOR_SIZE;
    const kind = u32(view, base);
    const number = u32(view, base + 4);
    const offset = u64(view, base + 8);
    const length = u64(view, base + 16);
    if (u64(view, base + 24) !== ALIGNMENT || offset !== expectedOffset || offset % ALIGNMENT !== 0) throw new Error("noncanonical extent placement");
    const end = offset + length;
    if (!Number.isSafeInteger(end) || end > bytes.length) throw new Error("truncated extent");
    const expectedDigest = bytes.subarray(base + 40, base + 72);
    const actualDigest = blake3(bytes.subarray(offset, end));
    if (!actualDigest.every((byte, digestIndex) => byte === expectedDigest[digestIndex])) throw new Error("extent digest mismatch");
    extents.push({ kind, number, offset, length, schemaId: schemaHex(view, base + 32) });
    expectedOffset = index + 1 < count ? aligned(end) : end;
  }
  if (expectedOffset !== fileLength || extents.length < 2 || extents[0]!.kind !== 0 || extents[1]!.kind !== 1) throw new Error("invalid extent table");
  let regionCount = 0;
  while (regionCount + 2 < extents.length && extents[regionCount + 2]!.kind === 2) {
    if (extents[regionCount + 2]!.number !== regionCount) throw new Error("noncanonical region numbering");
    regionCount++;
  }
  const auxExtents = extents.slice(regionCount + 2);
  if (auxExtents.length !== manifest.aux.length || auxExtents.some((extent, index) => extent.kind !== 3 || extent.number !== manifest.aux[index]!.number)) throw new Error("Aux descriptors do not match manifest");
  for (const ref of refs) {
    const region = extents[ref.targetRegion + 2];
    if (!region || ref.targetRegion >= regionCount) throw new Error("dangling region reference");
    if (region.schemaId !== ref.targetSchemaId) throw new Error("region schema mismatch");
  }
  const reachable = new Set(refs.map((ref) => ref.targetRegion));
  for (let region = 0; region < regionCount; region++) if (!reachable.has(region)) throw new Error("unreachable region");
  return { format, fileLength, descriptorOffset, requiredFeatures: manifest.required, optionalFeatures: manifest.optional, extents, aux: manifest.aux };
}

export function validateDurableFeatures(file: DurableFile, validators: ReadonlyMap<string, FeatureValidator>): { discardedAux: number; diagnostics: Array<{ feature: string; message: string }> } {
  for (const feature of file.requiredFeatures) {
    const validator = validators.get(feature);
    if (!validator) throw new Error(`unknown required feature ${feature}`);
    validator(file.aux.filter((aux) => aux.feature === feature));
  }
  let discardedAux = 0;
  const diagnostics: Array<{ feature: string; message: string }> = [];
  for (const feature of file.optionalFeatures) {
    const validator = validators.get(feature);
    if (!validator) continue;
    const aux = file.aux.filter((extent) => extent.feature === feature);
    try { validator(aux) } catch (error) {
      discardedAux += aux.length;
      diagnostics.push({ feature, message: error instanceof Error ? error.message : String(error) });
    }
  }
  return { discardedAux, diagnostics };
}
