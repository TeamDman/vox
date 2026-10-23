// phon's schema model for TypeScript, plus the self-describing schema parser.
//
// A `Schema` is the structural description the compact/typed engine plans and
// codes against. The model mirrors the canonical Rust definitions in
// `rust/phon-schema/src/schema.rs`; `schemaFromBytes` is a byte-for-byte port of
// Rust `schema_from_bytes` (`selfdescribing.rs` `dec_schema`/`dec_kind`/
// `dec_ref`), so a TS peer reconstructs the exact same schema a Rust peer
// emitted — schema bytes are the source of truth (`r[codegen.schema-is-source-
// of-truth]`).
//
// Schemas reference each other by `SchemaId` (a content-derived u64); a
// `Registry` resolves those refs. Primitive ids are content-derived too and
// recognized intrinsically — the registry is seeded with a primitive id->tag
// table rather than recomputing the blake3 ids here.
//
// Spec: docs/content/spec.md — "Type system", "Schema identity",
// "Self-describing mode".

import { blake3 } from "@noble/hashes/blake3.js";
import { ByteSink, DecodeError, MAX_DEPTH, Reader, Tag, U64_MAX, ZST_COUNT_CAP } from "./wire.ts";

// ============================================================================
// The schema model (mirror of rust/phon-schema/src/schema.rs)
// ============================================================================

/// A leaf type. Represented by its tag string (the same strings Rust's
/// `Primitive::tag` produces), which doubles as the discriminant.
export type Primitive =
  | "bool"
  | "u8"
  | "u16"
  | "u32"
  | "u64"
  | "u128"
  | "i8"
  | "i16"
  | "i32"
  | "i64"
  | "i128"
  | "f32"
  | "f64"
  | "char"
  | "string"
  | "bytes"
  | "datetime"
  | "uuid"
  | "qname"
  | "unit"
  | "never";

export const PRIMITIVES = [
  "bool", "u8", "u16", "u32", "u64", "u128", "i8", "i16", "i32", "i64", "i128",
  "f32", "f64", "char", "string", "bytes", "datetime", "uuid", "qname", "unit", "never",
] as const satisfies readonly Primitive[];

const PRIMITIVE_TAGS = new Set<string>(PRIMITIVES);

function asPrimitive(tag: string): Primitive {
  if (!PRIMITIVE_TAGS.has(tag)) throw new DecodeError(`unknown primitive '${tag}'`);
  return tag as Primitive;
}

/// A reference to a schema: concrete (by content-derived id, with type args) or
/// a type variable (parametric schemas). The TS engine supports concrete,
/// non-generic refs; variables and type args are carried but rejected at use.
// r[impl type-system.generics]
export type SchemaRef =
  | { readonly kind: "concrete"; readonly id: bigint; readonly args: SchemaRef[] }
  | { readonly kind: "var"; readonly name: string };

export interface Field {
  readonly name: string;
  readonly schema: SchemaRef;
  /// A reader field that is *not* required may be absent from the writer and
  /// filled with its default (`r[compat.reader-only-fields]`).
  readonly required: boolean;
}

// r[impl type-system.variant-payloads]
export type VariantPayload =
  | { readonly kind: "unit" }
  | { readonly kind: "newtype"; readonly ref: SchemaRef }
  | { readonly kind: "tuple"; readonly refs: SchemaRef[] }
  | { readonly kind: "struct"; readonly fields: Field[] };

export interface Variant {
  readonly name: string;
  /// The wire discriminant (a u32). Variants are matched across schemas by name;
  /// the index is what travels on the wire.
  readonly index: number;
  readonly payload: VariantPayload;
}

export type ChannelDirection = "tx" | "rx";

// r[impl type-system.canonical-form]
// r[impl type-system.array]
// r[impl type-system.tensor]
// r[impl type-system.channel]
// r[impl type-system.dynamic]
// r[impl type-system.external]
export type SchemaKind =
  | { readonly kind: "primitive"; readonly primitive: Primitive }
  | { readonly kind: "struct"; readonly name: string; readonly fields: Field[] }
  | { readonly kind: "enum"; readonly name: string; readonly variants: Variant[] }
  | { readonly kind: "tuple"; readonly elements: SchemaRef[] }
  | { readonly kind: "list"; readonly element: SchemaRef }
  | { readonly kind: "set"; readonly element: SchemaRef }
  | { readonly kind: "map"; readonly key: SchemaRef; readonly value: SchemaRef }
  | { readonly kind: "array"; readonly element: SchemaRef; readonly dimensions: bigint[] }
  | { readonly kind: "tensor"; readonly element: SchemaRef; readonly rank: number | null }
  | { readonly kind: "option"; readonly element: SchemaRef }
  | { readonly kind: "channel"; readonly direction: ChannelDirection; readonly element: SchemaRef }
  | { readonly kind: "dynamic" }
  | { readonly kind: "external"; readonly external: string; readonly metadata: SchemaRef | null }
  | { readonly kind: "semantic"; readonly name: string; readonly args: SchemaRef[]; readonly representation: SchemaRef };
export interface Schema {
  readonly id: bigint;
  readonly typeParams: string[];
  readonly kind: SchemaKind;
}

// ============================================================================
// Alignment & zero-sized analysis (mirror of compact.rs `alignment` /
// `min_wire_size_ref` / `is_zero_sized_*`)
// ============================================================================

/// The compact-mode alignment of a primitive scalar — the only source of wire
/// padding (`r[impl compact.alignment]`). Everything else is byte-aligned.
export function alignment(p: Primitive): number {
  switch (p) {
    case "u16":
    case "i16":
      return 2;
    case "u32":
    case "i32":
    case "f32":
    case "char":
      return 4;
    case "u64":
    case "i64":
    case "f64":
      return 8;
    case "u128":
    case "i128":
      return 16;
    default:
      return 1;
  }
}

const MIN_WIRE_DEPTH = 64;

/// `0` when a sequence element provably encodes to zero bytes (a `unit`, an
/// empty struct/tuple, an array of those), else `1` — the value to hand
/// `Reader.readLen` (`r[validate.lengths]`).
export function minWireSizeRef(reg: Registry, ref: SchemaRef): number {
  return isZeroSizedRef(reg, ref, 0) ? 0 : 1;
}

function isZeroSizedRef(reg: Registry, ref: SchemaRef, depth: number): boolean {
  if (depth > MIN_WIRE_DEPTH) return false;
  let kind: SchemaKind;
  try {
    kind = reg.resolve(ref);
  } catch {
    return false;
  }
  return isZeroSizedKind(reg, kind, depth);
}

