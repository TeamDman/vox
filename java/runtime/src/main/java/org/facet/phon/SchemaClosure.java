package org.facet.phon;

import java.io.ByteArrayOutputStream;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Objects;

/** Root schema and its validated canonical reachable schema table. */
public final class SchemaClosure {
    private final Schema root;
    private final Map<SchemaId, Schema> schemas;
    private final byte[] canonicalBytes;

    public SchemaClosure(Schema root, List<Schema> reachable) throws PhonException {
        this(root, reachable, PhonLimits.DEFAULT);
    }

    public SchemaClosure(Schema root, List<Schema> reachable, PhonLimits limits) throws PhonException {
        this.root = Objects.requireNonNull(root);
        if (reachable.size() + 1 > limits.referencedSchemas())
            throw new PhonException(PhonException.Kind.LIMIT, "schema count exceeds referencedSchemas");
        LinkedHashMap<SchemaId, Schema> table = new LinkedHashMap<>();
        table.put(root.id(), root);
        for (Schema schema : reachable) {
            if (table.containsKey(schema.id()))
                throw new PhonException(PhonException.Kind.SCHEMA, "duplicate schema id " + schema.id());
            table.put(schema.id(), schema);
        }
        this.schemas = java.util.Collections.unmodifiableMap(table);
        this.canonicalBytes = SchemaWire.encode(root);
        if (canonicalBytes.length > limits.schemaBytes())
            throw new PhonException(PhonException.Kind.LIMIT, "schema bytes exceed schemaBytes");
        List<Schema> composites = new ArrayList<>(table.values());
        Map<SchemaId, SchemaId> actual = SchemaIdentity.recompute(composites, limits);
        for (Schema schema : composites) {
            SchemaId computed = actual.get(schema.id());
            if (!schema.id().equals(computed))
                throw new PhonException(PhonException.Kind.SCHEMA,
                        "schema id mismatch: stated " + schema.id() + ", computed " + computed);
        }
    }

    public static SchemaClosure of(Schema root, Schema... reachable) throws PhonException {
        return new SchemaClosure(root, List.of(reachable));
    }
    public static SchemaClosure uncheckedOf(Schema root, Schema... reachable) {
        try {
            return of(root, reachable);
        } catch (PhonException failure) {
            throw new ExceptionInInitializerError(failure);
        }
    }
    public static SchemaClosure fromCanonicalBytes(
            SchemaId rootId, byte[][] canonicalSchemas, PhonLimits limits)
            throws PhonException {
        List<Schema> decoded = new ArrayList<>();
        for (byte[] canonicalSchema : canonicalSchemas) {
            decoded.add(SchemaWire.decode(canonicalSchema, limits));
        }
        Schema root = null;
        for (Schema schema : decoded) {
            if (schema.id().equals(rootId)) {
                root = schema;
                break;
            }
        }
        if (root == null) {
            throw new PhonException(PhonException.Kind.SCHEMA,
                    "root schema " + rootId + " is absent from canonical schema table");
        }
        decoded.remove(root);
        return new SchemaClosure(root, decoded, limits);
    }
    /** Parse the Rust `vox_phon::schema_bytes` closure format. */
    public static SchemaClosure fromBundleBytes(byte[] bytes, PhonLimits limits)
            throws PhonException {
        Objects.requireNonNull(bytes, "bytes");
        if (bytes.length > limits.schemaBytes()) {
            throw new PhonException(PhonException.Kind.LIMIT,
                    "schema bundle exceeds schemaBytes");
        }
        BundleReader reader = new BundleReader(bytes);
        SchemaId rootId = SchemaId.fromLong(reader.u64("schema bundle root"));
        long count = reader.u32("schema bundle count");
        if (count > limits.referencedSchemas()) {
            throw new PhonException(PhonException.Kind.LIMIT,
                    "schema bundle count exceeds referencedSchemas");
        }
        byte[][] schemas = new byte[(int) count][];
        for (int index = 0; index < schemas.length; index++) {
            long length = reader.u32("schema bundle entry length");
            if (length > limits.schemaBytes()) {
                throw new PhonException(PhonException.Kind.LIMIT,
                        "schema bundle entry exceeds schemaBytes");
            }
            schemas[index] = reader.bytes((int) length, "schema bundle entry");
        }
        // Auxiliary roots are not used by the connection envelope or handshake.
        // Reject them rather than silently accepting a shape the Java slice cannot use.
        if (reader.remaining() != 0) {
            long auxiliaryCount = reader.u32("schema bundle auxiliary root count");
            if (auxiliaryCount != 0) {
                throw new PhonException(PhonException.Kind.SCHEMA,
                        "auxiliary schema roots are outside the Java wire slice");
            }
        }
        reader.finished();
        return fromCanonicalBytes(rootId, schemas, limits);
    }

    /** Encode the Rust `vox_phon::schema_bytes` closure format. */
    public byte[] bundleBytes() throws PhonException {
        ByteArrayOutputStream output = new ByteArrayOutputStream();
        little(output, id().asLong(), 8);
        little(output, schemas.size(), 4);
        for (Schema schema : schemas.values()) {
            byte[] encoded = SchemaWire.encode(schema);
            little(output, encoded.length, 4);
            output.write(encoded, 0, encoded.length);
        }
        return output.toByteArray();
    }
    public Schema root() { return root; }
    public SchemaId id() { return root.id(); }
    public byte[] canonicalBytes() { return canonicalBytes.clone(); }
    public List<Schema> schemas() { return List.copyOf(schemas.values()); }
    Schema schema(SchemaId id) { return schemas.get(id); }
    Schema resolve(Schema.Ref ref) throws PhonException {
        if (ref.isVariable()) throw new PhonException(PhonException.Kind.SCHEMA, "unbound type variable " + ref.variable());
        Schema schema = schemas.get(ref.id());
        if (schema != null) return schema;
        for (Schema.Primitive primitive : Schema.Primitive.values())
            if (SchemaIdentity.primitiveId(primitive).equals(ref.id()))
                return new Schema(ref.id(), List.of(), new Schema.PrimitiveKind(primitive));
        throw new PhonException(PhonException.Kind.SCHEMA, "unknown schema " + ref.id());
    }

    private static void little(ByteArrayOutputStream output, long value, int width) {
        for (int index = 0; index < width; index++) {
            output.write((int) (value >>> (8 * index)) & 0xff);
        }
    }

    private static final class BundleReader {
        private final byte[] bytes;
        private int position;

        BundleReader(byte[] bytes) { this.bytes = bytes; }
        int remaining() { return bytes.length - position; }
        long u32(String part) throws PhonException { return little(4, part) & 0xffff_ffffL; }
        long u64(String part) throws PhonException { return little(8, part); }
        private long little(int width, String part) throws PhonException {
            if (remaining() < width) {
                throw new PhonException(PhonException.Kind.TRUNCATED,
                        "truncated " + part, position, null);
            }
            long value = 0;
            for (int index = 0; index < width; index++) {
                value |= (long) (bytes[position++] & 0xff) << (8 * index);
            }
            return value;
        }
        byte[] bytes(int length, String part) throws PhonException {
            if (length < 0 || remaining() < length) {
                throw new PhonException(PhonException.Kind.TRUNCATED,
                        "truncated " + part, position, null);
            }
            byte[] result = java.util.Arrays.copyOfRange(bytes, position, position + length);
            position += length;
            return result;
        }
        void finished() throws PhonException {
            if (remaining() != 0) {
                throw new PhonException(PhonException.Kind.MALFORMED,
                        "schema bundle has trailing bytes", position, null);
            }
        }
    }
}
