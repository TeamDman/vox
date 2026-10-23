use crate::{
    Field, Primitive, QualifiedName, Schema, SchemaId, SchemaKind, SchemaRef, Variant,
    VariantPayload, primitive_id, resolve_ids,
};

pub const REGION_REF_V1_NAME: &str = "org.bearcove.phon.region-ref-v1";
pub const LIST_OFFSETS_V1_NAME: &str = "org.bearcove.phon.list-offsets-v1";
pub const LIST_TARGET_V1_SCHEMA_NAME: &str = "org.bearcove.phon.ListTargetV1";
pub const LIST_OFFSETS_V1_SCHEMA_NAME: &str = "org.bearcove.phon.ListOffsetsV1";

// r[impl compact.file.explicit-regions]
// r[impl type-system.semantic]
#[must_use]
pub fn region_ref_v1_schema() -> Schema {
    resolve_ids(vec![Schema {
        id: SchemaId::from_raw(0),
        type_params: vec!["T".to_string()],
        kind: SchemaKind::Semantic {
            name: QualifiedName::try_from(REGION_REF_V1_NAME)
                .expect("canonical built-in qualified name")
                .into(),
            args: vec![SchemaRef::var("T")],
            representation: SchemaRef::concrete(primitive_id(Primitive::U32)),
        },
    }])
    .remove(0)
}

// r[impl compact.file.admission]
#[must_use]
pub fn list_offsets_v1_schemas() -> Vec<Schema> {
    let list_u64_key = SchemaId::from_raw(3);
    let list_target_key = SchemaId::from_raw(1);
    resolve_ids(vec![
        Schema {
            id: list_target_key,
            type_params: Vec::new(),
            kind: SchemaKind::Enum {
                name: LIST_TARGET_V1_SCHEMA_NAME.to_string(),
                variants: vec![
                    Variant {
                        name: "Root".to_string(),
                        index: 0,
                        payload: VariantPayload::Unit,
                    },
                    Variant {
                        name: "Region".to_string(),
                        index: 1,
                        payload: VariantPayload::Newtype(SchemaRef::concrete(primitive_id(
                            Primitive::U32,
                        ))),
                    },
                ],
            },
        },
        Schema {
            id: list_u64_key,
            type_params: Vec::new(),
            kind: SchemaKind::List {
                element: SchemaRef::concrete(primitive_id(Primitive::U64)),
            },
        },
        Schema {
            id: SchemaId::from_raw(2),
            type_params: Vec::new(),
            kind: SchemaKind::Struct {
                name: LIST_OFFSETS_V1_SCHEMA_NAME.to_string(),
                fields: vec![
                    Field {
                        name: "target".to_string(),
                        schema: SchemaRef::concrete(list_target_key),
                        required: true,
                    },
                    Field {
                        name: "offsets".to_string(),
                        schema: SchemaRef::concrete(list_u64_key),
                        required: true,
                    },
                ],
            },
        },
    ])
}

