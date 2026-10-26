//! Independent public-API checks for the proposed proxy implementation.
//! Run serially: the Drop oracle intentionally shares one counter.
use facet::Facet;
use phon::api::Codec;
use std::sync::atomic::{AtomicUsize, Ordering};

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
static SEEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
static DROPS: AtomicUsize = AtomicUsize::new(0);

// r[verify descriptors.explicit-proxy]
#[test]
fn missing_owner_destruction_contract_is_rejected_before_decoding() {
    let mut derived = phon::derive::of::<OwnedProxy>().unwrap();
    derived.descriptor.drop = None;
    assert!(
        phon_engine::typed::lower_typed(
            &derived.descriptor,
            &derived.descriptor_blocks,
            &phon_engine::Registry::new(derived.schemas),
        )
        .is_err()
    );
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Facet)]
#[facet(proxy = String)]
struct OwnedProxy(String);

impl Drop for OwnedProxy {
    fn drop(&mut self) {
        SEEN.lock().unwrap().push(self.0.clone());
        DROPS.fetch_add(1, Ordering::SeqCst);
        assert_ne!(self.0, "drop-panic", "test destructor panic");
    }
}

// r[verify descriptors.explicit-proxy]
#[test]
fn panicking_destructor_still_cleans_other_initialized_owners_once() {
    let _serial = SERIAL.lock().unwrap();
    clear_drops();
    let codec = Codec::<Vec<OwnedProxy>>::new().unwrap();
    let bytes = Codec::<Vec<String>>::new()
        .unwrap()
        .encode(&vec![
            "first".into(),
            "drop-panic".into(),
            "third".into(),
            "reject".into(),
        ])
        .unwrap();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| codec.decode(&bytes))).is_err()
    );
    assert_dropped(&["first", "drop-panic", "third"]);
}

impl TryFrom<String> for OwnedProxy {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value == "panic" {
            panic!("proxy conversion panic")
        }
        if value == "reject" {
            Err("rejected by validation")
        } else {
            Ok(Self(value))
        }
    }
}

impl TryFrom<&OwnedProxy> for String {
    type Error = &'static str;
    fn try_from(value: &OwnedProxy) -> Result<Self, Self::Error> {
        if value.0 == "encode-reject" {
            Err("rejected encode")
        } else {
            Ok(value.0.clone())
        }
    }
}

#[test]
fn encode_borrows_owned_source_even_when_conversion_fails() {
    let _serial = SERIAL.lock().unwrap();
    clear_drops();
    DROPS.store(0, Ordering::SeqCst);
    let source = OwnedProxy("encode-reject".into());
    assert!(Codec::<OwnedProxy>::new().unwrap().encode(&source).is_err());
    assert_eq!(DROPS.load(Ordering::SeqCst), 0);
    assert_eq!(source.0, "encode-reject");
    drop(source);
    assert_eq!(DROPS.load(Ordering::SeqCst), 1);
}

#[test]
fn rejected_later_element_drops_already_initialized_proxy_targets() {
    let _serial = SERIAL.lock().unwrap();
    clear_drops();
    DROPS.store(0, Ordering::SeqCst);
    let codec = Codec::<Vec<OwnedProxy>>::new().unwrap();
    let wire = Codec::<Vec<String>>::new().unwrap();
    let valid = wire.encode(&vec!["accepted".into()]).unwrap();
    drop(codec.decode(&valid).unwrap());
    assert_eq!(
        DROPS.load(Ordering::SeqCst),
        1,
        "Drop oracle must observe successful ownership"
    );
    DROPS.store(0, Ordering::SeqCst);
    let bytes = wire
        .encode(&vec!["accepted".into(), "reject".into()])
        .unwrap();
    assert!(codec.decode(&bytes).is_err());
    assert_eq!(
        DROPS.load(Ordering::SeqCst),
        1,
        "initialized earlier elements must not leak on rejection"
    );
}

