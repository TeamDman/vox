use std::collections::{BTreeSet, HashSet};

use taxon::SemanticName;

use crate::bytes::{
    DecodeError, Reader, Sink, write_bool, write_str, write_u8, write_u32, write_u64,
};
use crate::{
    ChannelDirection, Field, Primitive, QualifiedName, Schema, SchemaId, SchemaKind, SchemaRef,
    Variant, VariantPayload, resolve_ids,
};

const MAGIC: &[u8; 8] = b"PHONSCM1";
const VERSION: u32 = 1;
const TAG_UNIT: u8 = 0x00;
const TAG_BOOL: u8 = 0x01;
const TAG_U32: u8 = 0x04;
const TAG_U64: u8 = 0x05;
const TAG_STRING: u8 = 0x0f;
const TAG_LIST: u8 = 0x11;
const TAG_STRUCT: u8 = 0x16;
const TAG_ENUM: u8 = 0x17;
const TAG_NONE: u8 = 0x18;
const TAG_SOME: u8 = 0x19;

#[derive(Clone, Copy, Debug)]
pub struct SchemaBundleLimits {
    pub max_total_bytes: usize,
    pub max_owned_bytes: usize,
    pub max_strings: usize,
    pub max_schemas: usize,
    pub max_nesting: usize,
}
impl Default for SchemaBundleLimits {
    fn default() -> Self {
        Self {
            max_total_bytes: 64 * 1024 * 1024,
            max_owned_bytes: 16 * 1024 * 1024,
            max_strings: 1 << 20,
            max_schemas: 1 << 20,
            max_nesting: 128,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaBundle {
    schemas: Vec<Schema>,
}
impl SchemaBundle {
    #[must_use]
    pub fn schemas(&self) -> &[Schema] {
        &self.schemas
    }
}

pub fn schema_bundle_to_bytes(schemas: &[Schema]) -> Result<Vec<u8>, DecodeError> {
    let mut schemas = schemas.to_vec();
    schemas.sort_by_key(|schema| schema.id);
    if schemas.windows(2).any(|pair| pair[0].id == pair[1].id) {
        return Err(DecodeError::Malformed("duplicate schema id"));
    }
    validate_ids(&schemas)?;
    let mut strings = BTreeSet::new();
    for schema in &schemas {
        collect_schema(schema, &mut strings);
    }
    let strings: Vec<String> = strings.into_iter().collect();
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    write_u32(&mut out, VERSION);
    st(&mut out, "SchemaBundleV1", 2);
    write_str(&mut out, "strings");
    list(&mut out, strings.len());
    for string in &strings {
        val_str(&mut out, string);
    }
    write_str(&mut out, "schemas");
    list(&mut out, schemas.len());
    for schema in &schemas {
        enc_schema(&mut out, schema, &strings)?;
    }
    Ok(out)
}

pub fn schema_bundle_from_bytes(
    bytes: &[u8],
    limits: SchemaBundleLimits,
) -> Result<SchemaBundle, DecodeError> {
    if bytes.len() > limits.max_total_bytes {
        return Err(DecodeError::OwnedBytesLimitExceeded {
            configured: limits.max_total_bytes,
            attempted: bytes.len(),
        });
    }
    let mut d = Dec {
        r: Reader::new(bytes),
        limits,
        owned: 0,
    };
    if d.r.read_slice(MAGIC.len())? != MAGIC {
        return Err(DecodeError::Malformed("schema bundle magic"));
    }
    if d.r.read_u32()? != VERSION {
        return Err(DecodeError::Malformed("schema bundle version"));
    }
    d.st("SchemaBundleV1", 2)?;
    d.field("strings")?;
    let string_count = d.list_len()?;
    if string_count > limits.max_strings {
        return Err(DecodeError::OwnedBytesLimitExceeded {
            configured: limits.max_strings,
            attempted: string_count,
        });
    }
    let mut strings = Vec::new();
    strings
        .try_reserve_exact(string_count)
        .map_err(|_| DecodeError::AllocationFailed)?;
    for _ in 0..string_count {
        strings.push(d.string()?);
    }
    if strings.iter().any(String::is_empty)
        || strings
            .windows(2)
            .any(|pair| pair[0].as_bytes() >= pair[1].as_bytes())
    {
        return Err(DecodeError::Malformed("schema bundle string table"));
    }
    d.field("schemas")?;
    let schema_count = d.list_len()?;
    if schema_count > limits.max_schemas {
        return Err(DecodeError::OwnedBytesLimitExceeded {
            configured: limits.max_schemas,
            attempted: schema_count,
        });
    }
    let mut used = HashSet::new();
    let mut schemas = Vec::new();
    schemas
        .try_reserve_exact(schema_count)
        .map_err(|_| DecodeError::AllocationFailed)?;
    for _ in 0..schema_count {
        schemas.push(d.schema(&strings, &mut used, 0)?);
    }
    if d.r.remaining() != 0 {
        return Err(DecodeError::TrailingBytes(d.r.remaining()));
    }
    if used.len() != strings.len() {
        return Err(DecodeError::Malformed("unused schema bundle string"));
    }
    if schemas.windows(2).any(|pair| pair[0].id >= pair[1].id) {
        return Err(DecodeError::Malformed("schema bundle schema order"));
    }
    validate_ids(&schemas)?;
    Ok(SchemaBundle { schemas })
}

fn validate_ids(schemas: &[Schema]) -> Result<(), DecodeError> {
    validate_schema_shapes(schemas)?;
    let actual = resolve_ids(schemas.to_vec());
    if schemas
        .iter()
        .zip(actual)
        .any(|(stated, actual)| stated.id != actual.id)
    {
        Err(DecodeError::Malformed("schema id mismatch"))
    } else {
        Ok(())
    }
}
fn validate_schema_shapes(schemas: &[Schema]) -> Result<(), DecodeError> {
    let arities: std::collections::HashMap<SchemaId, usize> = schemas
        .iter()
        .map(|schema| (schema.id, schema.type_params.len()))
        .collect();
    for schema in schemas {
        let params: HashSet<&str> = schema.type_params.iter().map(String::as_str).collect();
        if params.len() != schema.type_params.len() || params.iter().any(|name| name.is_empty()) {
            return Err(DecodeError::Malformed("schema type parameters"));
        }
        validate_kind(&schema.kind, &params, &arities)?;
    }
    Ok(())
}
fn validate_kind(
    kind: &SchemaKind,
    params: &HashSet<&str>,
    arities: &std::collections::HashMap<SchemaId, usize>,
) -> Result<(), DecodeError> {
    match kind {
        SchemaKind::Primitive(_) | SchemaKind::Dynamic => {}
        SchemaKind::Struct { fields, .. } => {
            for field in fields {
                validate_ref(&field.schema, params, arities)?;
            }
        }
        SchemaKind::Enum { variants, .. } => {
            for variant in variants {
                match &variant.payload {
                    VariantPayload::Unit => {}
                    VariantPayload::Newtype(reference) => validate_ref(reference, params, arities)?,
                    VariantPayload::Tuple(references) => {
                        for reference in references {
                            validate_ref(reference, params, arities)?;
                        }
                    }
                    VariantPayload::Struct(fields) => {
                        for field in fields {
                            validate_ref(&field.schema, params, arities)?;
                        }
                    }
                }
            }
        }
        SchemaKind::Tuple { elements } => {
            for element in elements {
                validate_ref(element, params, arities)?;
            }
        }
        SchemaKind::List { element }
        | SchemaKind::Set { element }
        | SchemaKind::Option { element }
        | SchemaKind::Array { element, .. }
        | SchemaKind::Tensor { element, .. }
        | SchemaKind::Channel { element, .. } => validate_ref(element, params, arities)?,
        SchemaKind::Map { key, value } => {
            validate_ref(key, params, arities)?;
            validate_ref(value, params, arities)?;
        }
        SchemaKind::External { metadata, .. } => {
            if let Some(metadata) = metadata {
                validate_ref(metadata, params, arities)?;
            }
        }
        SchemaKind::Semantic {
            name,
            args,
            representation,
        } => {
            QualifiedName::try_from(name.clone())
                .map_err(|_| DecodeError::Malformed("qualified name"))?;
            for argument in args {
                validate_ref(argument, params, arities)?;
            }
            validate_ref(representation, params, arities)?;
        }
    }
    Ok(())
}
fn validate_ref(
    reference: &SchemaRef,
    params: &HashSet<&str>,
    arities: &std::collections::HashMap<SchemaId, usize>,
) -> Result<(), DecodeError> {
    match reference {
        SchemaRef::Var { name } if params.contains(name.as_str()) => Ok(()),
        SchemaRef::Var { .. } => Err(DecodeError::Malformed("undeclared schema variable")),
        SchemaRef::Concrete { id, args } => {
            let primitive = Primitive::ALL
                .iter()
                .any(|primitive| crate::primitive_id(*primitive) == *id);
            match (primitive, arities.get(id).copied()) {
                (true, _) if !args.is_empty() => {
                    return Err(DecodeError::Malformed("primitive schema arguments"));
                }
                (false, Some(expected)) if args.len() != expected => {
                    return Err(DecodeError::Malformed("schema argument arity"));
                }
                (false, None) => return Err(DecodeError::Malformed("missing schema reference")),
                _ => {}
            }
            for argument in args {
                validate_ref(argument, params, arities)?;
            }
            Ok(())
        }
    }
}
fn insert(value: &str, strings: &mut BTreeSet<String>) {
    if !value.is_empty() {
        strings.insert(value.to_string());
    }
}
fn collect_schema(schema: &Schema, strings: &mut BTreeSet<String>) {
    for p in &schema.type_params {
        insert(p, strings);
    }
    collect_kind(&schema.kind, strings);
}
fn collect_field(field: &Field, strings: &mut BTreeSet<String>) {
    insert(&field.name, strings);
    collect_ref(&field.schema, strings);
}
fn collect_ref(r: &SchemaRef, strings: &mut BTreeSet<String>) {
    match r {
        SchemaRef::Var { name } => insert(name, strings),
        SchemaRef::Concrete { args, .. } => {
            for a in args {
                collect_ref(a, strings)
            }
        }
    }
}
fn collect_kind(kind: &SchemaKind, strings: &mut BTreeSet<String>) {
    match kind {
        SchemaKind::Primitive(_) | SchemaKind::Dynamic => {}
        SchemaKind::Struct { name, fields } => {
            insert(name, strings);
            for f in fields {
                collect_field(f, strings);
            }
        }
        SchemaKind::Enum { name, variants } => {
            insert(name, strings);
            for v in variants {
                insert(&v.name, strings);
                match &v.payload {
                    VariantPayload::Unit => {}
                    VariantPayload::Newtype(r) => collect_ref(r, strings),
                    VariantPayload::Tuple(rs) => {
                        for r in rs {
                            collect_ref(r, strings)
                        }
                    }
                    VariantPayload::Struct(fs) => {
                        for f in fs {
                            collect_field(f, strings)
                        }
                    }
                }
            }
        }
        SchemaKind::Tuple { elements } => {
            for r in elements {
                collect_ref(r, strings)
            }
        }
        SchemaKind::List { element }
        | SchemaKind::Set { element }
        | SchemaKind::Option { element }
        | SchemaKind::Array { element, .. }
        | SchemaKind::Tensor { element, .. }
        | SchemaKind::Channel { element, .. } => collect_ref(element, strings),
        SchemaKind::Map { key, value } => {
            collect_ref(key, strings);
            collect_ref(value, strings);
        }
        SchemaKind::External { kind, metadata } => {
            insert(kind, strings);
            if let Some(r) = metadata {
                collect_ref(r, strings);
            }
        }
        SchemaKind::Semantic {
            name,
            args,
            representation,
        } => {
            insert(name.as_str(), strings);
            for a in args {
                collect_ref(a, strings)
            }
            collect_ref(representation, strings);
        }
    }
}
fn idx(strings: &[String], value: &str) -> Result<u32, DecodeError> {
    strings
        .binary_search_by(|s| s.as_bytes().cmp(value.as_bytes()))
        .map(|i| i as u32)
        .map_err(|_| DecodeError::Malformed("missing interned string"))
}

fn enc_schema<S: Sink>(o: &mut S, s: &Schema, strings: &[String]) -> Result<(), DecodeError> {
    st(o, "Schema", 3);
    write_str(o, "id");
    v64(o, s.id.as_u64());
    write_str(o, "type_params");
    list(o, s.type_params.len());
    for p in &s.type_params {
        v32(o, idx(strings, p)?);
    }
    write_str(o, "kind");
    enc_kind(o, &s.kind, strings)
}
fn enc_kind<S: Sink>(o: &mut S, k: &SchemaKind, strings: &[String]) -> Result<(), DecodeError> {
    write_u8(o, TAG_ENUM);
    match k {
        SchemaKind::Primitive(p) => {
            write_str(o, "Primitive");
            write_u8(o, TAG_ENUM);
            write_str(o, p.tag());
            unit(o)
        }
        SchemaKind::Struct { name, fields } => {
            write_str(o, "Struct");
            st(o, "Struct", 2);
            write_str(o, "name");
            v32(o, idx(strings, name)?);
            write_str(o, "fields");
            enc_fields(o, fields, strings)?
        }
        SchemaKind::Enum { name, variants } => {
            write_str(o, "Enum");
            st(o, "Enum", 2);
            write_str(o, "name");
            v32(o, idx(strings, name)?);
            write_str(o, "variants");
            list(o, variants.len());
            for v in variants {
                enc_variant(o, v, strings)?
            }
        }
        SchemaKind::Tuple { elements } => one_refs(o, "Tuple", "elements", elements, strings)?,
        SchemaKind::List { element } => one_ref(o, "List", "element", element, strings)?,
        SchemaKind::Set { element } => one_ref(o, "Set", "element", element, strings)?,
        SchemaKind::Option { element } => one_ref(o, "Option", "element", element, strings)?,
        SchemaKind::Map { key, value } => {
            write_str(o, "Map");
            st(o, "Map", 2);
            write_str(o, "key");
            enc_ref(o, key, strings)?;
            write_str(o, "value");
            enc_ref(o, value, strings)?
        }
        SchemaKind::Array {
            element,
            dimensions,
        } => {
            write_str(o, "Array");
            st(o, "Array", 2);
            write_str(o, "element");
            enc_ref(o, element, strings)?;
            write_str(o, "dimensions");
            list(o, dimensions.len());
            for d in dimensions {
                v64(o, *d)
            }
        }
        SchemaKind::Tensor { element, rank } => {
            write_str(o, "Tensor");
            st(o, "Tensor", 2);
            write_str(o, "element");
            enc_ref(o, element, strings)?;
            write_str(o, "rank");
            match rank {
                None => write_u8(o, TAG_NONE),
                Some(r) => {
                    write_u8(o, TAG_SOME);
                    v32(o, *r)
                }
            }
        }
        SchemaKind::Channel { direction, element } => {
            write_str(o, "Channel");
            st(o, "Channel", 2);
            write_str(o, "direction");
            write_u8(o, TAG_ENUM);
            write_str(
                o,
                match direction {
                    ChannelDirection::Tx => "tx",
                    ChannelDirection::Rx => "rx",
                },
            );
            unit(o);
            write_str(o, "element");
            enc_ref(o, element, strings)?
        }
        SchemaKind::Dynamic => {
            write_str(o, "Dynamic");
            unit(o)
        }
        SchemaKind::External { kind, metadata } => {
            write_str(o, "External");
            st(o, "External", 2);
            write_str(o, "kind");
            v32(o, idx(strings, kind)?);
            write_str(o, "metadata");
            match metadata {
                None => write_u8(o, TAG_NONE),
                Some(r) => {
                    write_u8(o, TAG_SOME);
                    enc_ref(o, r, strings)?
                }
            }
        }
        SchemaKind::Semantic {
            name,
            args,
            representation,
        } => {
            QualifiedName::try_from(name.clone())
                .map_err(|_| DecodeError::Malformed("qualified name"))?;
            write_str(o, "Semantic");
            st(o, "Semantic", 3);
            write_str(o, "name");
            v32(o, idx(strings, name.as_str())?);
            write_str(o, "args");
            enc_refs(o, args, strings)?;
            write_str(o, "representation");
            enc_ref(o, representation, strings)?
        }
    }
    Ok(())
}
fn one_ref<S: Sink>(
    o: &mut S,
    v: &str,
    f: &str,
    r: &SchemaRef,
    s: &[String],
) -> Result<(), DecodeError> {
    write_str(o, v);
    st(o, v, 1);
    write_str(o, f);
    enc_ref(o, r, s)
}
fn one_refs<S: Sink>(
    o: &mut S,
    v: &str,
    f: &str,
    rs: &[SchemaRef],
    s: &[String],
) -> Result<(), DecodeError> {
    write_str(o, v);
    st(o, v, 1);
    write_str(o, f);
    enc_refs(o, rs, s)
}
fn enc_refs<S: Sink>(o: &mut S, rs: &[SchemaRef], s: &[String]) -> Result<(), DecodeError> {
    list(o, rs.len());
    for r in rs {
        enc_ref(o, r, s)?
    }
    Ok(())
}
fn enc_ref<S: Sink>(o: &mut S, r: &SchemaRef, s: &[String]) -> Result<(), DecodeError> {
    write_u8(o, TAG_ENUM);
    match r {
        SchemaRef::Concrete { id, args } => {
            write_str(o, "Concrete");
            st(o, "Concrete", 2);
            write_str(o, "id");
            v64(o, id.as_u64());
            write_str(o, "args");
            enc_refs(o, args, s)?
        }
        SchemaRef::Var { name } => {
            write_str(o, "Var");
            st(o, "Var", 1);
            write_str(o, "name");
            v32(o, idx(s, name)?);
        }
    }
    Ok(())
}
fn enc_fields<S: Sink>(o: &mut S, fs: &[Field], s: &[String]) -> Result<(), DecodeError> {
    list(o, fs.len());
    for f in fs {
        st(o, "Field", 3);
        write_str(o, "name");
        v32(o, idx(s, &f.name)?);
        write_str(o, "schema");
        enc_ref(o, &f.schema, s)?;
        write_str(o, "required");
        vb(o, f.required)
    }
    Ok(())
}
fn enc_variant<S: Sink>(o: &mut S, v: &Variant, s: &[String]) -> Result<(), DecodeError> {
    st(o, "Variant", 3);
    write_str(o, "name");
    v32(o, idx(s, &v.name)?);
    write_str(o, "index");
    v32(o, v.index);
    write_str(o, "payload");
    write_u8(o, TAG_ENUM);
    match &v.payload {
        VariantPayload::Unit => {
            write_str(o, "Unit");
            unit(o)
        }
        VariantPayload::Newtype(r) => {
            write_str(o, "Newtype");
            enc_ref(o, r, s)?
        }
        VariantPayload::Tuple(rs) => {
            write_str(o, "Tuple");
            enc_refs(o, rs, s)?
        }
        VariantPayload::Struct(fs) => {
            write_str(o, "Struct");
            enc_fields(o, fs, s)?
        }
    }
    Ok(())
}
fn st<S: Sink>(o: &mut S, n: &str, f: u32) {
    write_u8(o, TAG_STRUCT);
    write_str(o, n);
    write_u32(o, f)
}
fn list<S: Sink>(o: &mut S, n: usize) {
    write_u8(o, TAG_LIST);
    write_u32(o, n as u32)
}
fn unit<S: Sink>(o: &mut S) {
    write_u8(o, TAG_UNIT)
}
fn vb<S: Sink>(o: &mut S, v: bool) {
    write_u8(o, TAG_BOOL);
    write_bool(o, v)
}
fn v32<S: Sink>(o: &mut S, v: u32) {
    write_u8(o, TAG_U32);
    write_u32(o, v)
}
fn v64<S: Sink>(o: &mut S, v: u64) {
    write_u8(o, TAG_U64);
    write_u64(o, v)
}
fn val_str<S: Sink>(o: &mut S, v: &str) {
    write_u8(o, TAG_STRING);
    write_str(o, v)
}

struct Dec<'a> {
    r: Reader<'a>,
    limits: SchemaBundleLimits,
    owned: usize,
}
impl<'a> Dec<'a> {
    fn expect(&mut self, t: u8, n: &'static str) -> Result<(), DecodeError> {
        let g = self.r.read_u8()?;
        if g == t {
            Ok(())
        } else {
            Err(DecodeError::UnexpectedTag {
                expected: n,
                got: g,
            })
        }
    }
    fn st(&mut self, n: &str, f: u32) -> Result<(), DecodeError> {
        self.expect(TAG_STRUCT, "struct")?;
        if self.r.read_str() != Ok(n) || self.r.read_u32()? != f {
            Err(DecodeError::Malformed("struct shape"))
        } else {
            Ok(())
        }
    }
    fn field(&mut self, n: &str) -> Result<(), DecodeError> {
        if self.r.read_str()? == n {
            Ok(())
        } else {
            Err(DecodeError::Malformed("field name"))
        }
    }
    fn list_len(&mut self) -> Result<usize, DecodeError> {
        self.expect(TAG_LIST, "list")?;
        self.r.read_len(1)
    }
    fn u32(&mut self) -> Result<u32, DecodeError> {
        self.expect(TAG_U32, "u32")?;
        self.r.read_u32()
    }
    fn u64(&mut self) -> Result<u64, DecodeError> {
        self.expect(TAG_U64, "u64")?;
        self.r.read_u64()
    }
    fn bool(&mut self) -> Result<bool, DecodeError> {
        self.expect(TAG_BOOL, "bool")?;
        self.r.read_bool()
    }
    fn unit(&mut self) -> Result<(), DecodeError> {
        self.expect(TAG_UNIT, "unit")
    }
    fn string(&mut self) -> Result<String, DecodeError> {
        self.expect(TAG_STRING, "string")?;
        let v = self.r.read_str()?;
        self.owned = self
            .owned
            .checked_add(v.len())
            .ok_or(DecodeError::AllocationFailed)?;
        if self.owned > self.limits.max_owned_bytes {
            return Err(DecodeError::OwnedBytesLimitExceeded {
                configured: self.limits.max_owned_bytes,
                attempted: self.owned,
            });
        }
        Ok(v.to_string())
    }
    fn ix(&mut self, s: &[String], u: &mut HashSet<usize>) -> Result<String, DecodeError> {
        let i = self.u32()? as usize;
        let v = s.get(i).ok_or(DecodeError::Malformed("string index"))?;
        u.insert(i);
        Ok(v.clone())
    }
    fn schema(
        &mut self,
        s: &[String],
        u: &mut HashSet<usize>,
        d: usize,
    ) -> Result<Schema, DecodeError> {
        if d > self.limits.max_nesting {
            return Err(DecodeError::DepthExceeded);
        }
        self.st("Schema", 3)?;
        self.field("id")?;
        let id = SchemaId::from_raw(self.u64()?);
        self.field("type_params")?;
        let n = self.list_len()?;
        let mut ps = Vec::with_capacity(n);
        for _ in 0..n {
            ps.push(self.ix(s, u)?)
        }
        self.field("kind")?;
        Ok(Schema {
            id,
            type_params: ps,
            kind: self.kind(s, u, d + 1)?,
        })
    }
    fn kind(
        &mut self,
        s: &[String],
        u: &mut HashSet<usize>,
        d: usize,
    ) -> Result<SchemaKind, DecodeError> {
        if d > self.limits.max_nesting {
            return Err(DecodeError::DepthExceeded);
        }
        self.expect(TAG_ENUM, "enum")?;
        Ok(match self.r.read_str()? {
            "Primitive" => {
                self.expect(TAG_ENUM, "enum")?;
                let n = self.r.read_str()?;
                self.unit()?;
                SchemaKind::Primitive(
                    Primitive::from_tag(n).ok_or(DecodeError::Malformed("primitive"))?,
                )
            }
            "Struct" => {
                self.st("Struct", 2)?;
                self.field("name")?;
                let n = self.ix(s, u)?;
                self.field("fields")?;
                SchemaKind::Struct {
                    name: n,
                    fields: self.fields(s, u, d + 1)?,
                }
            }
            "Enum" => {
                self.st("Enum", 2)?;
                self.field("name")?;
                let n = self.ix(s, u)?;
                self.field("variants")?;
                let c = self.list_len()?;
                let mut vs = Vec::with_capacity(c);
                for _ in 0..c {
                    vs.push(self.variant(s, u, d + 1)?)
                }
                SchemaKind::Enum {
                    name: n,
                    variants: vs,
                }
            }
            "Tuple" => {
                self.st("Tuple", 1)?;
                self.field("elements")?;
                SchemaKind::Tuple {
                    elements: self.refs(s, u, d + 1)?,
                }
            }
            "List" => SchemaKind::List {
                element: self.one("List", "element", s, u, d + 1)?,
            },
            "Set" => SchemaKind::Set {
                element: self.one("Set", "element", s, u, d + 1)?,
            },
            "Option" => SchemaKind::Option {
                element: self.one("Option", "element", s, u, d + 1)?,
            },
            "Map" => {
                self.st("Map", 2)?;
                self.field("key")?;
                let k = self.reference(s, u, d + 1)?;
                self.field("value")?;
                SchemaKind::Map {
                    key: k,
                    value: self.reference(s, u, d + 1)?,
                }
            }
            "Array" => {
                self.st("Array", 2)?;
                self.field("element")?;
                let e = self.reference(s, u, d + 1)?;
                self.field("dimensions")?;
                let c = self.list_len()?;
                let mut ds = Vec::with_capacity(c);
                for _ in 0..c {
                    ds.push(self.u64()?)
                }
                SchemaKind::Array {
                    element: e,
                    dimensions: ds,
                }
            }
            "Tensor" => {
                self.st("Tensor", 2)?;
                self.field("element")?;
                let e = self.reference(s, u, d + 1)?;
                self.field("rank")?;
                let r = match self.r.read_u8()? {
                    TAG_NONE => None,
                    TAG_SOME => Some(self.u32()?),
                    g => {
                        return Err(DecodeError::UnexpectedTag {
                            expected: "option",
                            got: g,
                        });
                    }
                };
                SchemaKind::Tensor {
                    element: e,
                    rank: r,
                }
            }
            "Channel" => {
                self.st("Channel", 2)?;
                self.field("direction")?;
                self.expect(TAG_ENUM, "enum")?;
                let dir = match self.r.read_str()? {
                    "tx" => ChannelDirection::Tx,
                    "rx" => ChannelDirection::Rx,
                    _ => return Err(DecodeError::Malformed("direction")),
                };
                self.unit()?;
                self.field("element")?;
                SchemaKind::Channel {
                    direction: dir,
                    element: self.reference(s, u, d + 1)?,
                }
            }
            "Dynamic" => {
                self.unit()?;
                SchemaKind::Dynamic
            }
            "External" => {
                self.st("External", 2)?;
                self.field("kind")?;
                let k = self.ix(s, u)?;
                self.field("metadata")?;
                let m = match self.r.read_u8()? {
                    TAG_NONE => None,
                    TAG_SOME => Some(self.reference(s, u, d + 1)?),
                    g => {
                        return Err(DecodeError::UnexpectedTag {
                            expected: "option",
                            got: g,
                        });
                    }
                };
                SchemaKind::External {
                    kind: k,
                    metadata: m,
                }
            }
            "Semantic" => {
                self.st("Semantic", 3)?;
                self.field("name")?;
                let raw = self.ix(s, u)?;
                let q = QualifiedName::try_from(raw.as_str())
                    .map_err(|_| DecodeError::Malformed("qualified name"))?;
                self.field("args")?;
                let a = self.refs(s, u, d + 1)?;
                self.field("representation")?;
                SchemaKind::Semantic {
                    name: SemanticName::from(q),
                    args: a,
                    representation: self.reference(s, u, d + 1)?,
                }
            }
            _ => return Err(DecodeError::Malformed("schema kind")),
        })
    }
    fn one(
        &mut self,
        n: &str,
        f: &str,
        s: &[String],
        u: &mut HashSet<usize>,
        d: usize,
    ) -> Result<SchemaRef, DecodeError> {
        self.st(n, 1)?;
        self.field(f)?;
        self.reference(s, u, d)
    }
    fn refs(
        &mut self,
        s: &[String],
        u: &mut HashSet<usize>,
        d: usize,
    ) -> Result<Vec<SchemaRef>, DecodeError> {
        let c = self.list_len()?;
        let mut rs = Vec::with_capacity(c);
        for _ in 0..c {
            rs.push(self.reference(s, u, d)?)
        }
        Ok(rs)
    }
    fn reference(
        &mut self,
        s: &[String],
        u: &mut HashSet<usize>,
        d: usize,
    ) -> Result<SchemaRef, DecodeError> {
        if d > self.limits.max_nesting {
            return Err(DecodeError::DepthExceeded);
        }
        self.expect(TAG_ENUM, "enum")?;
        match self.r.read_str()? {
            "Concrete" => {
                self.st("Concrete", 2)?;
                self.field("id")?;
                let id = SchemaId::from_raw(self.u64()?);
                self.field("args")?;
                Ok(SchemaRef::Concrete {
                    id,
                    args: self.refs(s, u, d + 1)?,
                })
            }
            "Var" => {
                self.st("Var", 1)?;
                self.field("name")?;
                Ok(SchemaRef::Var {
                    name: self.ix(s, u)?,
                })
            }
            _ => Err(DecodeError::Malformed("schema ref")),
        }
    }
    fn fields(
        &mut self,
        s: &[String],
        u: &mut HashSet<usize>,
        d: usize,
    ) -> Result<Vec<Field>, DecodeError> {
        let c = self.list_len()?;
        let mut fs = Vec::with_capacity(c);
        for _ in 0..c {
            self.st("Field", 3)?;
            self.field("name")?;
            let n = self.ix(s, u)?;
            self.field("schema")?;
            let r = self.reference(s, u, d)?;
            self.field("required")?;
            fs.push(Field {
                name: n,
                schema: r,
                required: self.bool()?,
            })
        }
        Ok(fs)
    }
    fn variant(
        &mut self,
        s: &[String],
        u: &mut HashSet<usize>,
        d: usize,
    ) -> Result<Variant, DecodeError> {
        self.st("Variant", 3)?;
        self.field("name")?;
        let n = self.ix(s, u)?;
        self.field("index")?;
        let i = self.u32()?;
        self.field("payload")?;
        self.expect(TAG_ENUM, "enum")?;
        let p = match self.r.read_str()? {
            "Unit" => {
                self.unit()?;
                VariantPayload::Unit
            }
            "Newtype" => VariantPayload::Newtype(self.reference(s, u, d)?),
            "Tuple" => VariantPayload::Tuple(self.refs(s, u, d)?),
            "Struct" => VariantPayload::Struct(self.fields(s, u, d)?),
            _ => return Err(DecodeError::Malformed("variant payload")),
        };
        Ok(Variant {
            name: n,
            index: i,
            payload: p,
        })
    }
}

#[cfg(test)]
mod independent_golden_consumer {
    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    struct Summary {
        version: u32,
        strings: Vec<String>,
        schema_ids: Vec<u64>,
    }

    struct Parser<'a> {
        bytes: &'a [u8],
        cursor: usize,
    }

    impl<'a> Parser<'a> {
        fn take(&mut self, count: usize) -> Result<&'a [u8], &'static str> {
            let end = self.cursor.checked_add(count).ok_or("overflow")?;
            let bytes = self.bytes.get(self.cursor..end).ok_or("truncated")?;
            self.cursor = end;
            Ok(bytes)
        }

        fn u8(&mut self) -> Result<u8, &'static str> {
            Ok(self.take(1)?[0])
        }
        fn u32(&mut self) -> Result<u32, &'static str> {
            Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
        }
        fn u64(&mut self) -> Result<u64, &'static str> {
            Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
        }
        fn string(&mut self) -> Result<&'a str, &'static str> {
            let len = self.u32()? as usize;
            std::str::from_utf8(self.take(len)?).map_err(|_| "utf8")
        }
        fn tag(&mut self, expected: u8) -> Result<(), &'static str> {
            if self.u8()? == expected {
                Ok(())
            } else {
                Err("tag")
            }
        }
        fn structure(&mut self, name: &str, fields: u32) -> Result<(), &'static str> {
            self.tag(TAG_STRUCT)?;
            if self.string()? == name && self.u32()? == fields {
                Ok(())
            } else {
                Err("struct")
            }
        }
        fn field(&mut self, name: &str) -> Result<(), &'static str> {
            if self.string()? == name {
                Ok(())
            } else {
                Err("field")
            }
        }
        fn list(&mut self) -> Result<usize, &'static str> {
            self.tag(TAG_LIST)?;
            Ok(self.u32()? as usize)
        }
        fn value_u32(&mut self) -> Result<u32, &'static str> {
            self.tag(TAG_U32)?;
            self.u32()
        }
        fn value_u64(&mut self) -> Result<u64, &'static str> {
            self.tag(TAG_U64)?;
            self.u64()
        }
        fn skip_ref(&mut self) -> Result<(), &'static str> {
            self.tag(TAG_ENUM)?;
            if self.string()? != "Concrete" {
                return Err("ref");
            }
            self.structure("Concrete", 2)?;
            self.field("id")?;
            self.value_u64()?;
            self.field("args")?;
            if self.list()? != 0 {
                return Err("args");
            }
            Ok(())
        }
        fn parse_point(mut self) -> Result<Summary, &'static str> {
            if self.take(MAGIC.len())? != MAGIC {
                return Err("magic");
            }
            let version = self.u32()?;
            self.structure("SchemaBundleV1", 2)?;
            self.field("strings")?;
            let mut strings = Vec::new();
            for _ in 0..self.list()? {
                self.tag(TAG_STRING)?;
                strings.push(self.string()?.to_string());
            }
            self.field("schemas")?;
            if self.list()? != 1 {
                return Err("schema count");
            }
            self.structure("Schema", 3)?;
            self.field("id")?;
            let schema_ids = vec![self.value_u64()?];
            self.field("type_params")?;
            if self.list()? != 0 {
                return Err("params");
            }
            self.field("kind")?;
            self.tag(TAG_ENUM)?;
            if self.string()? != "Struct" {
                return Err("kind");
            }
            self.structure("Struct", 2)?;
            self.field("name")?;
            if self.value_u32()? != 0 {
                return Err("name index");
            }
            self.field("fields")?;
            if self.list()? != 1 {
                return Err("field count");
            }
            self.structure("Field", 3)?;
            self.field("name")?;
            if self.value_u32()? != 1 {
                return Err("field index");
            }
            self.field("schema")?;
            self.skip_ref()?;
            self.field("required")?;
            self.tag(TAG_BOOL)?;
            if self.u8()? != 1 || self.cursor != self.bytes.len() {
                return Err("tail");
            }
            Ok(Summary {
                version,
                strings,
                schema_ids,
            })
        }
    }

    #[test]
    fn independent_consumer_admits_frozen_point_golden() {
        let bytes = include_bytes!("../testdata/schema-bundle-point-v1.phon");
        let independent = Parser { bytes, cursor: 0 }.parse_point().unwrap();
        let production = schema_bundle_from_bytes(bytes, SchemaBundleLimits::default()).unwrap();
        assert_eq!(independent.version, VERSION);
        assert_eq!(independent.strings, ["Point", "x"]);
        assert_eq!(
            independent.schema_ids,
            production
                .schemas()
                .iter()
                .map(|schema| schema.id.as_u64())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn independent_consumer_matches_truncation_rejection_class() {
        let bytes = include_bytes!("../testdata/schema-bundle-point-v1.phon");
        for end in 0..bytes.len() {
            assert!(
                Parser {
                    bytes: &bytes[..end],
                    cursor: 0
                }
                .parse_point()
                .is_err()
            );
            assert!(
                schema_bundle_from_bytes(&bytes[..end], SchemaBundleLimits::default()).is_err()
            );
        }
    }
}