function isZeroSizedKind(reg: Registry, kind: SchemaKind, depth: number): boolean {
  switch (kind.kind) {
    case "primitive":
      return kind.primitive === "unit";
    case "struct":
      return kind.fields.every((f) => isZeroSizedRef(reg, f.schema, depth + 1));
    case "tuple":
      return kind.elements.every((element) => isZeroSizedRef(reg, element, depth + 1));
    case "array":
      return isZeroSizedRef(reg, kind.element, depth + 1);
    case "semantic":
      return isZeroSizedRef(reg, kind.representation, depth + 1);
    default:
      return false;
  }
}

// ============================================================================
// Schema identity (port of rust/phon-schema/src/identity.rs)
// ============================================================================

function finalizedId(bytes: Uint8Array): bigint {
  const hash = blake3(bytes);
  return new DataView(hash.buffer, hash.byteOffset, hash.byteLength).getBigUint64(0, true);
}

function hashWith(write: (out: ByteSink) => void): bigint {
  const out = new ByteSink();
  write(out);
  return finalizedId(out.finish());
}

function writeTypeParams(out: ByteSink, params: readonly string[]): void {
  if (params.length === 0) return;
  out.str("type-params");
  out.u32(params.length);
  for (const param of params) out.str(param);
}

// r[impl schema-identity.canonical-encoding]
// r[impl schema-identity.computation]
// r[impl schema-identity.content-hash]
export function primitiveId(p: Primitive): bigint {
  return hashWith((out) => out.str(p));
}

export function primitiveTable(): { id: bigint; tag: Primitive }[] {
  return PRIMITIVES.map((tag) => ({ id: primitiveId(tag), tag }));
}

function visitKindTargets(kind: SchemaKind, f: (id: bigint) => void): void {
  switch (kind.kind) {
    case "primitive":
    case "dynamic":
      return;
    case "struct":
      kind.fields.forEach((field) => visitRefTargets(field.schema, f));
      return;
    case "enum":
      kind.variants.forEach((variant) => visitPayloadTargets(variant.payload, f));
      return;
    case "tuple":
      kind.elements.forEach((ref) => visitRefTargets(ref, f));
      return;
    case "list":
    case "set":
    case "array":
    case "tensor":
    case "option":
    case "channel":
      visitRefTargets(kind.element, f);
      return;
    case "map":
      visitRefTargets(kind.key, f);
      visitRefTargets(kind.value, f);
      return;
    case "external":
      if (kind.metadata) visitRefTargets(kind.metadata, f);
      return;
    case "semantic":
      kind.args.forEach((arg) => visitRefTargets(arg, f));
      visitRefTargets(kind.representation, f);
      return;
  }
}

function visitPayloadTargets(payload: VariantPayload, f: (id: bigint) => void): void {
  switch (payload.kind) {
    case "unit":
      return;
    case "newtype":
      visitRefTargets(payload.ref, f);
      return;
    case "tuple":
      payload.refs.forEach((ref) => visitRefTargets(ref, f));
      return;
    case "struct":
      payload.fields.forEach((field) => visitRefTargets(field.schema, f));
      return;
  }
}

function visitRefTargets(ref: SchemaRef, f: (id: bigint) => void): void {
  if (ref.kind === "var") return;
  f(ref.id);
  ref.args.forEach((arg) => visitRefTargets(arg, f));
}

class Tarjan {
  private nextOrder = 0;
  private readonly order: (number | undefined)[];
  private readonly lowlink: number[];
  private readonly onStack: boolean[];
  private readonly stack: number[] = [];
  private readonly adj: number[][];
  readonly sccs: number[][] = [];

  private constructor(adj: number[][]) {
    this.adj = adj;
    this.order = Array.from({ length: adj.length });
    this.lowlink = Array.from({ length: adj.length }, () => 0);
    this.onStack = Array.from({ length: adj.length }, () => false);
  }

  static run(adj: number[][]): number[][] {
    const t = new Tarjan(adj);
    for (let v = 0; v < adj.length; v++) {
      if (t.order[v] === undefined) t.strongconnect(v);
    }
    return t.sccs;
  }

  private strongconnect(v: number): void {
    this.order[v] = this.nextOrder;
    this.lowlink[v] = this.nextOrder;
    this.nextOrder++;
    this.stack.push(v);
    this.onStack[v] = true;

    for (const w of this.adj[v]!) {
      if (this.order[w] === undefined) {
        this.strongconnect(w);
        this.lowlink[v] = Math.min(this.lowlink[v]!, this.lowlink[w]!);
      } else if (this.onStack[w]) {
        this.lowlink[v] = Math.min(this.lowlink[v]!, this.order[w]!);
      }
    }

    if (this.lowlink[v] === this.order[v]) {
      const scc: number[] = [];
      while (true) {
        const w = this.stack.pop();
        if (w === undefined) throw new Error("empty Tarjan stack");
        this.onStack[w] = false;
        scc.push(w);
        if (w === v) break;
      }
      this.sccs.push(scc);
    }
  }
}

class IdentityWalk {
  private readonly batch: readonly Schema[];
  private readonly keyToIndex: ReadonlyMap<bigint, number>;
  private readonly component: ReadonlySet<number>;
  private readonly assigned: ReadonlyMap<number, bigint>;

  constructor(
    batch: readonly Schema[],
    keyToIndex: ReadonlyMap<bigint, number>,
    component: ReadonlySet<number>,
    assigned: ReadonlyMap<number, bigint>,
  ) {
    this.batch = batch;
    this.keyToIndex = keyToIndex;
    this.component = component;
    this.assigned = assigned;
  }