#[test]
fn trailing_wire_bytes_drop_initialized_owned_proxy_target() {
    let _serial = SERIAL.lock().unwrap();
    clear_drops();
    DROPS.store(0, Ordering::SeqCst);
    let codec = Codec::<OwnedProxy>::new().unwrap();
    let mut bytes = Codec::<String>::new()
        .unwrap()
        .encode(&"accepted".into())
        .unwrap();
    bytes.push(0);
    assert!(codec.decode(&bytes).is_err());
    assert_eq!(
        DROPS.load(Ordering::SeqCst),
        1,
        "completed value must be dropped before returning trailing-data error"
    );
}

#[derive(Debug, PartialEq, Facet)]
#[repr(C, align(64))]
#[facet(proxy = u128)]
struct Aligned {
    unrelated: [u8; 9],
    high: u64,
    low: u64,
}

impl From<u128> for Aligned {
    fn from(value: u128) -> Self {
        Self {
            unrelated: [99; 9],
            high: (value >> 64) as u64,
            low: value as u64,
        }
    }
}

impl From<&Aligned> for u128 {
    fn from(value: &Aligned) -> Self {
        (u128::from(value.high) << 64) | u128::from(value.low)
    }
}

#[test]
fn aligned_owner_and_u128_proxy_have_distinct_safe_layouts() {
    let _serial = SERIAL.lock().unwrap();
    clear_drops();
    let value = (1_u128 << 127) | 42;
    let codec = Codec::<Aligned>::new().unwrap();
    let encoded = codec.encode(&Aligned::from(value)).unwrap();
    assert_eq!(encoded, value.to_le_bytes());
    assert_eq!(codec.decode(&encoded).unwrap(), Aligned::from(value));
    let nested = Codec::<Vec<Option<Aligned>>>::new().unwrap();
    let values = vec![None, Some(Aligned::from(value)), Some(Aligned::from(0))];
    assert_eq!(
        nested.decode(&nested.encode(&values).unwrap()).unwrap(),
        values
    );
}

use std::collections::{BTreeMap, BTreeSet};

fn assert_dropped(expected: &[&str]) {
    let mut actual = SEEN.lock().unwrap().clone();
    actual.sort();
    let mut expected = expected.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    expected.sort();
    assert_eq!(
        actual, expected,
        "every initialized owner must drop exactly once"
    );
}

fn clear_drops() {
    SEEN.lock().unwrap().clear();
    DROPS.store(0, Ordering::SeqCst);
}

#[derive(Facet, Debug)]
struct EarlyFields<T> {
    first: OwnedProxy,
    nested: T,
    last: OwnedProxy,
}

#[derive(Facet)]
struct WireFields<T> {
    first: String,
    nested: T,
    last: String,
}

#[test]
fn nested_option_result_box_arrays_and_earlier_fields_unwind_once() {
    let _serial = SERIAL.lock().unwrap();
    clear_drops();
    type Target = EarlyFields<Option<Box<Result<[OwnedProxy; 2], OwnedProxy>>>>;
    type Wire = WireFields<Option<Box<Result<[String; 2], String>>>>;
    let codec = Codec::<Target>::new().unwrap();
    let wire = Codec::<Wire>::new().unwrap();
    for (a, b, last, expected) in [
        ("reject", "b", "last", vec!["first"]),
        ("a", "reject", "last", vec!["first", "a"]),
        ("a", "b", "reject", vec!["first", "a", "b"]),
    ] {
        clear_drops();
        let bytes = wire
            .encode(&Wire {
                first: "first".into(),
                nested: Some(Box::new(Ok([a.into(), b.into()]))),
                last: last.into(),
            })
            .unwrap();
        assert!(codec.decode(&bytes).is_err());
        assert_dropped(&expected);
    }
    clear_drops();
    let mut bytes = wire
        .encode(&Wire {
            first: "first".into(),
            nested: Some(Box::new(Ok(["a".into(), "b".into()]))),
            last: "last".into(),
        })
        .unwrap();
    drop(codec.decode(&bytes).unwrap());
    assert_dropped(&["first", "a", "b", "last"]);
    clear_drops();
    bytes.push(0);
    assert!(codec.decode(&bytes).is_err());
    assert_dropped(&["first", "a", "b", "last"]);
}

