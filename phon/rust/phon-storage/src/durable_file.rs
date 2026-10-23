use std::collections::{BTreeMap, HashMap};

use blake3::Hash;
use phon_schema::{
    DecodeError, QualifiedName, SchemaBundle, SchemaBundleLimits, SchemaId, SchemaRef,
    qualified_name_from_compact_bytes, schema_bundle_from_bytes, schema_bundle_to_bytes,
};

const MAGIC: &[u8; 8] = b"PHONFIL1";
const FORMAT: u32 = 1;
const ALIGNMENT: u64 = 16;
const FIXED_HEADER: usize = 32;
const DESCRIPTOR_SIZE: usize = 72;
const MANIFEST_MAGIC: &[u8; 8] = b"PHONFTR1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DurableFileError {
    OffsetOverflow,
    FileLength {
        declared: u64,
        actual: usize,
    },
    InvalidMagic,
    UnsupportedFormat(u32),
    InvalidDescriptor,
    MisalignedExtent {
        extent: usize,
        offset: u64,
    },
    NonMinimalPlacement {
        extent: usize,
        expected: u64,
        actual: u64,
    },
    TruncatedExtent {
        extent: usize,
        offset: u64,
        len: u64,
        file_len: usize,
    },
    RegionSchemaMismatch {
        region: u32,
        expected: SchemaRef,
        actual: SchemaRef,
        encoded_offset: u64,
    },
    UnreachableRegion {
        region: u32,
    },
    DanglingRegionReference {
        region: u32,
        encoded_offset: u64,
    },
    NonRepeatable {
        extent: usize,
    },
    SchemaBundle(DecodeError),
    MissingSchema(SchemaId),
    WriteFailed {
        written: u64,
        kind: std::io::ErrorKind,
    },
    AuxFeatureNotDeclared {
        feature: QualifiedName,
    },
    InvalidAuxNumber {
        feature: QualifiedName,
        name: QualifiedName,
        expected: u32,
        actual: u32,
    },
    InvalidFeatureManifest,
    UnknownRequiredFeature {
        feature: QualifiedName,
    },
    RequiredFeatureInvalid {
        feature: QualifiedName,
        message: &'static str,
    },
}
impl core::fmt::Display for DurableFileError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "durable PHON file error: {self:?}")
    }
}
impl std::error::Error for DurableFileError {}
type Result<T> = core::result::Result<T, DurableFileError>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegionRefOccurrence {
    pub source_region: Option<u32>,
    pub target_region: u32,
    pub target_schema: SchemaRef,
    pub encoded_offset: u64,
}
#[derive(Clone, Debug)]
enum PayloadSource {
    Repeatable(Vec<u8>),
    Sequence(Vec<Vec<u8>>),
}
#[derive(Clone, Debug)]
pub struct ExtentPayload {
    schema: SchemaRef,
    source: PayloadSource,
}
impl ExtentPayload {
    #[must_use]
    pub fn repeatable(schema: SchemaRef, bytes: Vec<u8>) -> Self {
        Self {
            schema,
            source: PayloadSource::Repeatable(bytes),
        }
    }
    #[must_use]
    pub fn non_repeatable(schema: SchemaRef, passes: Vec<Vec<u8>>) -> Self {
        Self {
            schema,
            source: PayloadSource::Sequence(passes),
        }
    }
    fn pass(&self, pass: usize) -> &[u8] {
        match &self.source {
            PayloadSource::Repeatable(bytes) => bytes,
            PayloadSource::Sequence(passes) => passes
                .get(pass)
                .or_else(|| passes.last())
                .map_or(&[], Vec::as_slice),
        }
    }
}

// r[impl compact.file.admission]
#[derive(Clone, Debug)]
pub struct AuxExtentPayload {
    feature: QualifiedName,
    name: QualifiedName,
    number: u32,
    payload: ExtentPayload,
}
impl AuxExtentPayload {
    #[must_use]
    pub fn compact(
        feature: QualifiedName,
        name: QualifiedName,
        number: u32,
        payload: ExtentPayload,
    ) -> Self {
        Self {
            feature,
            name,
            number,
            payload,
        }
    }
}