  // r[impl schema-identity.canonical-encoding]
  schema(idx: number, path: readonly number[], out: ByteSink): void {
    const schema = this.batch[idx]!;
    writeTypeParams(out, schema.typeParams);
    switch (schema.kind.kind) {
      case "primitive":
        out.str(schema.kind.primitive);
        return;
      case "struct":
        out.str("struct");
        out.str(schema.kind.name);
        out.u32(schema.kind.fields.length);
        schema.kind.fields.forEach((field) => this.field(field, path, out));
        return;
      case "enum":
        out.str("enum");
        out.str(schema.kind.name);
        out.u32(schema.kind.variants.length);
        for (const variant of schema.kind.variants) {
          out.str(variant.name);
          out.u32(variant.index);
          this.payload(variant.payload, path, out);
        }
        return;
      case "tuple":
        out.str("tuple");
        out.u32(schema.kind.elements.length);
        schema.kind.elements.forEach((ref) => this.reference(ref, path, out));
        return;
      case "list":
        out.str("list");
        this.reference(schema.kind.element, path, out);
        return;
      case "set":
        out.str("set");
        this.reference(schema.kind.element, path, out);
        return;
      case "option":
        out.str("option");
        this.reference(schema.kind.element, path, out);
        return;
      case "map":
        out.str("map");
        this.reference(schema.kind.key, path, out);
        this.reference(schema.kind.value, path, out);
        return;
      case "array":
        out.str("array");
        this.reference(schema.kind.element, path, out);
        out.u32(schema.kind.dimensions.length);
        schema.kind.dimensions.forEach((dimension) => out.u64(dimension));
        return;
      case "tensor":
        out.str("tensor");
        this.reference(schema.kind.element, path, out);
        if (schema.kind.rank === null) {
          out.u8(0);
        } else {
          out.u8(1);
          out.u32(schema.kind.rank);
        }
        return;
      case "channel":
        out.str("channel");
        out.str(schema.kind.direction);
        this.reference(schema.kind.element, path, out);
        return;
      case "dynamic":
        out.str("dynamic");
        return;
      case "external":
        out.str("external");
        out.str(schema.kind.external);
        if (schema.kind.metadata === null) {
          out.u8(0);
        } else {
          out.u8(1);
          this.reference(schema.kind.metadata, path, out);
        }
        return;
      case "semantic":
        out.str("semantic");
        out.str(schema.kind.name);
        out.u32(schema.kind.args.length);
        schema.kind.args.forEach((arg) => this.reference(arg, path, out));
        this.reference(schema.kind.representation, path, out);
        return;
    }
  }

  private field(field: Field, path: readonly number[], out: ByteSink): void {
    out.str(field.name);
    out.u8(field.required ? 1 : 0);
    this.reference(field.schema, path, out);
  }

  private payload(payload: VariantPayload, path: readonly number[], out: ByteSink): void {
    switch (payload.kind) {
      case "unit":
        out.str("unit");
        return;
      case "newtype":
        out.str("newtype");
        this.reference(payload.ref, path, out);
        return;
      case "tuple":
        out.str("tuple");
        out.u32(payload.refs.length);
        payload.refs.forEach((ref) => this.reference(ref, path, out));
        return;
      case "struct":
        out.str("struct");
        out.u32(payload.fields.length);
        payload.fields.forEach((field) => this.field(field, path, out));
        return;
    }
  }

  private reference(ref: SchemaRef, path: readonly number[], out: ByteSink): void {
    switch (ref.kind) {
      case "var":
        out.str("var");
        out.str(ref.name);
        return;
      case "concrete": {
        const target = this.keyToIndex.get(ref.id);
        if (target !== undefined && this.component.has(target)) {
          const depth = path.indexOf(target);
          if (depth >= 0) {
            out.str("backref");
            out.u32(depth);
          } else {
            out.str("inline");
            this.schema(target, [...path, target], out);
          }
        } else if (target !== undefined) {
          const id = this.assigned.get(target);
          if (id === undefined) throw new Error("dependency component assigned after dependent");
          out.str("concrete");
          out.u64(id);
        } else {
          out.str("concrete");
          out.u64(ref.id);
        }
        out.u32(ref.args.length);
        ref.args.forEach((arg) => this.reference(arg, path, out));
        return;
      }
    }
  }
}

function remapRef(ref: SchemaRef, map: ReadonlyMap<bigint, bigint>): SchemaRef {
  if (ref.kind === "var") return { kind: "var", name: ref.name };
  return {
    kind: "concrete",
    id: map.get(ref.id) ?? ref.id,
    args: ref.args.map((arg) => remapRef(arg, map)),
  };
}

function remapField(field: Field, map: ReadonlyMap<bigint, bigint>): Field {
  return { name: field.name, schema: remapRef(field.schema, map), required: field.required };
}

function remapPayload(payload: VariantPayload, map: ReadonlyMap<bigint, bigint>): VariantPayload {
  switch (payload.kind) {
    case "unit":
      return { kind: "unit" };
    case "newtype":
      return { kind: "newtype", ref: remapRef(payload.ref, map) };
    case "tuple":
      return { kind: "tuple", refs: payload.refs.map((ref) => remapRef(ref, map)) };
    case "struct":
      return { kind: "struct", fields: payload.fields.map((field) => remapField(field, map)) };
  }
}

function remapKind(kind: SchemaKind, map: ReadonlyMap<bigint, bigint>): SchemaKind {
  switch (kind.kind) {
    case "primitive":
    case "dynamic":
      return kind;
    case "struct":
      return { kind: "struct", name: kind.name, fields: kind.fields.map((field) => remapField(field, map)) };
    case "enum":
      return {
        kind: "enum",
        name: kind.name,
        variants: kind.variants.map((variant) => ({
          name: variant.name,
          index: variant.index,
          payload: remapPayload(variant.payload, map),
        })),
      };
    case "tuple":
      return { kind: "tuple", elements: kind.elements.map((ref) => remapRef(ref, map)) };
    case "list":
      return { kind: "list", element: remapRef(kind.element, map) };
    case "set":
      return { kind: "set", element: remapRef(kind.element, map) };
    case "option":
      return { kind: "option", element: remapRef(kind.element, map) };
    case "map":
      return { kind: "map", key: remapRef(kind.key, map), value: remapRef(kind.value, map) };
    case "array":
      return { kind: "array", element: remapRef(kind.element, map), dimensions: kind.dimensions };
    case "tensor":
      return { kind: "tensor", element: remapRef(kind.element, map), rank: kind.rank };
    case "channel":
      return { kind: "channel", direction: kind.direction, element: remapRef(kind.element, map) };
    case "external":
      return {
        kind: "external",
        external: kind.external,
        metadata: kind.metadata === null ? null : remapRef(kind.metadata, map),
      };
    case "semantic":
      return {
        kind: "semantic",
        name: kind.name,
        args: kind.args.map((arg) => remapRef(arg, map)),
        representation: remapRef(kind.representation, map),
      };
  }
}