#[test]
fn maps_drop_inserted_entries_and_uninserted_key_on_rejected_value() {
    let _serial = SERIAL.lock().unwrap();
    clear_drops();
    let codec = Codec::<BTreeMap<OwnedProxy, Vec<OwnedProxy>>>::new().unwrap();
    let wire = Codec::<BTreeMap<String, Vec<String>>>::new().unwrap();
    let value = BTreeMap::from([
        ("a-key".into(), vec!["a-value".into()]),
        ("b-key".into(), vec!["b-value".into(), "reject".into()]),
    ]);
    clear_drops();
    assert!(codec.decode(&wire.encode(&value).unwrap()).is_err());
    assert_dropped(&["a-key", "a-value", "b-key", "b-value"]);
    clear_drops();
    let valid = BTreeMap::from([
        ("a-key".into(), vec!["a-value".into()]),
        ("b-key".into(), vec!["b-value".into()]),
    ]);
    let mut bytes = wire.encode(&valid).unwrap();
    drop(codec.decode(&bytes).unwrap());
    assert_dropped(&["a-key", "a-value", "b-key", "b-value"]);
    clear_drops();
    bytes.push(0);
    assert!(codec.decode(&bytes).is_err());
    assert_dropped(&["a-key", "a-value", "b-key", "b-value"]);
}

#[test]
fn sets_drop_prior_entries_on_rejection_and_all_entries_on_trailing_bytes() {
    let _serial = SERIAL.lock().unwrap();
    clear_drops();
    let codec = Codec::<BTreeSet<OwnedProxy>>::new().unwrap();
    let wire = Codec::<BTreeSet<String>>::new().unwrap();
    clear_drops();
    assert!(
        codec
            .decode(
                &wire
                    .encode(&BTreeSet::from(["a".into(), "b".into(), "reject".into()]))
                    .unwrap()
            )
            .is_err()
    );
    assert_dropped(&["a", "b"]);
    clear_drops();
    let mut bytes = wire
        .encode(&BTreeSet::from(["a".into(), "b".into()]))
        .unwrap();
    bytes.push(0);
    assert!(codec.decode(&bytes).is_err());
    assert_dropped(&["a", "b"]);
}

#[test]
fn proxy_panic_unwinds_completed_elements_without_dropping_uninitialized_target() {
    let _serial = SERIAL.lock().unwrap();
    clear_drops();
    let codec = Codec::<Vec<OwnedProxy>>::new().unwrap();
    let bytes = Codec::<Vec<String>>::new()
        .unwrap()
        .encode(&vec!["a".into(), "b".into(), "panic".into()])
        .unwrap();
    clear_drops();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| codec.decode(&bytes))).is_err()
    );
    assert_dropped(&["a", "b"]);
}

#[test]
fn truncated_second_string_cleans_prior_completed_owner() {
    let _serial = SERIAL.lock().unwrap();
    clear_drops();
    let codec = Codec::<Vec<OwnedProxy>>::new().unwrap();
    let wire = Codec::<Vec<String>>::new().unwrap();
    let bytes = wire.encode(&vec!["first".into(), "second".into()]).unwrap();
    clear_drops();
    assert!(codec.decode(&bytes[..bytes.len() - 1]).is_err());
    assert_dropped(&["first"]);
}

#[derive(Facet, Debug)]
#[repr(C, u8)]
enum Sum<T> {
    Pair(OwnedProxy, T),
    Empty,
}

#[derive(Facet)]
#[repr(C, u8)]
enum WireSum<T> {
    Pair(String, T),
    Empty,
}

#[test]
fn partial_enum_payload_never_runs_the_uninitialized_enums_destructor() {
    let _serial = SERIAL.lock().unwrap();
    clear_drops();
    let codec = Codec::<Sum<Vec<OwnedProxy>>>::new().unwrap();
    let wire = Codec::<WireSum<Vec<String>>>::new().unwrap();
    clear_drops();
    let value = WireSum::Pair("first".into(), vec!["a".into(), "reject".into()]);
    let WireSum::Pair(first, _) = &value else {
        panic!("pair")
    };
    assert_eq!(first, "first");
    let bytes = wire.encode(&value).unwrap();
    assert!(codec.decode(&bytes).is_err());
    assert_dropped(&["first", "a"]);
    clear_drops();
    let bytes = wire
        .encode(&WireSum::Pair("first".into(), vec!["a".into()]))
        .unwrap();
    let value = codec.decode(&bytes).unwrap();
    let Sum::Pair(first, rest) = &value else {
        panic!("pair")
    };
    assert_eq!(first.0, "first");
    assert_eq!(rest.len(), 1);
    drop(value);
    assert_dropped(&["first", "a"]);
}

