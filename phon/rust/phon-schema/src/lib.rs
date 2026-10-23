//! phon's wire contract — the most widely depended-on layer, with no engine and
//! no language binding.
//!
//! This crate owns everything portable about a phon schema: the schema model,
//! content-derived schema identity, the dynamic [`value`] type, and the
//! self-describing codec that bootstraps schema exchange. Every other phon crate
//! depends on this one; it depends on nothing phon-specific.
//!
//! Spec: `docs/content/spec.md` — "Type system", "Schema identity",
//! "Self-describing mode", and `r[crates.concern-separation]`.

// r[impl crates.concern-separation]
pub mod builtins;
pub mod bytes;
pub mod identity;
pub mod qualified_name;
pub mod schema;
pub mod schema_bundle;
pub mod selfdescribing;

/// phon's dynamic value. In Rust this *is* `facet_value::Value`, re-exported
/// rather than wrapped — a `Dynamic` field carries one directly. The
/// self-describing codec maps the cases facet carries beyond the wire tag table
/// (null, date/time, qname, uuid) onto phon kinds.
///
/// Spec: "Value" (`r[value]`).
pub mod value {
    pub use facet_value::Value;
}

pub use builtins::{
    LIST_OFFSETS_V1_NAME, LIST_OFFSETS_V1_SCHEMA_NAME, LIST_TARGET_V1_SCHEMA_NAME,
    REGION_REF_V1_NAME, list_offsets_aux_number, list_offsets_v1_schemas, region_ref_v1_schema,
};
pub use bytes::{DecodeError, Reader};
pub use identity::{primitive_id, recursive_schema_ids, resolve_ids};
pub use qualified_name::{
    QualifiedName, QualifiedNameError, from_compact_bytes as qualified_name_from_compact_bytes,
    from_self_describing_bytes as qualified_name_from_self_describing_bytes,
};
pub use schema::{
    ChannelDirection, Field, Primitive, Schema, SchemaId, SchemaKind, SchemaRef, Variant,
    VariantPayload,
};
pub use schema_bundle::{
    SchemaBundle, SchemaBundleLimits, schema_bundle_from_bytes, schema_bundle_to_bytes,
};
pub use selfdescribing::{
    DecodeLimits, EncodeError, extended_from_string, extended_to_string, read_value,
    schema_from_bytes, schema_matches_bytes, schema_to_bytes, value_from_bytes, value_to_bytes,
    write_value,
};
pub use value::Value;

#[cfg(test)]
mod qualified_name_tests {
    use super::*;

    #[test]
    fn qualified_name_accepts_only_canonical_revision_one_grammar() {
        for valid in [
            "org.bearcove.phon.region-ref-v1",
            "org.bearcove.weavy.bytecode-v1",
            "phon.region-ref-v1",
        ] {
            assert_eq!(QualifiedName::try_from(valid).unwrap().as_str(), valid);
        }

        for invalid in [
            "",
            "local",
            ".org.example",
            "org..example",
            "org.example.",
            "Org.example",
            "org.example_name",
            "org.-example",
            "org.example-",
            "org.example--name",
            "1org.example",
            "org.1example",
            "org.example/thing",
        ] {
            assert!(
                QualifiedName::try_from(invalid).is_err(),
                "accepted {invalid}"
            );
        }
    }

    #[test]
    fn qualified_name_enforces_byte_bound_and_namespace_reservations() {
        let valid = format!("org.{}", "a".repeat(QualifiedName::MAX_BYTES - 4));
        assert_eq!(valid.len(), QualifiedName::MAX_BYTES);
        assert!(QualifiedName::try_from(valid.as_str()).is_ok());

        let too_long = format!("org.{}", "a".repeat(QualifiedName::MAX_BYTES - 3));
        assert_eq!(too_long.len(), QualifiedName::MAX_BYTES + 1);
        assert!(QualifiedName::try_from(too_long.as_str()).is_err());

        assert!(QualifiedName::application("phon.private-v1").is_err());
        assert!(QualifiedName::application("org.bearcove.phon.private-v1").is_err());
        assert!(QualifiedName::application("org.example.private-v1").is_ok());
    }

    #[test]
    fn qualified_name_wire_forms_are_byte_exact() {
        let name = QualifiedName::try_from("org.example.x-v1").unwrap();
        assert_eq!(name.compact_bytes(), b"\x10\0\0\0org.example.x-v1".to_vec());
        assert_eq!(
            name.self_describing_bytes(),
            b"\x0f\x10\0\0\0org.example.x-v1".to_vec()
        );
    }