// r[impl schema-identity.canonical-encoding]
// r[impl schema-identity.closure]
// r[impl schema-identity.computation]
// r[impl schema-identity.content-hash]
export function resolveIds(batch: readonly Schema[]): Schema[] {
  const keyToIndex = new Map<bigint, number>();
  batch.forEach((schema, index) => keyToIndex.set(schema.id, index));

  const adj = batch.map((): number[] => []);
  batch.forEach((schema, index) => {
    const seen = new Set<number>();
    visitKindTargets(schema.kind, (id) => {
      const target = keyToIndex.get(id);
      if (target !== undefined && !seen.has(target)) {
        seen.add(target);
        adj[index]!.push(target);
      }
    });
  });

  const sccs = Tarjan.run(adj);
  const assigned = new Map<number, bigint>();
  for (const scc of sccs) {
    const component = new Set(scc);
    const walk = new IdentityWalk(batch, keyToIndex, component, assigned);
    const local: [number, bigint][] = [];
    for (const index of scc) {
      local.push([index, hashWith((out) => walk.schema(index, [index], out))]);
    }
    for (const [index, id] of local) assigned.set(index, id);
  }

  const keyToReal = new Map<bigint, bigint>();
  batch.forEach((schema, index) => {
    const id = assigned.get(index);
    if (id === undefined) throw new Error("schema id was not assigned");
    keyToReal.set(schema.id, id);
  });

  return batch.map((schema, index) => {
    const id = assigned.get(index);
    if (id === undefined) throw new Error("schema id was not assigned");
    return { id, typeParams: [...schema.typeParams], kind: remapKind(schema.kind, keyToReal) };
  });
}

// ============================================================================
// Registry
// ============================================================================

/// Resolves `SchemaRef`s to `SchemaKind`s. Composite schemas are keyed by id;
/// primitive ids map to their tag. The engine plans and codes by walking
/// resolved kinds.
export class Registry {
  private readonly composites = new Map<bigint, Schema>();
  private readonly primitives = new Map<bigint, Primitive>();

  /// Build from parsed composite schemas plus a primitive id->tag table.
  /// When no table is supplied, primitive ids are computed locally.
  constructor(schemas: Iterable<Schema>, primitiveTableInput: Iterable<{ id: bigint; tag: Primitive }> = primitiveTable()) {
    for (const s of schemas) this.composites.set(s.id, s);
    for (const { id, tag } of primitiveTableInput) this.primitives.set(id, tag);
  }

  static validating(
    schemas: Iterable<Schema>,
    primitiveTableInput: Iterable<{ id: bigint; tag: Primitive }> = primitiveTable(),
  ): Registry {
    const schemaList = Array.from(schemas);
    validateSchemaBundle(schemaList);
    return new Registry(schemaList, primitiveTableInput);
  }

  schema(id: bigint): Schema | undefined {
    return this.composites.get(id);
  }

  /// A new registry with `extra` composite schemas merged in, sharing this
  /// registry's primitive table. Used to combine a peer's exchanged schemas
  /// (a writer closure) against the local reader registry for compat decode.
  /// Colliding ids are content-hashes, so an overwrite is identity.
  with(extra: Iterable<Schema>): Registry {
    const r = new Registry([], []);
    for (const [id, s] of this.composites) r.composites.set(id, s);
    for (const [id, t] of this.primitives) r.primitives.set(id, t);
    for (const s of extra) r.composites.set(s.id, s);
    return r;
  }

  withValidating(extra: Iterable<Schema>): Registry {
    const schemas = [...this.composites.values(), ...extra];
    validateSchemaBundle(schemas);
    return new Registry(schemas, [...this.primitives].map(([id, tag]) => ({ id, tag })));
  }

/// Resolve a concrete ref to a Var-free kind. A parametric schema's type
/// parameters are substituted by the ref's args, eagerly and per-reference, so
/// the walker never meets a `Var` (`r[type-system.generic-resolution]`). Each
/// arg carries its own binding forward, so no environment is threaded.
  // r[impl type-system.generic-resolution]
  // r[impl schema-identity.unknown-is-error]
  resolve(ref: SchemaRef): SchemaKind {
    if (ref.kind === "var") {
      throw new DecodeError("unbound type variable");
    }
    const prim = this.primitives.get(ref.id);
    if (prim !== undefined) {
      if (ref.args.length !== 0) throw new DecodeError("primitive carrying type arguments");
      return { kind: "primitive", primitive: prim };
    }
    const schema = this.composites.get(ref.id);
    if (schema === undefined) {
      throw new DecodeError(`unknown schema id ${ref.id.toString(16)}`);
    }
    if (schema.typeParams.length !== ref.args.length) {
      throw new DecodeError(
        `generic expects ${schema.typeParams.length} type arguments, got ${ref.args.length}`,
      );
    }
    if (ref.args.length === 0) return schema.kind;
    return substituteKind(schema.kind, schema.typeParams, ref.args);
  }
}

function validateKindRefs(kind: SchemaKind, provided: ReadonlySet<bigint>, primitives: ReadonlySet<bigint>): void {
  switch (kind.kind) {
    case "primitive":
    case "dynamic":
      return;
    case "struct":
      kind.fields.forEach((field) => validateRef(field.schema, provided, primitives));
      return;
    case "enum":
      kind.variants.forEach((variant) => validatePayloadRefs(variant.payload, provided, primitives));
      return;
    case "tuple":
      kind.elements.forEach((ref) => validateRef(ref, provided, primitives));
      return;
    case "list":
    case "set":
    case "array":
    case "tensor":
    case "option":
    case "channel":
      validateRef(kind.element, provided, primitives);
      return;
    case "map":
      validateRef(kind.key, provided, primitives);
      validateRef(kind.value, provided, primitives);
      return;
    case "external":
      if (kind.metadata) validateRef(kind.metadata, provided, primitives);
      return;
    case "semantic":
      kind.args.forEach((arg) => validateRef(arg, provided, primitives));
      validateRef(kind.representation, provided, primitives);
      return;
  }
}

function validatePayloadRefs(
  payload: VariantPayload,
  provided: ReadonlySet<bigint>,
  primitives: ReadonlySet<bigint>,
): void {
  switch (payload.kind) {
    case "unit":
      return;
    case "newtype":
      validateRef(payload.ref, provided, primitives);
      return;
    case "tuple":
      payload.refs.forEach((ref) => validateRef(ref, provided, primitives));
      return;
    case "struct":
      payload.fields.forEach((field) => validateRef(field.schema, provided, primitives));
      return;
  }
}

function validateRef(ref: SchemaRef, provided: ReadonlySet<bigint>, primitives: ReadonlySet<bigint>): void {
  if (ref.kind === "var") return;
  if (!provided.has(ref.id) && !primitives.has(ref.id)) {
    throw new DecodeError(`unknown schema id ${ref.id.toString(16)}`);
  }
  ref.args.forEach((arg) => validateRef(arg, provided, primitives));
}

function productDimensions(dimensions: readonly bigint[]): bigint {
  let product = 1n;
  for (const dimension of dimensions) {
    product *= dimension;
    if (product > U64_MAX) throw new DecodeError("array dimensions overflow");
  }
  return product;
}