pub struct DurableFilePlan {
    schemas: SchemaBundle,
    root: ExtentPayload,
    regions: Vec<ExtentPayload>,
    region_refs: Vec<RegionRefOccurrence>,
    required_features: Vec<QualifiedName>,
    optional_features: Vec<QualifiedName>,
    aux: Vec<AuxExtentPayload>,
}
impl DurableFilePlan {
    #[must_use]
    pub fn new(
        schemas: SchemaBundle,
        root: ExtentPayload,
        regions: Vec<ExtentPayload>,
        region_refs: Vec<RegionRefOccurrence>,
    ) -> Self {
        Self {
            schemas,
            root,
            regions,
            region_refs,
            required_features: Vec::new(),
            optional_features: Vec::new(),
            aux: Vec::new(),
        }
    }
    #[must_use]
    pub fn region_refs(&self) -> &[RegionRefOccurrence] {
        &self.region_refs
    }
    pub fn region_refs_mut(&mut self) -> &mut Vec<RegionRefOccurrence> {
        &mut self.region_refs
    }
    pub fn set_features(
        &mut self,
        required: Vec<QualifiedName>,
        optional: Vec<QualifiedName>,
        aux: Vec<AuxExtentPayload>,
    ) {
        self.required_features = required;
        self.optional_features = optional;
        self.aux = aux;
    }
    pub fn write_to_vec(&self) -> Result<Vec<u8>> {
        self.validate_graph()?;
        let manifest = FeatureManifest::canonical(
            &self.required_features,
            &self.optional_features,
            &self.aux,
        )?;
        let manifest_bytes = manifest.to_bytes()?;
        let schema_bytes = schema_bundle_to_bytes(self.schemas.schemas())
            .map_err(DurableFileError::SchemaBundle)?;
        let mut payloads = Vec::with_capacity(self.regions.len() + self.aux.len() + 2);
        payloads.push((schema_bytes.as_slice(), None, 0u32, 0u32));
        payloads.push((self.root.pass(0), Some(&self.root.schema), 1, 0));
        for (number, region) in self.regions.iter().enumerate() {
            payloads.push((region.pass(0), Some(&region.schema), 2, number as u32));
        }
        for aux in &self.aux {
            payloads.push((
                aux.payload.pass(0),
                Some(&aux.payload.schema),
                3,
                aux.number,
            ));
        }
        let descriptor_bytes = payloads
            .len()
            .checked_mul(DESCRIPTOR_SIZE)
            .ok_or(DurableFileError::OffsetOverflow)?;
        let descriptor_offset = FIXED_HEADER
            .checked_add(manifest_bytes.len())
            .ok_or(DurableFileError::OffsetOverflow)?;
        let envelope_len = descriptor_offset
            .checked_add(descriptor_bytes)
            .ok_or(DurableFileError::OffsetOverflow)? as u64;
        let mut offsets = Vec::with_capacity(payloads.len());
        let mut cursor = align_up(envelope_len, ALIGNMENT)?;
        for (index, (bytes, _, _, _)) in payloads.iter().enumerate() {
            offsets.push(cursor);
            cursor = cursor
                .checked_add(bytes.len() as u64)
                .ok_or(DurableFileError::OffsetOverflow)?;
            if index + 1 < payloads.len() {
                cursor = align_up(cursor, ALIGNMENT)?;
            }
        }
        let file_len = cursor;
        let capacity = usize::try_from(file_len).map_err(|_| DurableFileError::OffsetOverflow)?;
        let mut out = Vec::with_capacity(capacity);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&FORMAT.to_le_bytes());
        out.extend_from_slice(&(payloads.len() as u32).to_le_bytes());
        out.extend_from_slice(&file_len.to_le_bytes());
        out.extend_from_slice(&(descriptor_offset as u64).to_le_bytes());
        out.extend_from_slice(&manifest_bytes);
        for ((bytes, schema, kind, number), offset) in payloads.iter().zip(&offsets) {
            out.extend_from_slice(&kind.to_le_bytes());
            out.extend_from_slice(&number.to_le_bytes());
            out.extend_from_slice(&offset.to_le_bytes());
            out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            out.extend_from_slice(&ALIGNMENT.to_le_bytes());
            encode_schema_slot(&mut out, *schema)?;
            out.extend_from_slice(blake3::hash(bytes).as_bytes());
        }
        pad(&mut out, ALIGNMENT as usize);
        for (index, (expected, _, _, _)) in payloads.iter().enumerate() {
            let actual = if index == 0 {
                schema_bytes.as_slice()
            } else if index == 1 {
                self.root.pass(1)
            } else if index < self.regions.len() + 2 {
                self.regions[index - 2].pass(1)
            } else {
                self.aux[index - self.regions.len() - 2].payload.pass(1)
            };
            if actual.len() != expected.len() || blake3::hash(actual) != blake3::hash(expected) {
                return Err(DurableFileError::NonRepeatable { extent: index });
            }
            out.extend_from_slice(actual);
            if index + 1 < payloads.len() {
                pad(&mut out, ALIGNMENT as usize);
            }
        }
        debug_assert_eq!(out.len(), capacity);
        Ok(out)
    }
    pub fn write_to(&self, sink: &mut impl std::io::Write) -> Result<()> {
        let bytes = self.write_to_vec()?;
        let mut written = 0usize;
        while written < bytes.len() {
            match sink.write(&bytes[written..]) {
                Ok(0) => {
                    return Err(DurableFileError::WriteFailed {
                        written: written as u64,
                        kind: std::io::ErrorKind::WriteZero,
                    });
                }
                Ok(count) => written += count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => {
                    return Err(DurableFileError::WriteFailed {
                        written: written as u64,
                        kind: error.kind(),
                    });
                }
            }
        }
        sink.flush().map_err(|error| DurableFileError::WriteFailed {
            written: written as u64,
            kind: error.kind(),
        })
    }
    fn validate_graph(&self) -> Result<()> {
        validate_region_graph(
            &self
                .regions
                .iter()
                .map(|region| region.schema.clone())
                .collect::<Vec<_>>(),
            &self.region_refs,
        )
    }
}

