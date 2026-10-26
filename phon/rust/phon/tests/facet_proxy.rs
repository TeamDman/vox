use facet::Facet;
use phon::api::Codec;
use phon_schema::{Primitive, primitive_id};

// The proxy's u64 wire layout is deliberately different from this array's
// in-memory layout. Reading the wrapper as its proxy would be unsound.
#[derive(Debug, Clone, Copy, PartialEq, Facet)]
#[facet(proxy = u64)]
struct CheckedCount([u8; 32]);

impl TryFrom<u64> for CheckedCount {
    type Error = String;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        if value > 64 {
            return Err("count exceeds 64".into());
        }
        let mut bytes = [0; 32];
        bytes[17] = value as u8;
        Ok(Self(bytes))
    }
}

impl From<&CheckedCount> for u64 {
    fn from(value: &CheckedCount) -> Self {
        u64::from(value.0[17])
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct OpaqueCount(u64);

unsafe fn convert_out(
    target: facet::PtrConst,
    proxy: facet::PtrUninit,
) -> Result<facet::PtrMut, String> {
    // SAFETY: Facet supplies initialized OpaqueCount and aligned u64 storage.
    unsafe {
        let value = target.get::<OpaqueCount>().0;
        let slot = proxy.as_mut_byte_ptr().cast::<u64>();
        slot.write(value);
        Ok(facet::PtrMut::new(slot.cast::<u8>()))
    }
}

unsafe fn convert_in(
    proxy: facet::PtrConst,
    target: facet::PtrUninit,
) -> Result<facet::PtrMut, String> {
    // SAFETY: Facet supplies initialized u64 and aligned OpaqueCount storage.
    unsafe {
        let value = proxy.get::<u64>();
        if *value > 64 {
            return Err("opaque count exceeds 64".into());
        }
        let slot = target.as_mut_byte_ptr().cast::<OpaqueCount>();
        slot.write(OpaqueCount(*value));
        Ok(facet::PtrMut::new(slot.cast::<u8>()))
    }
}

const PROXY: facet::ProxyDef = facet::ProxyDef {
    shape: <u64 as Facet>::SHAPE,
    convert_in,
    convert_out,
};

// SAFETY: all reflection access goes through the correctly typed proxy above.
unsafe impl Facet<'_> for OpaqueCount {
    const SHAPE: &'static facet::Shape = &const {
        facet::ShapeBuilder::for_sized::<OpaqueCount>("OpaqueCount")
            .ty(facet::Type::User(facet::UserType::Opaque))
            .def(facet::Def::Scalar)
            .marker_traits(facet::MarkerTraits::COPY)
            .proxy(&PROXY)
            .build()
    };
}

// r[verify descriptors.fact-driven]
// r[verify descriptors.explicit-proxy]
#[test]
fn explicit_proxy_controls_wire_schema_and_value_conversion() {
    let derived = phon::derive::of::<CheckedCount>().unwrap();
    assert_eq!(derived.root, primitive_id(Primitive::U64));
    let codec = Codec::<CheckedCount>::new().unwrap();
    let expected = Codec::<u64>::new().unwrap().encode(&42).unwrap();
    assert_eq!(
        codec.encode(&CheckedCount::try_from(42).unwrap()).unwrap(),
        expected
    );
    assert_eq!(
        codec.decode(&expected).unwrap(),
        CheckedCount::try_from(42).unwrap()
    );
    assert!(codec.decode(&65_u64.to_le_bytes()).is_err());
}

// r[verify descriptors.fact-driven]
// r[verify descriptors.explicit-proxy]
#[test]
fn opaque_numeric_proxy_preserves_validation() {
    let derived = phon::derive::of::<OpaqueCount>().unwrap();
    assert_eq!(derived.root, primitive_id(Primitive::U64));
    let codec = Codec::<OpaqueCount>::new().unwrap();
    let bytes = codec.encode(&OpaqueCount(42)).unwrap();
    assert_eq!(bytes, 42_u64.to_le_bytes());
    assert_eq!(codec.decode(&bytes).unwrap(), OpaqueCount(42));
    assert!(codec.decode(&65_u64.to_le_bytes()).is_err());
}

#[derive(Debug, Clone, PartialEq, Facet)]
#[repr(transparent)]
#[facet(transparent, proxy = String)]
struct TextNumber(u64);