function validateFixedArrayCaps(kind: SchemaKind, reg: Registry): void {
  switch (kind.kind) {
    case "primitive":
    case "dynamic":
      return;
    case "struct":
      kind.fields.forEach((field) => validateFixedArrayRef(field.schema));
      return;
    case "enum":
      kind.variants.forEach((variant) => validateFixedArrayPayload(variant.payload));
      return;
    case "tuple":
      kind.elements.forEach((ref) => validateFixedArrayRef(ref));
      return;
    case "list":
    case "set":
    case "tensor":
    case "option":
    case "channel":
      validateFixedArrayRef(kind.element);
      return;
    case "map":
      validateFixedArrayRef(kind.key);
      validateFixedArrayRef(kind.value);
      return;
    case "array": {
      const count = productDimensions(kind.dimensions);
      if (minWireSizeRef(reg, kind.element) === 0 && count > BigInt(ZST_COUNT_CAP)) {
        throw new DecodeError(`fixed array count ${count} exceeds zero-sized cap ${ZST_COUNT_CAP}`);
      }
      validateFixedArrayRef(kind.element);
      return;
    }
    case "external":
      if (kind.metadata) validateFixedArrayRef(kind.metadata);
      return;
    case "semantic":
      kind.args.forEach((arg) => validateFixedArrayRef(arg));
      validateFixedArrayRef(kind.representation);
      return;
  }
}

function validateFixedArrayPayload(payload: VariantPayload): void {
  switch (payload.kind) {
    case "unit":
      return;
    case "newtype":
      validateFixedArrayRef(payload.ref);
      return;
    case "tuple":
      payload.refs.forEach((ref) => validateFixedArrayRef(ref));
      return;
    case "struct":
      payload.fields.forEach((field) => validateFixedArrayRef(field.schema));
      return;
  }
}

function validateFixedArrayRef(ref: SchemaRef): void {
  if (ref.kind === "var") return;
  ref.args.forEach((arg) => validateFixedArrayRef(arg));
}

// r[impl validate.bundles]
export function validateSchemaBundle(schemas: readonly Schema[]): void {
  const recomputed = resolveIds(schemas);
  schemas.forEach((schema, index) => {
    const expected = recomputed[index]!.id;
    if (schema.id !== expected) {
      throw new DecodeError(`schema id mismatch: stated ${schema.id.toString(16)}, recomputed ${expected.toString(16)}`);
    }
  });

  const provided = new Set(schemas.map((schema) => schema.id));
  const primitives = new Set(PRIMITIVES.map((primitive) => primitiveId(primitive)));
  schemas.forEach((schema) => validateKindRefs(schema.kind, provided, primitives));

  const reg = new Registry(schemas);
  schemas.forEach((schema) => validateFixedArrayCaps(schema.kind, reg));
}

// ============================================================================
// Generic substitution (mirror of compact.rs substitute_kind/substitute_ref)
// ============================================================================

function substituteRef(ref: SchemaRef, params: string[], args: SchemaRef[]): SchemaRef {
  if (ref.kind === "var") {
    const i = params.indexOf(ref.name);
    return i >= 0 ? args[i]! : ref;
  }
  // A concrete ref keeps its id; substitute within its own type args so nested
  // parametric refs (`Holder<T>` inside `Wrapper<T>`) carry the binding forward.
  return { kind: "concrete", id: ref.id, args: ref.args.map((a) => substituteRef(a, params, args)) };
}

function substituteField(f: Field, params: string[], args: SchemaRef[]): Field {
  return { name: f.name, schema: substituteRef(f.schema, params, args), required: f.required };
}

function substitutePayload(p: VariantPayload, params: string[], args: SchemaRef[]): VariantPayload {
  switch (p.kind) {
    case "unit":
      return p;
    case "newtype":
      return { kind: "newtype", ref: substituteRef(p.ref, params, args) };
    case "tuple":
      return { kind: "tuple", refs: p.refs.map((r) => substituteRef(r, params, args)) };
    case "struct":
      return { kind: "struct", fields: p.fields.map((f) => substituteField(f, params, args)) };
  }
}

function substituteKind(kind: SchemaKind, params: string[], args: SchemaRef[]): SchemaKind {
  switch (kind.kind) {
    case "primitive":
    case "dynamic":
      return kind;
    case "struct":
      return { kind: "struct", name: kind.name, fields: kind.fields.map((f) => substituteField(f, params, args)) };
    case "enum":
      return {
        kind: "enum",
        name: kind.name,
        variants: kind.variants.map((v) => ({ name: v.name, index: v.index, payload: substitutePayload(v.payload, params, args) })),
      };
    case "tuple":
      return { kind: "tuple", elements: kind.elements.map((e) => substituteRef(e, params, args)) };
    case "list":
      return { kind: "list", element: substituteRef(kind.element, params, args) };
    case "set":
      return { kind: "set", element: substituteRef(kind.element, params, args) };
    case "option":
      return { kind: "option", element: substituteRef(kind.element, params, args) };
    case "map":
      return { kind: "map", key: substituteRef(kind.key, params, args), value: substituteRef(kind.value, params, args) };
    case "array":
      return { kind: "array", element: substituteRef(kind.element, params, args), dimensions: kind.dimensions };
    case "tensor":
      return { kind: "tensor", element: substituteRef(kind.element, params, args), rank: kind.rank };
    case "channel":
      return { kind: "channel", direction: kind.direction, element: substituteRef(kind.element, params, args) };
    case "external":
      return {
        kind: "external",
        external: kind.external,
        metadata: kind.metadata ? substituteRef(kind.metadata, params, args) : null,
      };
    case "semantic":
      return {
        kind: "semantic",
        name: kind.name,
        args: kind.args.map((arg) => substituteRef(arg, params, args)),
        representation: substituteRef(kind.representation, params, args),
      };
  }
}

// ============================================================================
// Self-describing schema parser (port of selfdescribing.rs dec_schema/...)
// ============================================================================
const SCHEMA_BUNDLE_MAGIC = new Uint8Array([0x50, 0x48, 0x4f, 0x4e, 0x53, 0x43, 0x4d, 0x31]);
const SCHEMA_BUNDLE_VERSION = 1;

