package org.facet.phon;

import java.math.BigInteger;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.Map;

/** Dependency-free Java 17 test entry point against the checked-in Rust corpus. */
public final class PhonConformanceTest {
    private static int assertions;
    private PhonConformanceTest() {}

    public static void main(String[] args) throws Exception {
        Path repository=Path.of(args.length==0?".":args[0]).toAbsolutePath().normalize();
        primitiveIdsMatchRust();
        acceptedSchemaCorpusRoundTrips(repository);
        schemaBundlesRoundTrip(repository);
        valueCorpusRoundTrips(repository);
        typedCodecRoundTrips();
        compatibilityPlansEagerly();
        malformedAndLimitsFail(repository);
        System.out.println("Phon Java conformance passed ("+assertions+" assertions)");
    }

    private static void schemaBundlesRoundTrip(Path repository)throws Exception{
        byte[]point=Files.readAllBytes(repository.resolve("phon/conformance/cases/point/Point.phon"));
        SchemaClosure original=SchemaClosure.of(SchemaWire.decode(point,PhonLimits.DEFAULT));
        SchemaClosure decoded=SchemaClosure.fromBundleBytes(original.bundleBytes(),PhonLimits.DEFAULT);
        equal(original.id(),decoded.id(),"schema bundle root");
        same(original.canonicalBytes(),decoded.canonicalBytes(),"schema bundle canonical root");
        same(original.bundleBytes(),decoded.bundleBytes(),"schema bundle bytes");
    }

    private static void valueCorpusRoundTrips(Path repository)throws Exception{
        Path root=repository.resolve("phon/conformance/values");
        try(var files=Files.list(root)){
            for(Path file:files.filter(p->p.toString().endsWith(".phon")).sorted().toList()){
                byte[]wire=Files.readAllBytes(file);
                Value value=ValueWire.decode(wire,PhonLimits.DEFAULT);
                same(wire,ValueWire.encode(value,PhonLimits.DEFAULT),"value roundtrip "+file.getFileName());
            }
        }
    }

    private static void primitiveIdsMatchRust() {
        Map<Schema.Primitive,String> expected=Map.ofEntries(
                Map.entry(Schema.Primitive.BOOL,"178367a87f66fb46"),Map.entry(Schema.Primitive.U8,"2c8d54f2314d0f20"),
                Map.entry(Schema.Primitive.U16,"1be6c8d0625ea876"),Map.entry(Schema.Primitive.U32,"281c5be4f2ee63b4"),
                Map.entry(Schema.Primitive.U64,"d9356298b81639ac"),Map.entry(Schema.Primitive.U128,"767c691472231d95"),
                Map.entry(Schema.Primitive.I8,"3bd6a76856978968"),Map.entry(Schema.Primitive.I16,"269c2efb67f8a4c7"),
                Map.entry(Schema.Primitive.I32,"361f4536eee9f991"),Map.entry(Schema.Primitive.I64,"c6eb8c46f1e17fba"),
                Map.entry(Schema.Primitive.I128,"e935ee7d4b9fe594"),Map.entry(Schema.Primitive.F32,"8e02f623d1b2310c"),
                Map.entry(Schema.Primitive.F64,"3f2e589db81e95bf"),Map.entry(Schema.Primitive.CHAR,"18937b725e2e911b"),
                Map.entry(Schema.Primitive.STRING,"6d7dce914ee150e8"),Map.entry(Schema.Primitive.BYTES,"ba8125876d6388b4"),
                Map.entry(Schema.Primitive.DATETIME,"2df96deecf87538d"),Map.entry(Schema.Primitive.UUID,"228b7a9a7c76c62c"),
                Map.entry(Schema.Primitive.QNAME,"18b4e7af90ad4c0f"),Map.entry(Schema.Primitive.UNIT,"bc5c33249a2dc720"),
                Map.entry(Schema.Primitive.NEVER,"5db70a394660f3e6"));
        for(Map.Entry<Schema.Primitive,String>entry:expected.entrySet())
            equal(entry.getValue(),Schema.primitive(entry.getKey()).id().toString(),"primitive "+entry.getKey());
    }

    private static void acceptedSchemaCorpusRoundTrips(Path repository)throws Exception{
        Path root=repository.resolve("phon/conformance/cases");
        for(String group:List.of("point","enum_shapes","linked_list","generics")){
            List<Schema>schemas=new ArrayList<>();List<byte[]>bytes=new ArrayList<>();
            try(var files=Files.list(root.resolve(group))){for(Path file:files.filter(p->p.toString().endsWith(".phon")).sorted().toList()){byte[]wire=Files.readAllBytes(file);Schema schema=SchemaWire.decode(wire,PhonLimits.DEFAULT);schemas.add(schema);bytes.add(wire);same(wire,SchemaWire.encode(schema),"schema roundtrip "+file);}}
            for(Schema schema:schemas){List<Schema>reachable=new ArrayList<>(schemas);reachable.remove(schema);new SchemaClosure(schema,reachable);}
        }
        for(String name:List.of("list.phon","map.phon")){
            Path file=root.resolve("containers").resolve(name);byte[]wire=Files.readAllBytes(file);Schema schema=SchemaWire.decode(wire,PhonLimits.DEFAULT);
            same(wire,SchemaWire.encode(schema),"schema roundtrip "+name);new SchemaClosure(schema,List.of());
        }
    }

