package org.facet.phon;

import java.util.List;
import java.util.Objects;

/** Immutable canonical Phon schema node. */
public final class Schema {
    public enum Primitive {
        BOOL("bool"), U8("u8"), U16("u16"), U32("u32"), U64("u64"), U128("u128"),
        I8("i8"), I16("i16"), I32("i32"), I64("i64"), I128("i128"),
        F32("f32"), F64("f64"), CHAR("char"), STRING("string"), BYTES("bytes"),
        DATETIME("datetime"), UUID("uuid"), QNAME("qname"), UNIT("unit"), NEVER("never");
        private final String tag;
        Primitive(String tag) { this.tag = tag; }
        public String tag() { return tag; }
        public static Primitive fromTag(String tag) throws PhonException {
            for (Primitive value : values()) if (value.tag.equals(tag)) return value;
            throw new PhonException(PhonException.Kind.SCHEMA, "unknown primitive " + tag);
        }
    }

    public interface Kind {}
    public static final class PrimitiveKind implements Kind {
        private final Primitive primitive;
        public PrimitiveKind(Primitive primitive) { this.primitive = Objects.requireNonNull(primitive); }
        public Primitive primitive() { return primitive; }
    }
    public static final class RecordKind implements Kind {
        private final String name; private final List<Field> fields;
        public RecordKind(String name, List<Field> fields) {
            this.name = Objects.requireNonNull(name); this.fields = List.copyOf(fields);
        }
        public String name() { return name; } public List<Field> fields() { return fields; }
    }
    public static final class EnumKind implements Kind {
        private final String name; private final List<Variant> variants;
        public EnumKind(String name, List<Variant> variants) {
            this.name = Objects.requireNonNull(name); this.variants = List.copyOf(variants);
        }
        public String name() { return name; } public List<Variant> variants() { return variants; }
    }
    public static final class TupleKind implements Kind {
        private final List<Ref> elements;
        public TupleKind(List<Ref> elements) { this.elements = List.copyOf(elements); }
        public List<Ref> elements() { return elements; }
    }
    public static final class ListKind implements Kind {
        private final Ref element;
        public ListKind(Ref element) { this.element = Objects.requireNonNull(element); }
        public Ref element() { return element; }
    }
    public static final class SetKind implements Kind {
        private final Ref element;
        public SetKind(Ref element) { this.element = Objects.requireNonNull(element); }
        public Ref element() { return element; }
    }
    public static final class MapKind implements Kind {
        private final Ref key, value;
        public MapKind(Ref key, Ref value) {
            this.key = Objects.requireNonNull(key); this.value = Objects.requireNonNull(value);
        }
        public Ref key() { return key; } public Ref value() { return value; }
    }
    public static final class OptionKind implements Kind {
        private final Ref element;
        public OptionKind(Ref element) { this.element = Objects.requireNonNull(element); }
        public Ref element() { return element; }
    }
    public static final class ArrayKind implements Kind {
        private final Ref element; private final List<Long> dimensions;
        public ArrayKind(Ref element, List<Long> dimensions) {
            this.element = Objects.requireNonNull(element); this.dimensions = List.copyOf(dimensions);
        }
        public Ref element() { return element; } public List<Long> dimensions() { return dimensions; }
    }
    public static final class TensorKind implements Kind {
        private final Ref element; private final Integer rank;
        public TensorKind(Ref element, Integer rank) { this.element = Objects.requireNonNull(element); this.rank = rank; }
        public Ref element() { return element; } public Integer rank() { return rank; }
    }
    public static final class DynamicKind implements Kind {
        public static final DynamicKind INSTANCE = new DynamicKind(); private DynamicKind() {}
    }
    public static final class UnsupportedKind implements Kind {
        private final String name;
        public UnsupportedKind(String name) { this.name = Objects.requireNonNull(name); }
        public String name() { return name; }
    }

    public static final class Ref {
        private final SchemaId id; private final String variable; private final List<Ref> arguments;
        private Ref(SchemaId id, String variable, List<Ref> arguments) {
            this.id = id; this.variable = variable; this.arguments = List.copyOf(arguments);
        }
        public static Ref concrete(SchemaId id) { return concrete(id, List.of()); }
        public static Ref concrete(SchemaId id, List<Ref> arguments) {
            return new Ref(Objects.requireNonNull(id), null, arguments);
        }
        public static Ref variable(String name) { return new Ref(null, Objects.requireNonNull(name), List.of()); }
        public boolean isVariable() { return variable != null; }
        public SchemaId id() { return id; } public String variable() { return variable; }
        public List<Ref> arguments() { return arguments; }
    }
    public static final class Field {
        private final String name; private final Ref schema; private final boolean required;
        public Field(String name, Ref schema, boolean required) {
            this.name = Objects.requireNonNull(name); this.schema = Objects.requireNonNull(schema); this.required = required;
        }
        public String name() { return name; } public Ref schema() { return schema; } public boolean required() { return required; }
    }
    public abstract static class Payload {
        private Payload() {}
        public static Payload unit() { return UnitPayload.INSTANCE; }
        public static Payload newtype(Ref ref) { return new NewtypePayload(ref); }
        public static Payload tuple(List<Ref> refs) { return new TuplePayload(refs); }
        public static Payload record(List<Field> fields) { return new RecordPayload(fields); }
    }
    public static final class UnitPayload extends Payload {
        private static final UnitPayload INSTANCE = new UnitPayload(); private UnitPayload() {}
    }
    public static final class NewtypePayload extends Payload {
        private final Ref ref; private NewtypePayload(Ref ref) { this.ref = Objects.requireNonNull(ref); }
        public Ref ref() { return ref; }
    }
    public static final class TuplePayload extends Payload {
        private final List<Ref> refs; private TuplePayload(List<Ref> refs) { this.refs = List.copyOf(refs); }
        public List<Ref> refs() { return refs; }
    }
    public static final class RecordPayload extends Payload {
        private final List<Field> fields; private RecordPayload(List<Field> fields) { this.fields = List.copyOf(fields); }
        public List<Field> fields() { return fields; }
    }
    public static final class Variant {
        private final String name; private final int index; private final Payload payload;
        public Variant(String name, int index, Payload payload) {
            this.name = Objects.requireNonNull(name); this.index = index; this.payload = Objects.requireNonNull(payload);
        }
        public String name() { return name; } public int index() { return index; } public Payload payload() { return payload; }
    }

    private final SchemaId id;
    private final List<String> typeParameters;
    private final Kind kind;

    public static Schema primitive(Primitive primitive) {
        return new Schema(SchemaIdentity.primitiveId(primitive), List.of(), new PrimitiveKind(primitive));
    }

    public Schema(SchemaId id, List<String> typeParameters, Kind kind) {
        this.id = Objects.requireNonNull(id);
        this.typeParameters = List.copyOf(typeParameters);
        this.kind = Objects.requireNonNull(kind);
    }
    public SchemaId id() { return id; }
    public List<String> typeParameters() { return typeParameters; }
    public Kind kind() { return kind; }
}