/// Parse and admit a canonical `SchemaBundleEnvelope` (`PHONSCM1`, format 1).
// r[impl validate.bundles]
export function parseSchemaBundle(bytes: Uint8Array): Schema[] {
  const r = new Reader(bytes);
  const magic = r.readSlice(SCHEMA_BUNDLE_MAGIC.length);
  if (!magic.every((byte, index) => byte === SCHEMA_BUNDLE_MAGIC[index])) {
    throw new DecodeError("schema bundle magic");
  }
  if (r.readU32raw() !== SCHEMA_BUNDLE_VERSION) throw new DecodeError("schema bundle version");
  bundleStruct(r, "SchemaBundleV1", 2);
  bundleField(r, "strings");
  const strings = bundleList(r, 1, (reader) => bundleString(reader));
  for (let index = 0; index < strings.length; index++) {
    const current = strings[index]!;
    if (current.length === 0) throw new DecodeError("schema bundle string table");
    if (index > 0 && utf8Compare(strings[index - 1]!, current) >= 0) {
      throw new DecodeError("schema bundle string table");
    }
  }
  const used = new Set<number>();
  bundleField(r, "schemas");
  const schemas = bundleList(r, 1, (reader) => bundleSchema(reader, strings, used, 0));
  if (r.remaining() !== 0) throw new DecodeError(`${r.remaining()} trailing schema bundle bytes`);
  if (used.size !== strings.length) throw new DecodeError("unused schema bundle string");
  for (let index = 1; index < schemas.length; index++) {
    if (schemas[index - 1]!.id >= schemas[index]!.id) throw new DecodeError("schema bundle schema order");
  }
  validateSchemaBundle(schemas);
  return schemas;
}

function utf8Compare(left: string, right: string): number {
  const a = new TextEncoder().encode(left);
  const b = new TextEncoder().encode(right);
  const count = Math.min(a.length, b.length);
  for (let index = 0; index < count; index++) {
    if (a[index] !== b[index]) return a[index]! - b[index]!;
  }
  return a.length - b.length;
}

function bundleExpect(r: Reader, tag: number, what: string): void {
  const actual = r.readU8();
  if (actual !== tag) throw new DecodeError(`expected ${what}, got tag 0x${actual.toString(16)}`);
}

function bundleStruct(r: Reader, name: string, fields: number): void {
  bundleExpect(r, Tag.STRUCT, "struct");
  if (r.readStr() !== name || r.readU32raw() !== fields) throw new DecodeError("schema bundle struct shape");
}

function bundleField(r: Reader, name: string): void {
  if (r.readStr() !== name) throw new DecodeError("schema bundle field name");
}

function bundleList<T>(r: Reader, minElementSize: number, read: (reader: Reader) => T): T[] {
  bundleExpect(r, Tag.LIST, "list");
  const count = r.readLen(minElementSize);
  const values: T[] = [];
  for (let index = 0; index < count; index++) values.push(read(r));
  return values;
}

function bundleString(r: Reader): string {
  bundleExpect(r, Tag.STRING, "string");
  return r.readStr();
}

function bundleU32(r: Reader): number {
  bundleExpect(r, Tag.U32, "u32");
  return r.readU32raw();
}

function bundleU64(r: Reader): bigint {
  bundleExpect(r, Tag.U64, "u64");
  return r.readU64();
}

function bundleBool(r: Reader): boolean {
  bundleExpect(r, Tag.BOOL, "bool");
  return r.readBool();
}

function bundleUnit(r: Reader): void {
  bundleExpect(r, Tag.UNIT, "unit");
}

function bundleIndex(r: Reader, strings: readonly string[], used: Set<number>): string {
  const index = bundleU32(r);
  const value = strings[index];
  if (value === undefined) throw new DecodeError("schema bundle string index");
  used.add(index);
  return value;
}

function bundleSchema(r: Reader, strings: readonly string[], used: Set<number>, depth: number): Schema {
  checkDepth(depth);
  bundleStruct(r, "Schema", 3);
  bundleField(r, "id");
  const id = bundleU64(r);
  bundleField(r, "type_params");
  const typeParams = bundleList(r, 1, (reader) => bundleIndex(reader, strings, used));
  bundleField(r, "kind");
  return { id, typeParams, kind: bundleKind(r, strings, used, depth + 1) };
}

function bundleKind(r: Reader, strings: readonly string[], used: Set<number>, depth: number): SchemaKind {
  checkDepth(depth);
  bundleExpect(r, Tag.ENUM, "enum");
  const variant = r.readStr();
  switch (variant) {
    case "Primitive":
      bundleExpect(r, Tag.ENUM, "enum");
      return { kind: "primitive", primitive: bundlePrimitive(r) };
    case "Struct": {
      bundleStruct(r, "Struct", 2);
      bundleField(r, "name");
      const name = bundleIndex(r, strings, used);
      bundleField(r, "fields");
      return { kind: "struct", name, fields: bundleFields(r, strings, used, depth + 1) };
    }
    case "Enum": {
      bundleStruct(r, "Enum", 2);
      bundleField(r, "name");
      const name = bundleIndex(r, strings, used);
      bundleField(r, "variants");
      return { kind: "enum", name, variants: bundleList(r, 1, (reader) => bundleVariant(reader, strings, used, depth + 1)) };
    }
    case "Tuple":
      return { kind: "tuple", elements: bundleOneRefs(r, "Tuple", "elements", strings, used, depth + 1) };
    case "List":
      return { kind: "list", element: bundleOneRef(r, "List", "element", strings, used, depth + 1) };
    case "Set":
      return { kind: "set", element: bundleOneRef(r, "Set", "element", strings, used, depth + 1) };
    case "Option":
      return { kind: "option", element: bundleOneRef(r, "Option", "element", strings, used, depth + 1) };
    case "Map": {
      bundleStruct(r, "Map", 2);
      bundleField(r, "key");
      const key = bundleRef(r, strings, used, depth + 1);
      bundleField(r, "value");
      return { kind: "map", key, value: bundleRef(r, strings, used, depth + 1) };
    }
    case "Array": {
      bundleStruct(r, "Array", 2);
      bundleField(r, "element");
      const element = bundleRef(r, strings, used, depth + 1);
      bundleField(r, "dimensions");
      return { kind: "array", element, dimensions: bundleList(r, 1, bundleU64) };
    }
    case "Tensor": {
      bundleStruct(r, "Tensor", 2);
      bundleField(r, "element");
      const element = bundleRef(r, strings, used, depth + 1);
      bundleField(r, "rank");
      const tag = r.readU8();
      if (tag === Tag.OPTION_NONE) return { kind: "tensor", element, rank: null };
      if (tag === Tag.OPTION_SOME) return { kind: "tensor", element, rank: bundleU32(r) };
      throw new DecodeError("schema bundle tensor rank");
    }
    case "Channel": {
      bundleStruct(r, "Channel", 2);
      bundleField(r, "direction");
      bundleExpect(r, Tag.ENUM, "enum");
      const direction = r.readStr();
      bundleUnit(r);
      if (direction !== "tx" && direction !== "rx") throw new DecodeError("schema bundle channel direction");
      bundleField(r, "element");
      return { kind: "channel", direction, element: bundleRef(r, strings, used, depth + 1) };
    }
    case "Dynamic":
      bundleUnit(r);
      return { kind: "dynamic" };
    case "External": {
      bundleStruct(r, "External", 2);
      bundleField(r, "kind");
      const external = bundleIndex(r, strings, used);
      bundleField(r, "metadata");
      const tag = r.readU8();
      if (tag === Tag.OPTION_NONE) return { kind: "external", external, metadata: null };
      if (tag === Tag.OPTION_SOME) return { kind: "external", external, metadata: bundleRef(r, strings, used, depth + 1) };
      throw new DecodeError("schema bundle external metadata");
    }
    case "Semantic": {
      bundleStruct(r, "Semantic", 3);
      bundleField(r, "name");
      const name = bundleIndex(r, strings, used);
      bundleField(r, "args");
      const args = bundleRefs(r, strings, used, depth + 1);
      bundleField(r, "representation");
      return { kind: "semantic", name, args, representation: bundleRef(r, strings, used, depth + 1) };
    }
    default:
      throw new DecodeError(`unknown schema bundle kind '${variant}'`);
  }
}