// r[impl compact.file.bootstrap]
// r[impl compact.file.format-version]
#[derive(Clone, Debug, PartialEq, Eq)]
struct AuxIdentity {
    feature: QualifiedName,
    name: QualifiedName,
    number: u32,
}

#[derive(Clone, Debug, Default)]
struct FeatureManifest {
    required: Vec<QualifiedName>,
    optional: Vec<QualifiedName>,
    aux: Vec<AuxIdentity>,
}
impl FeatureManifest {
    fn canonical(
        required: &[QualifiedName],
        optional: &[QualifiedName],
        aux: &[AuxExtentPayload],
    ) -> Result<Self> {
        if !strictly_sorted(required)
            || !strictly_sorted(optional)
            || required
                .iter()
                .any(|feature| optional.binary_search(feature).is_ok())
        {
            return Err(DurableFileError::InvalidFeatureManifest);
        }
        let mut identities = aux
            .iter()
            .map(|aux| AuxIdentity {
                feature: aux.feature.clone(),
                name: aux.name.clone(),
                number: aux.number,
            })
            .collect::<Vec<_>>();
        identities.sort_by(|left, right| {
            (&left.feature, &left.name, left.number).cmp(&(
                &right.feature,
                &right.name,
                right.number,
            ))
        });
        let supplied = aux
            .iter()
            .map(|aux| (&aux.feature, &aux.name, aux.number))
            .collect::<Vec<_>>();
        let canonical = identities
            .iter()
            .map(|aux| (&aux.feature, &aux.name, aux.number))
            .collect::<Vec<_>>();
        if supplied != canonical {
            return Err(DurableFileError::InvalidFeatureManifest);
        }
        let mut expected_numbers = BTreeMap::<(&QualifiedName, &QualifiedName), u32>::new();
        for identity in &identities {
            if required.binary_search(&identity.feature).is_err()
                && optional.binary_search(&identity.feature).is_err()
            {
                return Err(DurableFileError::AuxFeatureNotDeclared {
                    feature: identity.feature.clone(),
                });
            }
            let expected = expected_numbers
                .entry((&identity.feature, &identity.name))
                .or_default();
            if identity.number != *expected {
                return Err(DurableFileError::InvalidAuxNumber {
                    feature: identity.feature.clone(),
                    name: identity.name.clone(),
                    expected: *expected,
                    actual: identity.number,
                });
            }
            *expected += 1;
        }
        Ok(Self {
            required: required.to_vec(),
            optional: optional.to_vec(),
            aux: identities,
        })
    }