#[derive(Facet, Debug)]
struct Tree {
    value: OwnedProxy,
    children: Vec<Tree>,
}

#[derive(Facet)]
struct WireTree {
    value: String,
    children: Vec<WireTree>,
}

#[test]
fn recursive_blocks_transfer_ownership_without_double_drop() {
    let _serial = SERIAL.lock().unwrap();
    let codec = Codec::<Tree>::new().unwrap();
    let wire = Codec::<WireTree>::new().unwrap();
    let make = |last: &str| WireTree {
        value: "root".into(),
        children: vec![
            WireTree {
                value: "child".into(),
                children: vec![WireTree {
                    value: "grandchild".into(),
                    children: vec![],
                }],
            },
            WireTree {
                value: last.into(),
                children: vec![],
            },
        ],
    };
    clear_drops();
    assert!(
        codec
            .decode(&wire.encode(&make("reject")).unwrap())
            .is_err()
    );
    assert_dropped(&["root", "child", "grandchild"]);
    clear_drops();
    let mut bytes = wire.encode(&make("last")).unwrap();
    let decoded = codec.decode(&bytes).unwrap();
    assert_eq!(decoded.value.0, "root");
    assert_eq!(decoded.children.len(), 2);
    drop(decoded);
    assert_dropped(&["root", "child", "grandchild", "last"]);
    clear_drops();
    bytes.push(0);
    assert!(codec.decode(&bytes).is_err());
    assert_dropped(&["root", "child", "grandchild", "last"]);
}

#[derive(Facet)]
struct BorrowingOwner<'a> {
    value: &'a OwnedProxy,
    guard: OwnedProxy,
}

impl Drop for BorrowingOwner<'_> {
    fn drop(&mut self) {
        assert_eq!(self.value.0, "pointee");
        assert!(
            !SEEN.lock().unwrap().iter().any(|s| s == "pointee"),
            "borrowed pointee must outlive its reader's destructor"
        );
    }
}

#[derive(Facet)]
struct BorrowingRoot<'a> {
    nested: &'a BorrowingOwner<'a>,
    last: OwnedProxy,
}

impl Drop for BorrowingRoot<'_> {
    fn drop(&mut self) {
        assert_eq!(self.nested.guard.0, "guard");
    }
}

#[derive(Facet)]
struct BorrowingWire {
    nested: BorrowingWireOwner,
    last: String,
}

#[derive(Facet)]
struct BorrowingWireOwner {
    value: String,
    guard: String,
}

#[test]
fn retained_reference_owners_drop_before_their_pointees_on_all_exit_paths() {
    let _serial = SERIAL.lock().unwrap();
    use phon_engine::{Registry, typed};
    let derived = phon::derive::of::<BorrowingRoot<'_>>().unwrap();
    let program = typed::lower_typed(
        &derived.descriptor,
        &derived.descriptor_blocks,
        &Registry::new(derived.schemas),
    )
    .unwrap();
    let wire = Codec::<BorrowingWire>::new().unwrap();
    let make = |last: &str| {
        wire.encode(&BorrowingWire {
            nested: BorrowingWireOwner {
                value: "pointee".into(),
                guard: "guard".into(),
            },
            last: last.into(),
        })
        .unwrap()
    };
    clear_drops();
    let bytes = make("last");
    let mut slot = std::mem::MaybeUninit::<BorrowingRoot<'_>>::uninit();
    let retention =
        unsafe { typed::decode_with_retention(&program, &bytes, slot.as_mut_ptr().cast()) }
            .unwrap();
    assert!(!retention.is_empty());
    drop(unsafe { slot.assume_init() });
    assert_dropped(&["last"]);
    drop(retention);
    assert_dropped(&["pointee", "guard", "last"]);
    for (mut bytes, trailing) in [(make("reject"), false), (make("last"), true)] {
        if trailing {
            bytes.push(0);
        }
        clear_drops();
        let mut slot = std::mem::MaybeUninit::<BorrowingRoot<'_>>::uninit();
        assert!(
            unsafe { typed::decode_with_retention(&program, &bytes, slot.as_mut_ptr().cast()) }
                .is_err()
        );
        if trailing {
            assert_dropped(&["pointee", "guard", "last"]);
        } else {
            assert_dropped(&["pointee", "guard"]);
        }
    }
    clear_drops();
    // The public owned-value API must reject engine-retained references, cleaning
    // the completed root while the pointees still live, rather than returning them.
    assert!(
        Codec::<BorrowingRoot<'_>>::new()
            .unwrap()
            .decode(&bytes)
            .is_err()
    );
    assert_dropped(&["pointee", "guard", "last"]);
}