function bundlePrimitive(r: Reader): Primitive {
  const name = r.readStr();
  bundleUnit(r);
  return asPrimitive(name);
}

function bundleOneRef(
  r: Reader,
  structName: string,
  fieldName: string,
  strings: readonly string[],
  used: Set<number>,
  depth: number,
): SchemaRef {
  bundleStruct(r, structName, 1);
  bundleField(r, fieldName);
  return bundleRef(r, strings, used, depth);
}

function bundleOneRefs(
  r: Reader,
  structName: string,
  fieldName: string,
  strings: readonly string[],
  used: Set<number>,
  depth: number,
): SchemaRef[] {
  bundleStruct(r, structName, 1);
  bundleField(r, fieldName);
  return bundleRefs(r, strings, used, depth);
}

function bundleRefs(r: Reader, strings: readonly string[], used: Set<number>, depth: number): SchemaRef[] {
  return bundleList(r, 1, (reader) => bundleRef(reader, strings, used, depth));
}

function bundleRef(r: Reader, strings: readonly string[], used: Set<number>, depth: number): SchemaRef {
  checkDepth(depth);
  bundleExpect(r, Tag.ENUM, "enum");
  const variant = r.readStr();
  if (variant === "Concrete") {
    bundleStruct(r, "Concrete", 2);
    bundleField(r, "id");
    const id = bundleU64(r);
    bundleField(r, "args");
    return { kind: "concrete", id, args: bundleRefs(r, strings, used, depth + 1) };
  }
  if (variant === "Var") {
    bundleStruct(r, "Var", 1);
    bundleField(r, "name");
    return { kind: "var", name: bundleIndex(r, strings, used) };
  }
  throw new DecodeError(`unknown schema bundle ref '${variant}'`);
}

function bundleFields(r: Reader, strings: readonly string[], used: Set<number>, depth: number): Field[] {
  return bundleList(r, 1, (reader) => {
    bundleStruct(reader, "Field", 3);
    bundleField(reader, "name");
    const name = bundleIndex(reader, strings, used);
    bundleField(reader, "schema");
    const schema = bundleRef(reader, strings, used, depth);
    bundleField(reader, "required");
    return { name, schema, required: bundleBool(reader) };
  });
}

function bundleVariant(r: Reader, strings: readonly string[], used: Set<number>, depth: number): Variant {
  bundleStruct(r, "Variant", 3);
  bundleField(r, "name");
  const name = bundleIndex(r, strings, used);
  bundleField(r, "index");
  const index = bundleU32(r);
  bundleField(r, "payload");
  bundleExpect(r, Tag.ENUM, "enum");
  const variant = r.readStr();
  let payload: VariantPayload;
  switch (variant) {
    case "Unit":
      bundleUnit(r);
      payload = { kind: "unit" };
      break;
    case "Newtype":
      payload = { kind: "newtype", ref: bundleRef(r, strings, used, depth) };
      break;
    case "Tuple":
      payload = { kind: "tuple", refs: bundleRefs(r, strings, used, depth) };
      break;
    case "Struct":
      payload = { kind: "struct", fields: bundleFields(r, strings, used, depth) };
      break;
    default:
      throw new DecodeError(`unknown schema bundle payload '${variant}'`);
  }
  return { name, index, payload };
}


/// Parse a `Schema` from self-describing bytes (the bytes Rust `schema_to_bytes`
/// produces). Rejects trailing bytes. Throws `DecodeError` on malformed input.
// r[impl self-describing.bootstraps-schemas]
export function schemaFromBytes(bytes: Uint8Array): Schema {
  const r = new Reader(bytes);
  const s = decSchema(r, 0);
  if (r.remaining() !== 0) {
    throw new DecodeError(`${r.remaining()} trailing bytes after schema`);
  }
  return s;
}

function checkDepth(depth: number): void {
  if (depth > MAX_DEPTH) throw new DecodeError("schema nests too deep");
}

// The schema self-describing form is a tagged value tree: enums are
// `ENUM`-tag + variant-name string + payload; structs are `STRUCT`-tag + name +
// field count + (name string, value)*. The decoder reads that framing exactly.
// r[impl self-describing.enum-payload]

function expect(r: Reader, t: number, what: string): void {
  const got = r.readU8();
  if (got !== t) throw new DecodeError(`expected ${what}, got tag 0x${got.toString(16)}`);
}

function dU32(r: Reader): number {
  expect(r, Tag.U32, "u32");
  return r.readU32raw();
}

function dU64(r: Reader): bigint {
  expect(r, Tag.U64, "u64");
  return r.readU64();
}

function dBool(r: Reader): boolean {
  expect(r, Tag.BOOL, "bool");
  return r.readBool();
}

function dStr(r: Reader): string {
  expect(r, Tag.STRING, "string");
  return r.readStr();
}

function dUnit(r: Reader): void {
  expect(r, Tag.UNIT, "unit");
}

/// Read a struct header (tag, name, field count), verifying the count.
function stBegin(r: Reader, fields: number): void {
  expect(r, Tag.STRUCT, "struct");
  r.readStr(); // struct name (informational)
  if (r.readU32raw() !== fields) throw new DecodeError("struct field count");
}

/// Read and discard a struct field's name (fields are positional here).
function fname(r: Reader): void {
  r.readStr();
}

function listLen(r: Reader): number {
  expect(r, Tag.LIST, "list");
  return r.readLen(1);
}

function decSchema(r: Reader, depth: number): Schema {
  checkDepth(depth);
  stBegin(r, 3);
  fname(r);
  const id = dU64(r);
  fname(r);
  const n = listLen(r);
  const typeParams: string[] = [];
  for (let i = 0; i < n; i++) typeParams.push(dStr(r));
  fname(r);
  const kind = decKind(r, depth + 1);
  return { id, typeParams, kind };
}