    private static void typedCodecRoundTrips()throws Exception{
        PhonAdapter<String>strings=new PhonAdapter<>(){
            private final SchemaClosure schema=closure(Schema.primitive(Schema.Primitive.STRING));
            public SchemaClosure schema(){return schema;}public void encode(PhonEncoder e,String v)throws PhonException{e.writeString(v);}
            public String decode(PhonDecoder d)throws PhonException{return d.readString();}
        };
        String value="héllo λ \uD83C\uDF0D";byte[]wire=PhonCodec.encode(strings,value,PhonLimits.DEFAULT);
        equal(value,PhonCodec.decode(strings,wire,PhonLimits.DEFAULT),"typed Unicode");
        PhonAdapter<BigInteger>u128=new PhonAdapter<>(){
            private final SchemaClosure schema=closure(Schema.primitive(Schema.Primitive.U128));
            public SchemaClosure schema(){return schema;}public void encode(PhonEncoder e,BigInteger v)throws PhonException{e.writeU128(v);}
            public BigInteger decode(PhonDecoder d)throws PhonException{return d.readU128();}
        };
        BigInteger max=BigInteger.ONE.shiftLeft(128).subtract(BigInteger.ONE);
        equal(max,PhonCodec.decode(u128,PhonCodec.encode(u128,max,PhonLimits.DEFAULT),PhonLimits.DEFAULT),"u128 boundary");
    }

    private static void compatibilityPlansEagerly()throws Exception{
        SchemaClosure same=closure(Schema.primitive(Schema.Primitive.STRING));
        CompatibilityPlan.plan(same,same,PhonLimits.DEFAULT);
        boolean failed=false;try{CompatibilityPlan.plan(same,closure(Schema.primitive(Schema.Primitive.U32)),PhonLimits.DEFAULT);}catch(PhonException e){failed=e.kind()==PhonException.Kind.INCOMPATIBLE;}
        check(failed,"primitive incompatibility is detected during planning");
    }

    private static void malformedAndLimitsFail(Path repository)throws Exception{
        PhonAdapter<String>adapter=new PhonAdapter<>(){
            private final SchemaClosure schema=closure(Schema.primitive(Schema.Primitive.STRING));
            public SchemaClosure schema(){return schema;}public void encode(PhonEncoder e,String v)throws PhonException{e.writeString(v);}
            public String decode(PhonDecoder d)throws PhonException{return d.readString();}
        };
        byte[] good=PhonCodec.encode(adapter,"abc",PhonLimits.DEFAULT);
        for(int length=0;length<good.length;length++){boolean failed=false;try{PhonCodec.decode(adapter,Arrays.copyOf(good,length),PhonLimits.DEFAULT);}catch(PhonException e){failed=true;}check(failed,"truncation "+length);}
        PhonLimits tiny=new PhonLimits(2,32,2,2,2,2,2);boolean bounded=false;try{PhonCodec.decode(adapter,good,tiny);}catch(PhonException e){bounded=e.kind()==PhonException.Kind.LIMIT;}check(bounded,"input limit");
        byte[] badPresence={2};PhonDecoder decoder=new PhonDecoder(badPresence,PhonLimits.DEFAULT);boolean invalid=false;try{decoder.readPresence();}catch(PhonException e){invalid=e.kind()==PhonException.Kind.MALFORMED;}check(invalid,"invalid discriminant");
        PhonLimits schemaTiny=new PhonLimits(1024,1,8,8,8,8,8);boolean schemaBound=false;
        byte[]point=Files.readAllBytes(repository.resolve("phon/conformance/cases/point/Point.phon"));
        try{SchemaWire.decode(point,schemaTiny);}catch(PhonException e){schemaBound=e.kind()==PhonException.Kind.LIMIT;}check(schemaBound,"schema byte limit");
        PhonLimits shallow=new PhonLimits(64,64,2,8,8,8,8);boolean depthBound=false;
        try{ValueWire.decode(new byte[]{25,25,25,24},shallow);}catch(PhonException e){depthBound=e.kind()==PhonException.Kind.LIMIT;}check(depthBound,"nesting limit");
        byte[]threeNulls={17,3,0,0,0,24,24,24};boolean countBound=false;
        try{ValueWire.decode(threeNulls,new PhonLimits(64,64,8,2,8,8,8));}catch(PhonException e){countBound=e.kind()==PhonException.Kind.LIMIT;}check(countBound,"collection entry limit");
        byte[]selfDescribingString={15,3,0,0,0,'a','b','c'};boolean runBound=false;try{ValueWire.decode(selfDescribingString,new PhonLimits(64,64,8,8,2,8,8));}catch(PhonException e){runBound=e.kind()==PhonException.Kind.LIMIT;}check(runBound,"byte run limit");
        boolean refsBound=false;try{new SchemaClosure(Schema.primitive(Schema.Primitive.STRING),List.of(Schema.primitive(Schema.Primitive.U32)),new PhonLimits(64,512,8,8,8,1,8));}catch(PhonException e){refsBound=e.kind()==PhonException.Kind.LIMIT;}check(refsBound,"referenced schema limit");
        Schema pointSchema=SchemaWire.decode(point,PhonLimits.DEFAULT);SchemaClosure pointClosure=SchemaClosure.of(pointSchema);boolean workBound=false;
        try{CompatibilityPlan.plan(pointClosure,pointClosure,new PhonLimits(1024,1024,8,8,8,8,1));}catch(PhonException e){workBound=e.kind()==PhonException.Kind.LIMIT;}check(workBound,"planning work limit");
    }
    private static SchemaClosure closure(Schema schema){try{return SchemaClosure.of(schema);}catch(PhonException e){throw new ExceptionInInitializerError(e);}}
    private static void same(byte[]a,byte[]b,String message){check(Arrays.equals(a,b),message);}
    private static void equal(Object a,Object b,String message){check(a.equals(b),message+": expected "+a+", got "+b);}
    private static void check(boolean value,String message){assertions++;if(!value)throw new AssertionError(message);}
}