    fn to_bytes(&self) -> Result<Vec<u8>> {
        if self.required.is_empty() && self.optional.is_empty() && self.aux.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        out.extend_from_slice(MANIFEST_MAGIC);
        write_names(&mut out, &self.required)?;
        write_names(&mut out, &self.optional)?;
        out.extend_from_slice(&(self.aux.len() as u32).to_le_bytes());
        for aux in &self.aux {
            write_name(&mut out, &aux.feature)?;
            write_name(&mut out, &aux.name)?;
            out.extend_from_slice(&aux.number.to_le_bytes());
        }
        Ok(out)
    }

    fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() {
            return Ok(Self::default());
        }
        let mut cursor = 0usize;
        if take(bytes, &mut cursor, MANIFEST_MAGIC.len())? != MANIFEST_MAGIC {
            return Err(DurableFileError::InvalidFeatureManifest);
        }
        let required = read_names(bytes, &mut cursor)?;
        let optional = read_names(bytes, &mut cursor)?;
        let count = read_u32(bytes, &mut cursor)? as usize;
        let mut aux = Vec::with_capacity(count);
        for _ in 0..count {
            aux.push(AuxIdentity {
                feature: read_name(bytes, &mut cursor)?,
                name: read_name(bytes, &mut cursor)?,
                number: read_u32(bytes, &mut cursor)?,
            });
        }
        if cursor != bytes.len() {
            return Err(DurableFileError::InvalidFeatureManifest);
        }
        let payloads = aux
            .iter()
            .map(|identity| AuxExtentPayload {
                feature: identity.feature.clone(),
                name: identity.name.clone(),
                number: identity.number,
                payload: ExtentPayload::repeatable(
                    SchemaRef::concrete(SchemaId::from_raw(1)),
                    Vec::new(),
                ),
            })
            .collect::<Vec<_>>();
        Self::canonical(&required, &optional, &payloads)
    }
}

fn strictly_sorted(names: &[QualifiedName]) -> bool {
    names.windows(2).all(|pair| pair[0] < pair[1])
}

fn write_names(out: &mut Vec<u8>, names: &[QualifiedName]) -> Result<()> {
    out.extend_from_slice(&(names.len() as u32).to_le_bytes());
    for name in names {
        write_name(out, name)?;
    }
    Ok(())
}

fn write_name(out: &mut Vec<u8>, name: &QualifiedName) -> Result<()> {
    let bytes = name.compact_bytes();
    let len = u32::try_from(bytes.len()).map_err(|_| DurableFileError::OffsetOverflow)?;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&bytes);
    Ok(())
}

fn read_names(bytes: &[u8], cursor: &mut usize) -> Result<Vec<QualifiedName>> {
    let count = read_u32(bytes, cursor)? as usize;
    (0..count).map(|_| read_name(bytes, cursor)).collect()
}