#[test]
fn compatibility_decoder_cleans_completed_fields_and_defaults() {
    let _serial = SERIAL.lock().unwrap();
    use phon_engine::{Registry, typed};
    #[derive(Facet)]
    struct Writer {
        first: String,
        last: String,
    }
    #[derive(Facet)]
    struct Reader {
        first: OwnedProxy,
        #[facet(default)]
        added: OwnedProxy,
        last: OwnedProxy,
    }

    let writer = phon::derive::of::<Writer>().unwrap();
    let reader = phon::derive::of::<Reader>().unwrap();
    let registry = Registry::new(writer.schemas.into_iter().chain(reader.schemas));
    let program = typed::lower_decode(
        writer.root,
        &reader.descriptor,
        &reader.descriptor_blocks,
        &registry,
    )
    .unwrap();
    let wire = Codec::<Writer>::new().unwrap();
    clear_drops();
    let bytes = wire
        .encode(&Writer {
            first: "first".into(),
            last: "reject".into(),
        })
        .unwrap();
    let mut slot = std::mem::MaybeUninit::<Reader>::uninit();
    assert!(unsafe { typed::decode_with(&program, &bytes, slot.as_mut_ptr().cast()) }.is_err());
    assert_dropped(&["first"]); // defaults are applied after wire fields
    clear_drops();
    let mut bytes = wire
        .encode(&Writer {
            first: "first".into(),
            last: "last".into(),
        })
        .unwrap();
    let mut slot = std::mem::MaybeUninit::<Reader>::uninit();
    unsafe { typed::decode_with(&program, &bytes, slot.as_mut_ptr().cast()) }.unwrap();
    let value = unsafe { slot.assume_init() };
    assert_eq!(value.first.0, "first");
    assert_eq!(value.added.0, "default");
    assert_eq!(value.last.0, "last");
    drop(value);
    assert_dropped(&["first", "default", "last"]);
    clear_drops();
    bytes.push(0);
    let mut slot = std::mem::MaybeUninit::<Reader>::uninit();
    assert!(unsafe { typed::decode_with(&program, &bytes, slot.as_mut_ptr().cast()) }.is_err());
    assert_dropped(&["first", "default", "last"]);
}

impl Default for OwnedProxy {
    fn default() -> Self {
        Self("default".into())
    }
}

