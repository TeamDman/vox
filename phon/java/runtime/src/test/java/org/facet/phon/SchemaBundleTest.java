package org.facet.phon;

import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Arrays;
import java.util.List;
import java.util.Map;

/** Independent Rust goldens and adversarial admission tests for PHONSCM1. */
final class SchemaBundleTest {
    private static int checks;
    private static final SchemaBundleLimits LIMITS = SchemaBundleLimits.DEFAULT;
    private interface Action { void run() throws Exception; }
    private static void check(boolean value, String message) {
        checks++;
        if (!value) throw new AssertionError(message);
    }
    private static void rejects(Action action, String message) throws Exception {
        try { action.run(); } catch (PhonException expected) { checks++; return; }
        throw new AssertionError("accepted " + message);
    }
    private static Schema node(Schema.Kind kind, String... params) throws Exception {
        Schema provisional = new Schema(SchemaId.fromLong(42), List.of(params), kind);
        return new Schema(SchemaIdentity.recompute(List.of(provisional), PhonLimits.DEFAULT)
                .get(provisional.id()), provisional.typeParameters(), kind);
    }
    static void run(Path repository) throws Exception {
        for (String name : List.of("region-ref", "semantic-u32", "generic-tuple", "channel-tx",
                "channel-rx", "external-none", "external-metadata")) {
            byte[] bytes = Files.readAllBytes(repository.resolve("phon/conformance/java-cutover/" + name + ".phon"));
            List<Schema> schemas = SchemaBundle.decode(bytes, LIMITS).schemas();
            check(Arrays.equals(bytes, SchemaBundle.encode(schemas, LIMITS)), name + " canonical Rust bytes and ids");
            for (int length = 0; length < bytes.length; length++) {
                byte[] truncated = Arrays.copyOf(bytes, length);
                rejects(() -> SchemaBundle.decode(truncated, LIMITS), name + " truncation " + length);
            }
            rejects(() -> SchemaBundle.decode(Arrays.copyOf(bytes, bytes.length + 1), LIMITS), "trailing byte");
            for (int offset : new int[]{0, 8, 12, 17}) {
                byte[] bad = bytes.clone(); bad[offset] ^= 0x40;
                rejects(() -> SchemaBundle.decode(bad, LIMITS), "magic/version/tag/field");
            }
            rejects(() -> SchemaBundle.decode(bytes, new SchemaBundleLimits(bytes.length-1, 10000, 100, 100, 128, 10000)), "total byte limit");
            if (!name.startsWith("channel-")) rejects(() -> SchemaBundle.decode(bytes, new SchemaBundleLimits(10000, 1, 100, 100, 128, 10000)), "owned byte limit");
            if (!name.equals("external-none")) rejects(() -> SchemaBundle.decode(bytes, new SchemaBundleLimits(10000, 10000, 100, 100, 1, 10000)), "depth limit");
            rejects(() -> SchemaBundle.decode(bytes, new SchemaBundleLimits(10000, 10000, 100, 100, 128, 1)), "work limit");
        }
        byte[] point = Files.readAllBytes(repository.resolve("phon/rust/phon-schema/testdata/schema-bundle-point-v1.phon"));
        check(Arrays.equals(point, SchemaBundle.encode(SchemaBundle.decode(point, LIMITS).schemas(), LIMITS)), "upstream point bundle");
        byte[] semantic = Files.readAllBytes(repository.resolve("phon/rust/phon-schema/testdata/schema-semantic-region-ref-v1.phon"));
        Schema region = SchemaWire.decode(semantic, PhonLimits.DEFAULT);
        check(Arrays.equals(semantic, SchemaWire.encode(region)), "upstream bare semantic bytes");
        check(region.id().equals(SchemaIdentity.recompute(List.of(region), PhonLimits.DEFAULT).get(region.id())), "upstream semantic id");

        Schema.Ref u32 = Schema.Ref.concrete(Schema.primitive(Schema.Primitive.U32).id());
        Schema a = node(new Schema.SemanticKind(new QualifiedName("com.example.a"), List.of(), u32));
        Schema b = node(new Schema.SemanticKind(new QualifiedName("com.example.b"), List.of(), u32));
        check(!a.id().equals(b.id()), "semantic name changes identity");
        rejects(() -> CompatibilityPlan.plan(SchemaClosure.of(a), SchemaClosure.of(b), PhonLimits.DEFAULT), "semantic equivalence from representation");
        rejects(() -> CompatibilityPlan.plan(SchemaClosure.of(a), SchemaClosure.of(a), PhonLimits.DEFAULT), "semantic without registered handler");
        PhonEncoder encoder = new PhonEncoder(PhonLimits.DEFAULT); encoder.writeU32(123);
        PhonCodec.decodeValue(SchemaClosure.of(a), encoder.finish(), PhonLimits.DEFAULT);
        checks++;
        Schema generic = node(new Schema.TupleKind(List.of(Schema.Ref.variable("T"))), "T");
        Schema use = node(new Schema.ListKind(Schema.Ref.concrete(generic.id(), List.of(u32))));
        SchemaBundle.encode(List.of(generic, use), LIMITS);
        checks++;
        rejects(() -> SchemaBundle.encode(List.of(use), LIMITS), "missing reference");
        Schema wrongArity = node(new Schema.ListKind(Schema.Ref.concrete(generic.id())));
        rejects(() -> SchemaBundle.encode(List.of(generic, wrongArity), LIMITS), "arity");
        Schema undeclared = node(new Schema.TupleKind(List.of(Schema.Ref.variable("T"))));
        rejects(() -> SchemaBundle.encode(List.of(undeclared), LIMITS), "undeclared variable");
        rejects(() -> SchemaBundle.encode(List.of(a, a), LIMITS), "duplicate id");
        rejects(() -> SchemaBundle.encode(List.of(new Schema(b.id(), List.of(), a.kind())), LIMITS), "hash mismatch");
        rejects(() -> SchemaBundle.encode(List.of(a,b), new SchemaBundleLimits(10000,10000,100,1,128,10000)), "schema count");
        rejects(() -> SchemaBundle.encode(List.of(a,b), new SchemaBundleLimits(10000,10000,1,100,128,10000)), "string count");
        for (String name : List.of("", "single", "Com.example", "com..a", "com.a-", "com.a--b", "com.é", "com.1a", "a." + "b".repeat(254))) {
            try { new QualifiedName(name); throw new AssertionError("accepted qualified name " + name); }
            catch (IllegalArgumentException expected) { checks++; }
        }
        // Exercise generic substitution in the compact codec, not only admission.
        SchemaClosure genericUse = new SchemaClosure(use, List.of(generic));
        PhonEncoder values = new PhonEncoder(PhonLimits.DEFAULT);
        values.writeCount(1); values.writeU32(321);
        byte[] compact = values.finish();
        check(Arrays.equals(compact, PhonCodec.encodeValue(genericUse,
                PhonCodec.decodeValue(genericUse, compact, PhonLimits.DEFAULT), PhonLimits.DEFAULT)),
                "generic substitution compact roundtrip");
        // Deep in-memory references must fail before reaching recursive encoders.
        Schema.Ref deep = u32;
        for (int i=0; i<10000; i++) deep = Schema.Ref.concrete(generic.id(), List.of(deep));
        Schema deepSchema = new Schema(SchemaId.fromLong(77), List.of(), new Schema.ListKind(deep));
        rejects(() -> SchemaWire.encode(deepSchema), "bare encoder depth");
        rejects(() -> SchemaBundle.encode(List.of(generic,deepSchema), LIMITS), "bundle encoder depth");
        rejects(() -> new SchemaClosure(deepSchema,List.of(generic)), "closure depth before encode");
        rejects(() -> new SchemaClosure(use,List.of()), "closure missing reference");
        malformedTables(a, b);
        System.out.println("Schema bundle cutover passed (" + checks + " assertions)");
    }
    private static byte[] raw(List<String> strings, List<Schema> schemas, Map<String,Integer> indices) throws Exception {
        SchemaWire.Out out = new SchemaWire.Out(100000, indices);
        out.raw("PHONSCM1".getBytes(java.nio.charset.StandardCharsets.US_ASCII)); out.u32(1);
        out.struct("SchemaBundleV1",2); out.name("strings"); out.list(strings.size());
        for(String string: strings) out.valueString(string);
        out.name("schemas"); out.list(schemas.size());
        for(Schema schema: schemas) SchemaWire.schema(out,schema);
        return out.bytes();
    }
    private static void malformedTables(Schema a, Schema b) throws Exception {
        String name = ((Schema.SemanticKind)a.kind()).name().value();
        for(List<String> strings: List.of(List.of(name,name),List.of("z.z",name),List.of(name,"z.z"))) {
            byte[] bytes = raw(strings,List.of(a),Map.of(name,strings.indexOf(name)));
            rejects(() -> SchemaBundle.decode(bytes,LIMITS), "duplicate/unordered/unused strings");
        }
        byte[] utf8 = raw(List.of(name),List.of(a),Map.of(name,0));
        byte[] needle = name.getBytes(java.nio.charset.StandardCharsets.UTF_8);
        int textAt = -1;
        for (int i=0; i<=utf8.length-needle.length; i++) {
            if (Arrays.equals(Arrays.copyOfRange(utf8,i,i+needle.length),needle)) { textAt=i; break; }
        }
        check(textAt>=0, "fixture string location");
        utf8[textAt]=(byte)0xff;
        rejects(() -> SchemaBundle.decode(utf8,LIMITS), "invalid UTF-8");
        byte[] invalidName = raw(List.of("Com.example.a"),List.of(a),Map.of(name,0));
        rejects(() -> SchemaBundle.decode(invalidName,LIMITS), "wire qualified name");
        Schema missing = new Schema(SchemaId.fromLong(11),List.of(),new Schema.ListKind(Schema.Ref.concrete(SchemaId.fromLong(12))));
        byte[] missingWire=raw(List.of(),List.of(missing),Map.of());
        rejects(() -> SchemaBundle.decode(missingWire,LIMITS), "wire missing reference");
        byte[] mismatch=raw(List.of(name),List.of(new Schema(b.id(),List.of(),a.kind())),Map.of(name,0));
        rejects(() -> SchemaBundle.decode(mismatch,LIMITS), "wire hash mismatch");
        byte[] badIndex = raw(List.of(name),List.of(a),Map.of(name,99));
        rejects(() -> SchemaBundle.decode(badIndex,LIMITS), "symbol out of range");
        byte[] duplicate = raw(List.of(name),List.of(a,a),Map.of(name,0));
        rejects(() -> SchemaBundle.decode(duplicate,LIMITS), "duplicate schemas");
        String other=((Schema.SemanticKind)b.kind()).name().value();
        List<Schema> reverse=a.id().compareTo(b.id())>0?List.of(a,b):List.of(b,a);
        byte[] order=raw(List.of(name,other),reverse,Map.of(name,0,other,1));
        rejects(() -> SchemaBundle.decode(order,LIMITS), "schema order");
    }
}