fn read_name(bytes: &[u8], cursor: &mut usize) -> Result<QualifiedName> {
    let len = read_u32(bytes, cursor)? as usize;
    qualified_name_from_compact_bytes(take(bytes, cursor, len)?)
        .map_err(|_| DurableFileError::InvalidFeatureManifest)
}

fn read_u32(bytes: &[u8], cursor: &mut usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        take(bytes, cursor, 4)?.try_into().unwrap(),
    ))
}

fn take<'a>(bytes: &'a [u8], cursor: &mut usize, len: usize) -> Result<&'a [u8]> {
    let end = cursor
        .checked_add(len)
        .ok_or(DurableFileError::OffsetOverflow)?;
    let result = bytes
        .get(*cursor..end)
        .ok_or(DurableFileError::InvalidFeatureManifest)?;
    *cursor = end;
    Ok(result)
}

fn validate_region_graph(regions: &[SchemaRef], refs: &[RegionRefOccurrence]) -> Result<()> {
    let mut seen: HashMap<u32, u64> = HashMap::new();
    for reference in refs {
        let Some(actual) = regions.get(reference.target_region as usize) else {
            return Err(DurableFileError::DanglingRegionReference {
                region: reference.target_region,
                encoded_offset: reference.encoded_offset,
            });
        };
        if actual != &reference.target_schema {
            return Err(DurableFileError::RegionSchemaMismatch {
                region: reference.target_region,
                expected: actual.clone(),
                actual: reference.target_schema.clone(),
                encoded_offset: reference.encoded_offset,
            });
        }
        seen.entry(reference.target_region)
            .or_insert(reference.encoded_offset);
    }
    for region in 0..regions.len() as u32 {
        if !seen.contains_key(&region) {
            return Err(DurableFileError::UnreachableRegion { region });
        }
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct ExtentDescriptor {
    index: usize,
    descriptor_offset: usize,
    kind: u32,
    number: u32,
    offset: u64,
    len: u64,
    schema: Option<SchemaRef>,
    digest: Hash,
}
impl ExtentDescriptor {
    #[must_use]
    pub fn offset(&self) -> u64 {
        self.offset
    }
    #[must_use]
    pub fn end(&self) -> u64 {
        self.offset + self.len
    }
    pub fn patch_offset(&self, bytes: &mut [u8], offset: u64) {
        bytes[self.descriptor_offset + 8..self.descriptor_offset + 16]
            .copy_from_slice(&offset.to_le_bytes())
    }
}
#[derive(Clone, Copy)]
pub struct ExtentView<'a> {
    descriptor: &'a ExtentDescriptor,
    bytes: &'a [u8],
}
impl<'a> ExtentView<'a> {
    #[must_use]
    pub fn schema(&self) -> &SchemaRef {
        self.descriptor.schema.as_ref().expect("typed extent")
    }
    #[must_use]
    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }
    #[must_use]
    pub fn extent(&self) -> &ExtentDescriptor {
        self.descriptor
    }
}

#[derive(Clone, Copy)]
pub struct AuxExtentView<'a> {
    identity: &'a AuxIdentity,
    view: ExtentView<'a>,
}
impl<'a> AuxExtentView<'a> {
    #[must_use]
    pub fn feature(&self) -> &QualifiedName {
        &self.identity.feature
    }
    #[must_use]
    pub fn name(&self) -> &QualifiedName {
        &self.identity.name
    }
    #[must_use]
    pub fn number(&self) -> u32 {
        self.identity.number
    }
    #[must_use]
    pub fn view(&self) -> ExtentView<'a> {
        self.view
    }
}

type FeatureValidator = for<'a> fn(&[AuxExtentView<'a>]) -> core::result::Result<(), &'static str>;