    #[test]
    fn qualified_name_decoders_reject_truncation_and_noncanonical_bytes() {
        let canonical = b"\x10\0\0\0org.example.x-v1";
        assert_eq!(
            qualified_name_from_compact_bytes(canonical)
                .unwrap()
                .as_str(),
            "org.example.x-v1"
        );
        assert_eq!(
            qualified_name_from_self_describing_bytes(b"\x0f\x10\0\0\0org.example.x-v1")
                .unwrap()
                .as_str(),
            "org.example.x-v1"
        );

        for bytes in [&canonical[..0], &canonical[..3]] {
            assert!(matches!(
                qualified_name_from_compact_bytes(bytes),
                Err(DecodeError::UnexpectedEof { .. })
            ));
        }
        assert!(matches!(
            qualified_name_from_compact_bytes(&canonical[..8]),
            Err(DecodeError::LengthTooLarge { .. })
        ));
        assert!(matches!(
            qualified_name_from_compact_bytes(b"\x0b\0\0\0Org.example"),
            Err(DecodeError::Malformed("qualified name"))
        ));
        assert!(matches!(
            qualified_name_from_self_describing_bytes(b"\x10\0\0\0\0"),
            Err(DecodeError::UnexpectedTag {
                expected: "qualified name string",
                got: 0x10
            })
        ));
    }
}

#[cfg(test)]
mod schema_bundle_tests {
    use super::*;

    fn point_schema() -> Schema {
        resolve_ids(vec![Schema {
            id: SchemaId::from_raw(1),
            type_params: Vec::new(),
            kind: SchemaKind::Struct {
                name: "Point".to_string(),
                fields: vec![Field {
                    name: "x".to_string(),
                    schema: SchemaRef::concrete(primitive_id(Primitive::U32)),
                    required: true,
                }],
            },
        }])
        .remove(0)
    }