#[test]
fn duplicate_set_and_map_inputs_drop_consumed_and_retained_values_once() {
    let _serial = SERIAL.lock().unwrap();
    clear_drops();
    // Compact collection bytes contain a count followed by elements/entries.
    let string_list = Codec::<Vec<String>>::new().unwrap();
    let string_set = Codec::<BTreeSet<String>>::new().unwrap();
    assert_eq!(
        string_list.encode(&vec!["a".into(), "b".into()]).unwrap(),
        string_set
            .encode(&BTreeSet::from(["a".into(), "b".into()]))
            .unwrap()
    );
    let duplicate = string_list.encode(&vec!["a".into(), "a".into()]).unwrap();
    assert!(
        Codec::<BTreeSet<OwnedProxy>>::new()
            .unwrap()
            .decode(&duplicate)
            .is_err()
    );
    assert_dropped(&["a", "a"]);
    clear_drops();
    let pairs = Codec::<Vec<(String, String)>>::new().unwrap();
    let string_map = Codec::<BTreeMap<String, String>>::new().unwrap();
    assert_eq!(
        pairs.encode(&vec![("a".into(), "value".into())]).unwrap(),
        string_map
            .encode(&BTreeMap::from([("a".into(), "value".into())]))
            .unwrap()
    );
    let duplicate = pairs
        .encode(&vec![
            ("a".into(), "first".into()),
            ("a".into(), "second".into()),
        ])
        .unwrap();
    assert!(
        Codec::<BTreeMap<OwnedProxy, OwnedProxy>>::new()
            .unwrap()
            .decode(&duplicate)
            .is_err()
    );
    assert_dropped(&["a", "a", "first", "second"]);
}

#[derive(Facet)]
#[facet(transparent)]
struct Flag(bool);
impl Drop for Flag {
    fn drop(&mut self) {
        SEEN.lock().unwrap().push(format!("flag:{}", self.0));
    }
}
#[derive(Facet)]
#[facet(transparent)]
struct Glyph(char);
impl Drop for Glyph {
    fn drop(&mut self) {
        SEEN.lock().unwrap().push(format!("glyph:{}", self.0));
    }
}
#[derive(Facet)]
struct ScalarOwner<T> {
    before: OwnedProxy,
    scalar: T,
}
impl<T> Drop for ScalarOwner<T> {
    fn drop(&mut self) {
        SEEN.lock().unwrap().push("aggregate".into());
    }
}

// r[verify descriptors.explicit-proxy]
#[test]
fn invalid_scalar_bytes_never_become_initialized_owners() {
    let _serial = SERIAL.lock().unwrap();
    fn check<T: for<'a> Facet<'a>, W: for<'a> Facet<'a>>(bad: W, good: W, extra: &[&str]) {
        let codec = Codec::<(ScalarOwner<T>, OwnedProxy)>::new().unwrap();
        let wire = Codec::<((String, W), String)>::new().unwrap();
        clear_drops();
        let bytes = wire
            .encode(&(("first".into(), bad), "reject".into()))
            .unwrap();
        assert!(codec.decode(&bytes).is_err());
        assert_dropped(&["first"]); // neither invalid leaf nor aggregate may Drop
        clear_drops();
        let bytes = wire
            .encode(&(("first".into(), good), "reject".into()))
            .unwrap();
        assert!(codec.decode(&bytes).is_err());
        let mut expected = vec!["aggregate", "first"];
        expected.extend_from_slice(extra);
        assert_dropped(&expected);
    }
    check::<bool, u8>(2, 1, &[]);
    check::<char, u32>(0xD800, 'A' as u32, &[]);
    check::<char, u32>(0x110000, 'A' as u32, &[]);
    check::<Flag, u8>(2, 1, &["flag:true"]);
    check::<Glyph, u32>(0xD800, 'A' as u32, &["glyph:A"]);
}

// r[verify descriptors.explicit-proxy]
#[test]
fn invalid_scalar_collection_elements_drop_only_valid_prefix() {
    let _serial = SERIAL.lock().unwrap();
    fn check<T: for<'a> Facet<'a>, W: for<'a> Facet<'a>>(wire: W, expected: &[&str]) {
        clear_drops();
        let bytes = Codec::<(String, W)>::new()
            .unwrap()
            .encode(&("first".into(), wire))
            .unwrap();
        assert!(
            Codec::<(OwnedProxy, T)>::new()
                .unwrap()
                .decode(&bytes)
                .is_err()
        );
        assert_dropped(expected);
    }
    check::<Vec<Flag>, _>(vec![1_u8, 2], &["first", "flag:true"]);
    check::<[Flag; 2], _>([1_u8, 2], &["first", "flag:true"]);
    check::<Vec<Glyph>, _>(vec!['A' as u32, 0xD800], &["first", "glyph:A"]);
    check::<[Glyph; 2], _>(['A' as u32, 0x110000], &["first", "glyph:A"]);
}