#[derive(Default)]
pub struct FeatureRegistry {
    validators: BTreeMap<QualifiedName, FeatureValidator>,
}
impl FeatureRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
    pub fn register(&mut self, feature: QualifiedName, validator: FeatureValidator) {
        self.validators.insert(feature, validator);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeatureDiagnostic {
    feature: QualifiedName,
    message: &'static str,
}
impl FeatureDiagnostic {
    #[must_use]
    pub fn feature(&self) -> &QualifiedName {
        &self.feature
    }
    #[must_use]
    pub fn message(&self) -> &'static str {
        self.message
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeatureAdmission {
    diagnostics: Vec<FeatureDiagnostic>,
    discarded_aux: usize,
}
impl FeatureAdmission {
    #[must_use]
    pub fn diagnostics(&self) -> &[FeatureDiagnostic] {
        &self.diagnostics
    }
    #[must_use]
    pub fn discarded_aux(&self) -> usize {
        self.discarded_aux
    }
}
// r[impl compact.file.admission]
pub struct StructuralFileView<'a> {
    schemas: SchemaBundle,
    bytes: &'a [u8],
    extents: Vec<ExtentDescriptor>,
    manifest: FeatureManifest,
    region_count: usize,
}
impl<'a> StructuralFileView<'a> {
    pub fn parse(bytes: &'a [u8], refs: &[RegionRefOccurrence]) -> Result<Self> {
        if bytes.len() < FIXED_HEADER {
            return Err(DurableFileError::FileLength {
                declared: FIXED_HEADER as u64,
                actual: bytes.len(),
            });
        }
        if &bytes[..8] != MAGIC {
            return Err(DurableFileError::InvalidMagic);
        }
        let format = u32_at(bytes, 8)?;
        if format != FORMAT {
            return Err(DurableFileError::UnsupportedFormat(format));
        }
        let count = u32_at(bytes, 12)? as usize;
        let file_len = u64_at(bytes, 16)?;
        if file_len != bytes.len() as u64 {
            return Err(DurableFileError::FileLength {
                declared: file_len,
                actual: bytes.len(),
            });
        }
        let descriptor_offset =
            usize::try_from(u64_at(bytes, 24)?).map_err(|_| DurableFileError::OffsetOverflow)?;
        if descriptor_offset < FIXED_HEADER || descriptor_offset > bytes.len() {
            return Err(DurableFileError::InvalidFeatureManifest);
        }
        let manifest = FeatureManifest::parse(&bytes[FIXED_HEADER..descriptor_offset])?;
        let descriptor_end = descriptor_offset
            .checked_add(
                count
                    .checked_mul(DESCRIPTOR_SIZE)
                    .ok_or(DurableFileError::OffsetOverflow)?,
            )
            .ok_or(DurableFileError::OffsetOverflow)?;
        if descriptor_end > bytes.len() {
            return Err(DurableFileError::TruncatedExtent {
                extent: 0,
                offset: 0,
                len: descriptor_end as u64,
                file_len: bytes.len(),
            });
        }

        let mut extents = Vec::with_capacity(count);
        for index in 0..count {
            let base = descriptor_offset + index * DESCRIPTOR_SIZE;
            let kind = u32_at(bytes, base)?;
            let number = u32_at(bytes, base + 4)?;
            let offset = u64_at(bytes, base + 8)?;
            let len = u64_at(bytes, base + 16)?;
            let alignment = u64_at(bytes, base + 24)?;
            if alignment != ALIGNMENT || !offset.is_multiple_of(ALIGNMENT) {
                return Err(DurableFileError::MisalignedExtent {
                    extent: index,
                    offset,
                });
            }
            let schema = decode_schema_slot(&bytes[base + 32..base + 40])?;
            let digest = Hash::from_bytes(bytes[base + 40..base + 72].try_into().unwrap());
            extents.push(ExtentDescriptor {
                index,
                descriptor_offset: base,
                kind,
                number,
                offset,
                len,
                schema,
                digest,
            });
        }

        let mut expected = align_up(descriptor_end as u64, ALIGNMENT)?;
        for extent in &extents {
            if extent.offset != expected {
                return Err(DurableFileError::NonMinimalPlacement {
                    extent: extent.index,
                    expected,
                    actual: extent.offset,
                });
            }
            let end = extent
                .offset
                .checked_add(extent.len)
                .ok_or(DurableFileError::OffsetOverflow)?;
            if end > file_len {
                return Err(DurableFileError::TruncatedExtent {
                    extent: extent.index,
                    offset: extent.offset,
                    len: extent.len,
                    file_len: bytes.len(),
                });
            }
            let actual = &bytes[extent.offset as usize..end as usize];
            if blake3::hash(actual) != extent.digest {
                return Err(DurableFileError::InvalidDescriptor);
            }
            expected = if extent.index + 1 < extents.len() {
                align_up(end, ALIGNMENT)?
            } else {
                end
            };
        }
        if expected != file_len {
            return Err(DurableFileError::FileLength {
                declared: expected,
                actual: bytes.len(),
            });
        }
        if extents.len() < 2 || extents[0].kind != 0 || extents[1].kind != 1 {
            return Err(DurableFileError::InvalidDescriptor);
        }
        let mut region_count = 0usize;
        for extent in extents.iter().skip(2) {
            if extent.kind != 2 {
                break;
            }
            if extent.number as usize != region_count {
                return Err(DurableFileError::InvalidDescriptor);
            }
            region_count += 1;
        }
        let aux_extents = &extents[region_count + 2..];
        if aux_extents.len() != manifest.aux.len()
            || aux_extents.iter().any(|extent| extent.kind != 3)
            || aux_extents
                .iter()
                .zip(&manifest.aux)
                .any(|(extent, identity)| extent.number != identity.number)
        {
            return Err(DurableFileError::InvalidDescriptor);
        }

        let schema_extent = &extents[0];
        let schemas = schema_bundle_from_bytes(
            &bytes[schema_extent.offset as usize..schema_extent.end() as usize],
            SchemaBundleLimits::default(),
        )
        .map_err(DurableFileError::SchemaBundle)?;
        for extent in extents.iter().skip(1) {
            validate_extent_schema(extent, &schemas)?;
        }
        let regions = extents
            .iter()
            .skip(2)
            .take(region_count)
            .map(|extent| {
                extent
                    .schema
                    .clone()
                    .ok_or(DurableFileError::InvalidDescriptor)
            })
            .collect::<Result<Vec<_>>>()?;
        validate_region_graph(&regions, refs)?;
        Ok(Self {
            schemas,
            bytes,
            extents,
            manifest,
            region_count,
        })
    }
    #[must_use]
    pub fn required_features(&self) -> &[QualifiedName] {
        &self.manifest.required
    }
    #[must_use]
    pub fn optional_features(&self) -> &[QualifiedName] {
        &self.manifest.optional
    }
    #[must_use]
    pub fn aux_extents(&self) -> Vec<AuxExtentView<'_>> {
        self.manifest
            .aux
            .iter()
            .enumerate()
            .map(|(index, identity)| AuxExtentView {
                identity,
                view: self.view(self.region_count + 2 + index),
            })
            .collect()
    }
    pub fn admit_features(&self, registry: &FeatureRegistry) -> Result<FeatureAdmission> {
        let all_aux = self.aux_extents();
        let mut admission = FeatureAdmission::default();
        for feature in &self.manifest.required {
            let Some(validator) = registry.validators.get(feature) else {
                return Err(DurableFileError::UnknownRequiredFeature {
                    feature: feature.clone(),
                });
            };
            let aux = all_aux
                .iter()
                .copied()
                .filter(|extent| extent.feature() == feature)
                .collect::<Vec<_>>();
            if let Err(message) = validator(&aux) {
                return Err(DurableFileError::RequiredFeatureInvalid {
                    feature: feature.clone(),
                    message,
                });
            }
        }
        for feature in &self.manifest.optional {
            let Some(validator) = registry.validators.get(feature) else {
                continue;
            };
            let aux = all_aux
                .iter()
                .copied()
                .filter(|extent| extent.feature() == feature)
                .collect::<Vec<_>>();
            if let Err(message) = validator(&aux) {
                admission.discarded_aux += aux.len();
                admission.diagnostics.push(FeatureDiagnostic {
                    feature: feature.clone(),
                    message,
                });
            }
        }
        Ok(admission)
    }
    #[must_use]
    pub fn extents(&self) -> &[ExtentDescriptor] {
        &self.extents
    }
    #[must_use]
    pub fn schemas(&self) -> &SchemaBundle {
        &self.schemas
    }
    #[must_use]
    pub fn root_schema(&self) -> &SchemaRef {
        self.extents[1].schema.as_ref().expect("root schema")
    }
    #[must_use]
    pub fn root(&self) -> ExtentView<'_> {
        self.view(1)
    }
    pub fn region(&self, number: u32) -> Option<ExtentView<'_>> {
        if number as usize >= self.region_count {
            return None;
        }
        Some(self.view(number as usize + 2))
    }
    fn view(&self, index: usize) -> ExtentView<'_> {
        let descriptor = &self.extents[index];
        let start = descriptor.offset as usize;
        ExtentView {
            descriptor,
            bytes: &self.bytes[start..start + descriptor.len as usize],
        }
    }
}

