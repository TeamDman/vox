//! Independent Rust oracle for Java's Taxon identity and PHONSCM1 implementation.
use phon_schema::{
    ChannelDirection, Primitive, QualifiedName, Schema, SchemaId, SchemaKind, SchemaRef,
    primitive_id, region_ref_v1_schema, resolve_ids, schema_bundle_from_bytes,
    schema_bundle_to_bytes, SchemaBundleLimits,
};

fn node(kind: SchemaKind, params: &[&str]) -> Schema {
    resolve_ids(vec![Schema {
        id: SchemaId::from_raw(0),
        type_params: params.iter().map(|s| (*s).to_string()).collect(),
        kind,
    }]).remove(0)
}

#[test]
fn java_cutover_bundles_match_rust() {
    let u32_ref = SchemaRef::concrete(primitive_id(Primitive::U32));
    let cases = [
        ("region-ref", region_ref_v1_schema()),
        ("semantic-u32", node(SchemaKind::Semantic {
            name: QualifiedName::try_from("com.example.counter-v1").unwrap().into(),
            args: vec![], representation: u32_ref.clone(),
        }, &[])),
        ("generic-tuple", node(SchemaKind::Tuple {
            elements: vec![SchemaRef::var("T"), SchemaRef::var("U")],
        }, &["T", "U"])),
        ("channel-tx", node(SchemaKind::Channel {
            direction: ChannelDirection::Tx, element: u32_ref.clone(),
        }, &[])),
        ("channel-rx", node(SchemaKind::Channel {
            direction: ChannelDirection::Rx, element: u32_ref.clone(),
        }, &[])),
        ("external-none", node(SchemaKind::External {
            kind: "com.example.handle".into(), metadata: None,
        }, &[])),
        ("external-metadata", node(SchemaKind::External {
            kind: "com.example.handle".into(), metadata: Some(u32_ref),
        }, &[])),
    ];
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/java-cutover");
    for (name, schema) in cases {
        let bytes = schema_bundle_to_bytes(std::slice::from_ref(&schema)).unwrap();
        let decoded = schema_bundle_from_bytes(&bytes, SchemaBundleLimits::default()).unwrap();
        assert_eq!(decoded.schemas(), &[schema]);
        let path = directory.join(format!("{name}.phon"));
        if std::env::var_os("PHON_UPDATE_JAVA_CUTOVER_GOLDENS").is_some() {
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(&path, &bytes).unwrap();
        }
        assert_eq!(std::fs::read(path).expect("checked-in Rust/Java fixture"), bytes, "{name}");
    }
}