impl TryFrom<String> for TextNumber {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let number: u64 = value.parse().map_err(|_| "not a number".to_owned())?;
        if number > 64 {
            return Err("number exceeds 64".into());
        }
        Ok(Self(number))
    }
}

impl TryFrom<&TextNumber> for String {
    type Error = String;
    fn try_from(value: &TextNumber) -> Result<Self, Self::Error> {
        if value.0 > 64 {
            return Err("number exceeds 64".into());
        }
        Ok(value.0.to_string())
    }
}

#[test]
// r[verify descriptors.explicit-proxy]
fn owned_string_proxy_precedes_transparency_and_validates_both_directions() {
    let derived = phon::derive::of::<TextNumber>().unwrap();
    assert_eq!(derived.root, primitive_id(Primitive::String));
    let codec = Codec::<TextNumber>::new().unwrap();
    let string_codec = Codec::<String>::new().unwrap();
    let bytes = string_codec.encode(&"42".to_owned()).unwrap();
    assert_eq!(codec.encode(&TextNumber(42)).unwrap(), bytes);
    assert_eq!(codec.decode(&bytes).unwrap(), TextNumber(42));
    assert!(codec.encode(&TextNumber(65)).is_err());
    let rejected = string_codec.encode(&"65".to_owned()).unwrap();
    assert!(codec.decode(&rejected).is_err());
    assert!(codec.decode(&[1, 0, 0, 0, 0xff]).is_err());
    assert!(codec.decode(&bytes[..bytes.len() - 1]).is_err());
    assert_eq!(codec.decode(&bytes).unwrap(), TextNumber(42));
}

#[derive(Debug, PartialEq, Facet)]
struct Packet {
    optional: Option<TextNumber>,
    repeated: Vec<CheckedCount>,
    opaque: OpaqueCount,
}

#[test]
// r[verify descriptors.explicit-proxy]
fn nested_optional_and_collection_proxies_roundtrip() {
    let codec = Codec::<Packet>::new().unwrap();
    for optional in [None, Some(TextNumber(42))] {
        let value = Packet {
            optional,
            repeated: vec![
                CheckedCount::try_from(0).unwrap(),
                CheckedCount::try_from(64).unwrap(),
            ],
            opaque: OpaqueCount(7),
        };
        let bytes = codec.encode(&value).unwrap();
        assert_eq!(codec.decode(&bytes).unwrap(), value);
    }
    let invalid = Packet {
        optional: Some(TextNumber(65)),
        repeated: Vec::new(),
        opaque: OpaqueCount(0),
    };
    assert!(codec.encode(&invalid).is_err());
}

// r[verify descriptors.explicit-proxy]
#[test]
fn compatibility_decode_preserves_proxy_validation_and_primitive_strictness() {
    use phon_engine::{Registry, typed};
    let reader = phon::derive::of::<CheckedCount>().unwrap();
    assert!(
        typed::lower_decode(
            primitive_id(Primitive::U16),
            &reader.descriptor,
            &reader.descriptor_blocks,
            &Registry::new(reader.schemas.clone()),
        )
        .is_err(),
        "the existing no-implicit-widening rule must also apply to proxies"
    );
    let program = typed::lower_decode(
        primitive_id(Primitive::U64),
        &reader.descriptor,
        &reader.descriptor_blocks,
        &Registry::new(reader.schemas),
    )
    .unwrap();
    let mut slot = core::mem::MaybeUninit::<CheckedCount>::uninit();
    // SAFETY: the program is lowered for CheckedCount and the slot is aligned.
    unsafe { typed::decode_with(&program, &42_u64.to_le_bytes(), slot.as_mut_ptr().cast()) }
        .unwrap();
    assert_eq!(
        unsafe { slot.assume_init() },
        CheckedCount::try_from(42).unwrap()
    );
    let mut rejected = core::mem::MaybeUninit::<CheckedCount>::uninit();
    assert!(
        unsafe {
            typed::decode_with(
                &program,
                &256_u64.to_le_bytes(),
                rejected.as_mut_ptr().cast(),
            )
        }
        .is_err()
    );
}

#[derive(Debug, PartialEq, Facet)]
#[repr(transparent)]
#[facet(transparent)]
struct TransparentText(String);

