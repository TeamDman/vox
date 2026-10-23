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
    private final Map<String, SchemaId> auxiliaryRoots;
    private final byte[] canonicalBytes;
    private final PhonLimits limits;

    public SchemaClosure(Schema root, List<Schema> reachable) throws PhonException {
        this(root, reachable, PhonLimits.DEFAULT);
    }

    public SchemaClosure(Schema root, List<Schema> reachable, PhonLimits limits) throws PhonException {
        this(root, reachable, Map.of(), limits);
    }

    public SchemaClosure(
            Schema root,
            List<Schema> reachable,
            Map<String, SchemaId> auxiliaryRoots,
            PhonLimits limits) throws PhonException {
        this.root = Objects.requireNonNull(root);
        this.limits = Objects.requireNonNull(limits, "limits");
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
        LinkedHashMap<String, SchemaId> roots = new LinkedHashMap<>();
        for (Map.Entry<String, SchemaId> entry : new java.util.TreeMap<>(
                Objects.requireNonNull(auxiliaryRoots, "auxiliaryRoots")).entrySet()) {
            String role = Objects.requireNonNull(entry.getKey(), "auxiliary root role");
            SchemaId id = Objects.requireNonNull(entry.getValue(), "auxiliary root id");
            if (role.isEmpty()) {
                throw new PhonException(
                        PhonException.Kind.SCHEMA, "auxiliary schema root role must not be empty");
            }
            if (!table.containsKey(id) && !isPrimitive(id)) {
                throw new PhonException(
                        PhonException.Kind.SCHEMA,
                        "auxiliary schema root " + id + " is absent from schema table");
            }
            if (roots.put(role, id) != null) {
                throw new PhonException(
                        PhonException.Kind.SCHEMA, "duplicate auxiliary schema root role " + role);
            }
        }
        this.auxiliaryRoots = java.util.Collections.unmodifiableMap(roots);
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
    public static SchemaClosure withAuxiliaryRoots(
            Schema root,
            List<Schema> reachable,
            Map<String, SchemaId> auxiliaryRoots,
            PhonLimits limits) throws PhonException {
        return new SchemaClosure(root, reachable, auxiliaryRoots, limits);
    }
    public static SchemaClosure uncheckedWithAuxiliaryRoots(
            Schema root, List<Schema> reachable, Map<String, SchemaId> auxiliaryRoots) {
        try {
            return withAuxiliaryRoots(root, reachable, auxiliaryRoots, PhonLimits.DEFAULT);
        } catch (PhonException failure) {
            throw new ExceptionInInitializerError(failure);
        }
    }
    public static SchemaClosure uncheckedWithAuxiliaryClosures(
            Schema root,
            List<Schema> reachable,
            Map<String, SchemaClosure> auxiliaryClosures) {
        try {
            LinkedHashMap<SchemaId, Schema> all = new LinkedHashMap<>();
            for (Schema schema : reachable) mergeSchema(all, schema);
            return withMergedClosures(root, all, auxiliaryClosures);
        } catch (PhonException failure) {
            throw new ExceptionInInitializerError(failure);
        }
    }
    /** Add role-keyed channel roots to an already complete primary closure. */
    public static SchemaClosure uncheckedWithAuxiliaryClosures(
            SchemaClosure primary,
            Map<String, SchemaClosure> auxiliaryClosures) {
        try {
            LinkedHashMap<SchemaId, Schema> all = new LinkedHashMap<>();
            for (Schema schema : Objects.requireNonNull(primary).schemas()) {
                mergeSchema(all, schema);
            }
            return withMergedClosures(primary.root(), all, auxiliaryClosures);
        } catch (PhonException failure) {
            throw new ExceptionInInitializerError(failure);
        }
    }
    private static SchemaClosure withMergedClosures(
            Schema root,
            LinkedHashMap<SchemaId, Schema> all,
            Map<String, SchemaClosure> auxiliaryClosures) throws PhonException {
            LinkedHashMap<String, SchemaId> roots = new LinkedHashMap<>();
            for (Map.Entry<String, SchemaClosure> entry : auxiliaryClosures.entrySet()) {
                SchemaClosure closure = Objects.requireNonNull(entry.getValue());
                roots.put(entry.getKey(), closure.id());
                for (Schema schema : closure.schemas()) mergeSchema(all, schema);
            }
            all.remove(root.id());
            ArrayList<Schema> ordered = new ArrayList<>(all.values());
            ordered.sort(java.util.Comparator.comparing(schema -> schema.id().toString()));
            return new SchemaClosure(root, ordered, roots, PhonLimits.DEFAULT);
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
        return new SchemaClosure(root, decoded, Map.of(), limits);
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
        LinkedHashMap<String, SchemaId> auxiliaryRoots = new LinkedHashMap<>();
        if (reader.remaining() != 0) {
            long auxiliaryCount = reader.u32("schema bundle auxiliary root count");
            if (auxiliaryCount > limits.referencedSchemas()) {
                throw new PhonException(
                        PhonException.Kind.LIMIT,
                        "auxiliary schema root count exceeds referencedSchemas");
            }
            for (long index = 0; index < auxiliaryCount; index++) {
                long roleLength = reader.u32("auxiliary schema root role length");
                if (roleLength > limits.schemaBytes()) {
                    throw new PhonException(
                            PhonException.Kind.LIMIT,
                            "auxiliary schema root role exceeds schemaBytes");
                }
                String role;
                try {
                    role = java.nio.charset.StandardCharsets.UTF_8.newDecoder()
                            .onMalformedInput(java.nio.charset.CodingErrorAction.REPORT)
                            .onUnmappableCharacter(java.nio.charset.CodingErrorAction.REPORT)
                            .decode(java.nio.ByteBuffer.wrap(reader.bytes(
                                    (int) roleLength, "auxiliary schema root role")))
                            .toString();
                } catch (java.nio.charset.CharacterCodingException failure) {
                    throw new PhonException(
                            PhonException.Kind.MALFORMED,
                            "invalid auxiliary schema root role", failure);
                }
                SchemaId auxiliaryRoot =
                        SchemaId.fromLong(reader.u64("auxiliary schema root"));
                if (auxiliaryRoots.put(role, auxiliaryRoot) != null) {
                    throw new PhonException(
                            PhonException.Kind.SCHEMA,
                            "duplicate auxiliary schema root role " + role);
                }
            }
        }
        reader.finished();
        List<Schema> decoded = new ArrayList<>();
        for (byte[] canonicalSchema : schemas) {
            decoded.add(SchemaWire.decode(canonicalSchema, limits));
        }
        Schema root = decoded.stream()
                .filter(schema -> schema.id().equals(rootId))
                .findFirst()
                .orElseThrow(() -> new PhonException(
                        PhonException.Kind.SCHEMA,
                        "root schema " + rootId + " is absent from canonical schema table"));
        decoded.remove(root);
        return new SchemaClosure(root, decoded, auxiliaryRoots, limits);
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
        if (!auxiliaryRoots.isEmpty()) {
            little(output, auxiliaryRoots.size(), 4);
            for (Map.Entry<String, SchemaId> entry : auxiliaryRoots.entrySet()) {
                byte[] role = entry.getKey().getBytes(java.nio.charset.StandardCharsets.UTF_8);
                little(output, role.length, 4);
                output.write(role, 0, role.length);
                little(output, entry.getValue().asLong(), 8);
            }
        }
        return output.toByteArray();
    }
    public Schema root() { return root; }
    public SchemaId id() { return root.id(); }
    public byte[] canonicalBytes() { return canonicalBytes.clone(); }
    public List<Schema> schemas() { return List.copyOf(schemas.values()); }
    public Map<String, SchemaId> auxiliaryRoots() { return auxiliaryRoots; }
    public SchemaClosure auxiliary(String role) throws PhonException {
        SchemaId id = auxiliaryRoots.get(role);
        if (id == null) return null;
        Schema auxiliaryRoot = schemas.get(id);
        if (auxiliaryRoot == null) {
            for (Schema.Primitive primitive : Schema.Primitive.values()) {
                if (SchemaIdentity.primitiveId(primitive).equals(id)) {
                    auxiliaryRoot = new Schema(id, List.of(), new Schema.PrimitiveKind(primitive));
                    break;
                }
            }
        }
        if (auxiliaryRoot == null) {
            throw new PhonException(
                    PhonException.Kind.SCHEMA, "unknown auxiliary schema root " + id);
        }
        ArrayList<Schema> reachable = new ArrayList<>(schemas.values());
        reachable.remove(auxiliaryRoot);
        return new SchemaClosure(auxiliaryRoot, reachable, Map.of(), limits);
    }
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

    private static boolean isPrimitive(SchemaId id) {
        for (Schema.Primitive primitive : Schema.Primitive.values()) {
            if (SchemaIdentity.primitiveId(primitive).equals(id)) return true;
        }
        return false;
    }

    private static void mergeSchema(Map<SchemaId, Schema> schemas, Schema candidate)
            throws PhonException {
        Schema existing = schemas.putIfAbsent(candidate.id(), candidate);
        if (existing != null
                && !java.util.Arrays.equals(
                        SchemaWire.encode(existing), SchemaWire.encode(candidate))) {
            throw new PhonException(
                    PhonException.Kind.SCHEMA,
                    "conflicting canonical schemas share id " + candidate.id());
        }
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