function decKind(r: Reader, depth: number): SchemaKind {
  checkDepth(depth);
  // r[impl self-describing.enum-payload]
  expect(r, Tag.ENUM, "enum");
  const variant = r.readStr();
  switch (variant) {
    case "Primitive":
      return { kind: "primitive", primitive: decPrimitive(r) };
    case "Struct": {
      stBegin(r, 2);
      fname(r);
      const name = dStr(r);
      fname(r);
      const fields = decFieldList(r, depth + 1);
      return { kind: "struct", name, fields };
    }
    case "Enum": {
      stBegin(r, 2);
      fname(r);
      const name = dStr(r);
      fname(r);
      const count = listLen(r);
      const variants: Variant[] = [];
      for (let i = 0; i < count; i++) variants.push(decVariant(r, depth + 1));
      return { kind: "enum", name, variants };
    }
    case "Tuple": {
      stBegin(r, 1);
      fname(r);
      return { kind: "tuple", elements: decRefList(r, depth + 1) };
    }
    case "List": {
      stBegin(r, 1);
      fname(r);
      return { kind: "list", element: decRef(r, depth + 1) };
    }
    case "Set": {
      stBegin(r, 1);
      fname(r);
      return { kind: "set", element: decRef(r, depth + 1) };
    }
    case "Option": {
      stBegin(r, 1);
      fname(r);
      return { kind: "option", element: decRef(r, depth + 1) };
    }
    case "Map": {
      stBegin(r, 2);
      fname(r);
      const key = decRef(r, depth + 1);
      fname(r);
      const value = decRef(r, depth + 1);
      return { kind: "map", key, value };
    }
    case "Array": {
      stBegin(r, 2);
      fname(r);
      const element = decRef(r, depth + 1);
      fname(r);
      const count = listLen(r);
      const dimensions: bigint[] = [];
      for (let i = 0; i < count; i++) dimensions.push(dU64(r));
      return { kind: "array", element, dimensions };
    }
    case "Tensor": {
      stBegin(r, 2);
      fname(r);
      const element = decRef(r, depth + 1);
      fname(r);
      const t = r.readU8();
      let rank: number | null;
      if (t === Tag.OPTION_NONE) rank = null;
      else if (t === Tag.OPTION_SOME) rank = dU32(r);
      else throw new DecodeError(`expected option, got tag 0x${t.toString(16)}`);
      return { kind: "tensor", element, rank };
    }
    case "Channel": {
      stBegin(r, 2);
      fname(r);
      const direction = decDirection(r);
      fname(r);
      const element = decRef(r, depth + 1);
      return { kind: "channel", direction, element };
    }
    case "Dynamic": {
      dUnit(r);
      return { kind: "dynamic" };
    }
    case "External": {
      stBegin(r, 2);
      fname(r);
      const external = dStr(r);
      fname(r);
      const t = r.readU8();
      let metadata: SchemaRef | null;
      if (t === Tag.OPTION_NONE) metadata = null;
      else if (t === Tag.OPTION_SOME) metadata = decRef(r, depth + 1);
      else throw new DecodeError(`expected option, got tag 0x${t.toString(16)}`);
      return { kind: "external", external, metadata };
    }
    case "Semantic": {
      stBegin(r, 3);
      fname(r);
      const name = dStr(r);
      fname(r);
      const args = decRefList(r, depth + 1);
      fname(r);
      const representation = decRef(r, depth + 1);
      return { kind: "semantic", name, args, representation };
    }
    default:
      throw new DecodeError(`unknown SchemaKind variant '${variant}'`);
  }
}

function decPrimitive(r: Reader): Primitive {
  // r[impl self-describing.enum-payload]
  expect(r, Tag.ENUM, "enum");
  const name = r.readStr();
  dUnit(r);
  return asPrimitive(name);
}

function decDirection(r: Reader): ChannelDirection {
  // r[impl self-describing.enum-payload]
  expect(r, Tag.ENUM, "enum");
  const name = r.readStr();
  dUnit(r);
  if (name === "tx" || name === "rx") return name;
  throw new DecodeError(`unknown channel direction '${name}'`);
}

function decRef(r: Reader, depth: number): SchemaRef {
  checkDepth(depth);
  // r[impl self-describing.enum-payload]
  expect(r, Tag.ENUM, "enum");
  const variant = r.readStr();
  switch (variant) {
    case "Concrete": {
      stBegin(r, 2);
      fname(r);
      const id = dU64(r);
      fname(r);
      const args = decRefList(r, depth + 1);
      return { kind: "concrete", id, args };
    }
    case "Var": {
      stBegin(r, 1);
      fname(r);
      return { kind: "var", name: dStr(r) };
    }
    default:
      throw new DecodeError(`unknown SchemaRef variant '${variant}'`);
  }
}

function decField(r: Reader, depth: number): Field {
  checkDepth(depth);
  stBegin(r, 3);
  fname(r);
  const name = dStr(r);
  fname(r);
  const schema = decRef(r, depth + 1);
  fname(r);
  const required = dBool(r);
  return { name, schema, required };
}

function decVariant(r: Reader, depth: number): Variant {
  checkDepth(depth);
  stBegin(r, 3);
  fname(r);
  const name = dStr(r);
  fname(r);
  const index = dU32(r);
  fname(r);
  const payload = decVariantPayload(r, depth + 1);
  return { name, index, payload };
}

function decVariantPayload(r: Reader, depth: number): VariantPayload {
  checkDepth(depth);
  // r[impl self-describing.enum-payload]
  expect(r, Tag.ENUM, "enum");
  const variant = r.readStr();
  switch (variant) {
    case "Unit":
      dUnit(r);
      return { kind: "unit" };
    case "Newtype":
      return { kind: "newtype", ref: decRef(r, depth + 1) };
    case "Tuple":
      return { kind: "tuple", refs: decRefList(r, depth + 1) };
    case "Struct":
      return { kind: "struct", fields: decFieldList(r, depth + 1) };
    default:
      throw new DecodeError(`unknown VariantPayload variant '${variant}'`);
  }
}

function decRefList(r: Reader, depth: number): SchemaRef[] {
  const n = listLen(r);
  const v: SchemaRef[] = [];
  for (let i = 0; i < n; i++) v.push(decRef(r, depth + 1));
  return v;
}

function decFieldList(r: Reader, depth: number): Field[] {
  const n = listLen(r);
  const v: Field[] = [];
  for (let i = 0; i < n; i++) v.push(decField(r, depth + 1));
  return v;
}