    fn write_point_golden_if_requested(bytes: &[u8]) {
        if std::env::var_os("PHON_UPDATE_GOLDEN").is_some() {
            std::fs::write(
                concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/testdata/schema-bundle-point-v1.phon"
                ),
                bytes,
            )
            .unwrap();
        }
    }

    #[test]
    fn schema_bundle_v1_roundtrips_byte_identically() {
        let bytes = schema_bundle_to_bytes(&[point_schema()]).unwrap();
        let bundle = schema_bundle_from_bytes(&bytes, SchemaBundleLimits::default()).unwrap();
        assert_eq!(bundle.schemas(), &[point_schema()]);
        assert_eq!(schema_bundle_to_bytes(bundle.schemas()).unwrap(), bytes);
    }

    #[test]
    fn schema_bundle_v1_rejects_noncanonical_string_table() {
        let bytes = schema_bundle_to_bytes(&[point_schema()]).unwrap();
        let point = bytes
            .windows(5)
            .position(|window| window == b"Point")
            .expect("Point string");
        let mut corrupt = bytes.clone();
        corrupt[point] = b'Z';
        assert!(schema_bundle_from_bytes(&corrupt, SchemaBundleLimits::default()).is_err());
    }

    #[test]
    fn schema_bundle_v1_rejects_every_truncation() {
        let bytes = schema_bundle_to_bytes(&[point_schema()]).unwrap();
        for end in 0..bytes.len() {
            assert!(
                schema_bundle_from_bytes(&bytes[..end], SchemaBundleLimits::default()).is_err(),
                "accepted truncation at byte {end}"
            );
        }
    }

    #[test]
    fn schema_bundle_v1_rejects_unknown_version_tag_wrong_id_and_trailing_bytes() {
        let bytes = schema_bundle_to_bytes(&[point_schema()]).unwrap();

        let mut wrong_version = bytes.clone();
        wrong_version[8..12].copy_from_slice(&2_u32.to_le_bytes());
        assert!(schema_bundle_from_bytes(&wrong_version, SchemaBundleLimits::default()).is_err());

        let mut unknown_tag = bytes.clone();
        unknown_tag[12] = 0xff;
        assert!(schema_bundle_from_bytes(&unknown_tag, SchemaBundleLimits::default()).is_err());

        let mut wrong_id = bytes.clone();
        let id = point_schema().id.as_u64().to_le_bytes();
        let id_offset = wrong_id
            .windows(id.len())
            .position(|window| window == id)
            .unwrap();
        wrong_id[id_offset] ^= 1;
        assert!(schema_bundle_from_bytes(&wrong_id, SchemaBundleLimits::default()).is_err());

        let mut trailing = bytes;
        trailing.push(0);
        assert!(matches!(
            schema_bundle_from_bytes(&trailing, SchemaBundleLimits::default()),
            Err(DecodeError::TrailingBytes(1))
        ));
    }

    #[test]
    fn schema_bundle_v1_enforces_total_and_owned_byte_limits() {
        let bytes = schema_bundle_to_bytes(&[point_schema()]).unwrap();
        let limits = SchemaBundleLimits {
            max_total_bytes: bytes.len() - 1,
            ..SchemaBundleLimits::default()
        };
        assert!(matches!(
            schema_bundle_from_bytes(&bytes, limits),
            Err(DecodeError::OwnedBytesLimitExceeded { .. })
        ));

        let limits = SchemaBundleLimits {
            max_owned_bytes: 5,
            ..SchemaBundleLimits::default()
        };
        assert!(matches!(
            schema_bundle_from_bytes(&bytes, limits),
            Err(DecodeError::OwnedBytesLimitExceeded { .. })
        ));
    }

    #[test]
    fn schema_bundle_v1_rejects_duplicate_ids_and_invalid_reference_closure() {
        let point = point_schema();
        assert!(matches!(
            schema_bundle_to_bytes(&[point.clone(), point.clone()]),
            Err(DecodeError::Malformed("duplicate schema id"))
        ));

        let missing = resolve_ids(vec![Schema {
            id: SchemaId::from_raw(1),
            type_params: Vec::new(),
            kind: SchemaKind::List {
                element: SchemaRef::concrete(SchemaId::from_raw(0xdead_beef)),
            },
        }]);
        assert!(matches!(
            schema_bundle_to_bytes(&missing),
            Err(DecodeError::Malformed("missing schema reference"))
        ));
    }

    #[test]
    fn schema_bundle_v1_rejects_generic_and_semantic_contract_mismatches() {
        let generic = resolve_ids(vec![
            Schema {
                id: SchemaId::from_raw(1),
                type_params: vec!["T".to_string()],
                kind: SchemaKind::Struct {
                    name: "Box".to_string(),
                    fields: vec![Field {
                        name: "value".to_string(),
                        schema: SchemaRef::var("T"),
                        required: true,
                    }],
                },
            },
            Schema {
                id: SchemaId::from_raw(2),
                type_params: Vec::new(),
                kind: SchemaKind::List {
                    element: SchemaRef::concrete(SchemaId::from_raw(1)),
                },
            },
        ]);
        assert!(matches!(
            schema_bundle_to_bytes(&generic),
            Err(DecodeError::Malformed("schema argument arity"))
        ));

        let semantic = resolve_ids(vec![Schema {
            id: SchemaId::from_raw(1),
            type_params: Vec::new(),
            kind: SchemaKind::Semantic {
                name: taxon::SemanticName::try_from("Org.Invalid").unwrap(),
                args: Vec::new(),
                representation: SchemaRef::concrete(primitive_id(Primitive::U32)),
            },
        }]);
        assert!(matches!(
            schema_bundle_to_bytes(&semantic),
            Err(DecodeError::Malformed("qualified name"))
        ));
    }

    #[test]
    fn schema_bundle_v1_admits_recursive_cycle_closure() {
        let recursive = resolve_ids(vec![Schema {
            id: SchemaId::from_raw(7),
            type_params: Vec::new(),
            kind: SchemaKind::Struct {
                name: "Node".to_string(),
                fields: vec![Field {
                    name: "next".to_string(),
                    schema: SchemaRef::concrete(SchemaId::from_raw(7)),
                    required: false,
                }],
            },
        }]);
        let bytes = schema_bundle_to_bytes(&recursive).unwrap();
        assert_eq!(
            schema_bundle_from_bytes(&bytes, SchemaBundleLimits::default())
                .unwrap()
                .schemas(),
            recursive
        );
    }

    #[test]
    fn schema_bundle_v1_bytes_are_golden() {
        const GOLDEN: &[u8] = include_bytes!("../testdata/schema-bundle-point-v1.phon");
        let bytes = schema_bundle_to_bytes(&[point_schema()]).unwrap();
        write_point_golden_if_requested(&bytes);
        assert_eq!(bytes, GOLDEN);
        assert_eq!(
            blake3::hash(GOLDEN).to_hex().as_str(),
            "be73e4dfa8001fbe86bcc910d60d0c70d746ffb7ad888c3f994b1ade50637411"
        );
    }
}