// r[impl compact.file.admission]
#[must_use]
pub const fn list_offsets_aux_number(target_index: u32) -> u32 {
    target_index
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{schema_bundle_to_bytes, schema_to_bytes};

    fn write_region_ref_schema_golden_if_requested(bytes: &[u8]) {
        if std::env::var_os("PHON_UPDATE_GOLDEN").is_some() {
            std::fs::write(
                concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/testdata/schema-semantic-region-ref-v1.phon"
                ),
                bytes,
            )
            .unwrap();
        }
    }

    fn list_builtins() -> Vec<Schema> {
        list_offsets_v1_schemas()
    }

    fn schema_id_named(schemas: &[Schema], expected: &str) -> SchemaId {
        schemas
            .iter()
            .find_map(|schema| match &schema.kind {
                SchemaKind::Struct { name, .. } | SchemaKind::Enum { name, .. }
                    if name == expected =>
                {
                    Some(schema.id)
                }
                _ => None,
            })
            .unwrap()
    }
    // r[verify compact.file.explicit-regions]
    // r[verify type-system.semantic]
    #[test]
    fn builtins_have_exact_canonical_shapes() {
        let region_ref = region_ref_v1_schema();
        assert_eq!(region_ref.type_params, ["T"]);
        match &region_ref.kind {
            SchemaKind::Semantic {
                name,
                args,
                representation,
            } => {
                assert_eq!(name.as_str(), REGION_REF_V1_NAME);
                assert_eq!(args, &[SchemaRef::var("T")]);
                assert_eq!(
                    representation,
                    &SchemaRef::concrete(primitive_id(Primitive::U32))
                );
            }
            other => panic!("unexpected RegionRef kind: {other:?}"),
        }

        let schemas = list_builtins();
        assert_eq!(schemas.len(), 3);
        let target_id = schema_id_named(&schemas, LIST_TARGET_V1_SCHEMA_NAME);
        let target = schemas
            .iter()
            .find(|schema| schema.id == target_id)
            .unwrap();
        let offsets_id = schema_id_named(&schemas, LIST_OFFSETS_V1_SCHEMA_NAME);
        let offsets = schemas
            .iter()
            .find(|schema| schema.id == offsets_id)
            .unwrap();
        match &target.kind {
            SchemaKind::Enum { variants, .. } => {
                assert_eq!(variants.len(), 2);
                assert_eq!(variants[0].name, "Root");
                assert_eq!(variants[0].index, 0);
                assert_eq!(variants[0].payload, VariantPayload::Unit);
                assert_eq!(variants[1].name, "Region");
                assert_eq!(variants[1].index, 1);
                assert_eq!(
                    variants[1].payload,
                    VariantPayload::Newtype(SchemaRef::concrete(primitive_id(Primitive::U32)))
                );
            }
            _ => unreachable!(),
        }
        match &offsets.kind {
            SchemaKind::Struct { fields, .. } => {
                assert_eq!(fields.len(), 2);
                assert_eq!(fields[0].name, "target");
                assert_eq!(fields[0].schema, SchemaRef::concrete(target_id));
                assert!(fields[0].required);
                assert_eq!(fields[1].name, "offsets");
                assert!(fields[1].required);
                let SchemaRef::Concrete { id, .. } = fields[1].schema else {
                    unreachable!();
                };
                let list = schemas.iter().find(|schema| schema.id == id).unwrap();
                assert_eq!(
                    list.kind,
                    SchemaKind::List {
                        element: SchemaRef::concrete(primitive_id(Primitive::U64))
                    }
                );
            }
            _ => unreachable!(),
        }
    }

    // r[verify compact.file.admission]
    #[test]
    fn builtins_have_canonical_ids_and_golden_bytes() {
        let region_ref = region_ref_v1_schema();
        let list_offsets = list_builtins();
        assert_eq!(region_ref.id.as_u64(), 0xa1c7_6c5b_c4e6_7e1a);
        assert_eq!(
            schema_id_named(&list_offsets, LIST_TARGET_V1_SCHEMA_NAME).as_u64(),
            0x6884_7f90_5821_353e
        );
        assert_eq!(
            schema_id_named(&list_offsets, LIST_OFFSETS_V1_SCHEMA_NAME).as_u64(),
            0xf3bb_a25b_02fc_24f3
        );
        let region_ref_bytes = schema_to_bytes(&region_ref);
        write_region_ref_schema_golden_if_requested(&region_ref_bytes);
        assert_eq!(
            blake3::hash(&region_ref_bytes).to_hex().as_str(),
            "5883d9eb48d532a92e363f295e28411edea871296b784ee85d15b3550c87deec"
        );
        assert_eq!(
            blake3::hash(&schema_bundle_to_bytes(&list_offsets).unwrap())
                .to_hex()
                .as_str(),
            "f174aaee648d3ae17c0fb5544996ac1aa841c2fb3aded47e076ddad080447001"
        );
    }

    // r[verify compact.file.admission]
    #[test]
    fn every_identity_input_changes_a_builtin_id() {
        let canonical = list_builtins();
        let root_index = canonical
            .iter()
            .position(|schema| {
                matches!(&schema.kind, SchemaKind::Struct { name, .. } if name == LIST_OFFSETS_V1_SCHEMA_NAME)
            })
            .unwrap();
        let target_index = canonical
            .iter()
            .position(|schema| {
                matches!(&schema.kind, SchemaKind::Enum { name, .. } if name == LIST_TARGET_V1_SCHEMA_NAME)
            })
            .unwrap();
        let list_index = canonical
            .iter()
            .position(|schema| matches!(schema.kind, SchemaKind::List { .. }))
            .unwrap();

        for mutation in 0..6 {
            let mut changed = canonical.clone();
            match mutation {
                0 => match &mut changed[target_index].kind {
                    SchemaKind::Enum { variants, .. } => variants[1].index = 2,
                    _ => unreachable!(),
                },
                1 => match &mut changed[target_index].kind {
                    SchemaKind::Enum { variants, .. } => variants[1].name = "Area".to_string(),
                    _ => unreachable!(),
                },
                2 => match &mut changed[root_index].kind {
                    SchemaKind::Struct { fields, .. } => {
                        fields[0].schema = SchemaRef::concrete(primitive_id(Primitive::U32));
                    }
                    _ => unreachable!(),
                },
                3 => match &mut changed[list_index].kind {
                    SchemaKind::List { element } => {
                        *element = SchemaRef::concrete(primitive_id(Primitive::U32));
                    }
                    _ => unreachable!(),
                },
                4 => match &mut changed[root_index].kind {
                    SchemaKind::Struct { fields, .. } => fields.swap(0, 1),
                    _ => unreachable!(),
                },
                5 => match &mut changed[root_index].kind {
                    SchemaKind::Struct { fields, .. } => fields[1].required = false,
                    _ => unreachable!(),
                },
                _ => unreachable!(),
            }
            let resolved = resolve_ids(changed);
            assert_ne!(
                resolved[root_index].id, canonical[root_index].id,
                "mutation {mutation}"
            );
        }
    }

    // r[verify compact.file.admission]
    #[test]
    fn list_offsets_aux_numbering_is_root_then_regions() {
        assert_eq!(
            (0..4).map(list_offsets_aux_number).collect::<Vec<_>>(),
            [0, 1, 2, 3]
        );
    }
}