// r[verify descriptors.explicit-proxy]
#[test]
fn transparent_string_without_proxy_keeps_its_wire_mapping() {
    let codec = Codec::<TransparentText>::new().unwrap();
    let string = Codec::<String>::new().unwrap();
    let value = TransparentText("unchanged".into());
    let bytes = codec.encode(&value).unwrap();
    assert_eq!(bytes, string.encode(&value.0).unwrap());
    assert_eq!(codec.decode(&bytes).unwrap(), value);
}

macro_rules! unsupported_proxy {
    ($name:ident, $representation:ty) => {
        struct $name;
        // SAFETY: opaque ZST layout; conversions are deliberately unreachable
        // because derivation must reject this representation before execution.
        unsafe impl Facet<'_> for $name {
            const SHAPE: &'static facet::Shape = &const {
                facet::ShapeBuilder::for_sized::<Self>(stringify!($name))
                    .ty(facet::Type::User(facet::UserType::Opaque))
                    .def(facet::Def::Scalar)
                    .proxy(&facet::ProxyDef {
                        shape: <$representation as Facet>::SHAPE,
                        convert_in: |_, _| unreachable!("unsupported proxy conversion"),
                        convert_out: |_, _| unreachable!("unsupported proxy conversion"),
                    })
                    .build()
            };
        }
    };
}

unsupported_proxy!(CompositeProxy, Vec<u8>);
unsupported_proxy!(BorrowedProxy, &'static str);
unsupported_proxy!(ChainedProxy, TextNumber);

// r[verify descriptors.explicit-proxy]
#[test]
fn unsupported_proxy_representations_fail_during_derivation() {
    assert!(phon::derive::of::<CompositeProxy>().is_err());
    assert!(phon::derive::of::<BorrowedProxy>().is_err());
    assert!(phon::derive::of::<ChainedProxy>().is_err());
    assert!(phon::derive::of::<Option<CompositeProxy>>().is_err());
    assert!(phon::derive::of::<Vec<ChainedProxy>>().is_err());
}

// r[verify descriptors.explicit-proxy]
#[test]
fn maps_and_nested_sequences_return_conversion_errors_and_remain_reusable() {
    use std::collections::BTreeMap;
    let codec = Codec::<BTreeMap<u32, Vec<TextNumber>>>::new().unwrap();
    let valid = BTreeMap::from([(1, vec![TextNumber(2), TextNumber(42)])]);
    let invalid = BTreeMap::from([(1, vec![TextNumber(2), TextNumber(65)])]);
    assert!(codec.encode(&invalid).is_err());
    let bytes = codec.encode(&valid).unwrap();
    assert_eq!(codec.decode(&bytes).unwrap(), valid);
}

#[derive(Debug, Clone, PartialEq, Facet)]
struct ZeroWire {
    unit: (),
}

// r[verify descriptors.explicit-proxy]
#[test]
fn proxy_graph_preserves_zero_wire_collection_elements() {
    let value = (CheckedCount::try_from(42).unwrap(), vec![(); 64]);
    let codec = Codec::<(CheckedCount, Vec<()>)>::new().unwrap();
    let bytes = codec.encode(&value).unwrap();
    assert_eq!(
        bytes,
        Codec::<(u64, Vec<()>)>::new()
            .unwrap()
            .encode(&(42, vec![(); 64]))
            .unwrap()
    );
    assert_eq!(codec.decode(&bytes).unwrap(), value);

    let records = (
        CheckedCount::try_from(42).unwrap(),
        vec![ZeroWire { unit: () }; 64],
    );
    let codec = Codec::<(CheckedCount, Vec<ZeroWire>)>::new().unwrap();
    assert_eq!(
        codec.decode(&codec.encode(&records).unwrap()).unwrap(),
        records
    );

    let arrays = (CheckedCount::try_from(42).unwrap(), vec![[(); 3]; 64]);
    let codec = Codec::<(CheckedCount, Vec<[(); 3]>)>::new().unwrap();
    assert_eq!(
        codec.decode(&codec.encode(&arrays).unwrap()).unwrap(),
        arrays
    );

    let set = (
        CheckedCount::try_from(42).unwrap(),
        std::collections::BTreeSet::from([()]),
    );
    let codec = Codec::<(CheckedCount, std::collections::BTreeSet<()>)>::new().unwrap();
    assert_eq!(codec.decode(&codec.encode(&set).unwrap()).unwrap(), set);
}
