package org.facet.phon;

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
}