fn validate_extent_schema(extent: &ExtentDescriptor, schemas: &SchemaBundle) -> Result<()> {
    let Some(SchemaRef::Concrete { id, args }) = extent.schema.as_ref() else {
        return Err(DurableFileError::InvalidDescriptor);
    };
    if !args.is_empty() {
        return Err(DurableFileError::InvalidDescriptor);
    }
    let primitive = phon_schema::Primitive::ALL
        .iter()
        .any(|primitive| phon_schema::primitive_id(*primitive) == *id);
    if !primitive && !schemas.schemas().iter().any(|schema| schema.id == *id) {
        return Err(DurableFileError::MissingSchema(*id));
    }
    Ok(())
}
pub fn align_up(value: u64, alignment: u64) -> Result<u64> {
    if alignment == 0 || !alignment.is_power_of_two() {
        return Err(DurableFileError::InvalidDescriptor);
    }
    value
        .checked_add(alignment - 1)
        .map(|v| v & !(alignment - 1))
        .ok_or(DurableFileError::OffsetOverflow)
}
fn pad(bytes: &mut Vec<u8>, alignment: usize) {
    while !bytes.len().is_multiple_of(alignment) {
        bytes.push(0)
    }
}
fn encode_schema_slot(out: &mut Vec<u8>, schema: Option<&SchemaRef>) -> Result<()> {
    match schema {
        None => out.extend_from_slice(&0u64.to_le_bytes()),
        Some(SchemaRef::Concrete { id, args }) if args.is_empty() => {
            out.extend_from_slice(&id.as_u64().to_le_bytes())
        }
        Some(_) => return Err(DurableFileError::InvalidDescriptor),
    }
    Ok(())
}
fn decode_schema_slot(bytes: &[u8]) -> Result<Option<SchemaRef>> {
    let raw = u64::from_le_bytes(
        bytes
            .try_into()
            .map_err(|_| DurableFileError::InvalidDescriptor)?,
    );
    if raw == 0 {
        Ok(None)
    } else {
        Ok(Some(SchemaRef::concrete(SchemaId::from_raw(raw))))
    }
}
fn u32_at(bytes: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or(DurableFileError::InvalidDescriptor)?
            .try_into()
            .unwrap(),
    ))
}
fn u64_at(bytes: &[u8], offset: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(
        bytes
            .get(offset..offset + 8)
            .ok_or(DurableFileError::InvalidDescriptor)?
            .try_into()
            .unwrap(),
    ))
}
