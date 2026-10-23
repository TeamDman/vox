package org.facet.phon;

import java.nio.charset.StandardCharsets;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Comparator;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.TreeSet;

/** Canonical PHONSCM1 schema table. Distinct from Vox's root/auxiliary closure envelope. */
public final class SchemaBundle {
    private static final byte[] MAGIC = "PHONSCM1".getBytes(StandardCharsets.US_ASCII);
    private static final Comparator<String> UTF8_ORDER = (left,right) -> Arrays.compareUnsigned(
            left.getBytes(StandardCharsets.UTF_8),right.getBytes(StandardCharsets.UTF_8));
    private final List<Schema> schemas;
    private SchemaBundle(List<Schema> schemas){this.schemas=List.copyOf(schemas);}
    public List<Schema> schemas(){return schemas;}

    public static byte[] encode(List<Schema> input,SchemaBundleLimits limits)throws PhonException{
        validate(input,limits);
        List<Schema>schemas=new ArrayList<>(input);schemas.sort(Comparator.comparing(Schema::id));
        TreeSet<String>names=new TreeSet<>(UTF8_ORDER);
        for(Schema schema:schemas){names.addAll(schema.typeParameters());collectKind(schema.kind(),names);}
        names.remove("");
        if(names.size()>limits.strings())throw SchemaWire.limit("schema bundle string count");
        long owned=0;Map<String,Integer>indices=new HashMap<>();
        for(String name:names){owned+=SchemaWire.utf8(name).length;if(owned>limits.ownedBytes())throw SchemaWire.limit("schema bundle owned bytes");indices.put(name,indices.size());}
        SchemaWire.Out out=new SchemaWire.Out(limits.totalBytes(),indices);
        out.raw(MAGIC);out.u32(1);out.struct("SchemaBundleV1",2);out.name("strings");out.list(names.size());
        for(String name:names)out.valueString(name);
        out.name("schemas");out.list(schemas.size());for(Schema schema:schemas)SchemaWire.schema(out,schema);
        return out.bytes();
    }

    public static SchemaBundle decode(byte[] bytes,SchemaBundleLimits limits)throws PhonException{
        if(bytes.length>limits.totalBytes())throw SchemaWire.limit("schema bundle total bytes");
        SchemaWire.In in=new SchemaWire.In(bytes,limits.phonLimits(),limits.ownedBytes());
        for(byte expected:MAGIC)if(in.tag()!=(expected&255))throw in.error("schema bundle magic");
        if(in.u32()!=1)throw in.error("schema bundle version");
        in.struct("SchemaBundleV1",2);in.field("strings");int count=in.list();
        if(count>limits.strings())throw SchemaWire.limit("schema bundle string count");
        List<String>names=new ArrayList<>();String previous=null;
        for(int i=0;i<count;i++){
            String name=in.valueString();
            if(name.isEmpty()||(previous!=null&&UTF8_ORDER.compare(previous,name)>=0))throw in.error("schema bundle string table");
            names.add(name);previous=name;
        }
        in.symbols=names;in.field("schemas");int schemaCount=in.list();
        if(schemaCount>limits.schemas())throw SchemaWire.limit("schema bundle schema count");
        List<Schema>schemas=new ArrayList<>();SchemaId previousId=null;
        for(int i=0;i<schemaCount;i++){
            Schema schema=SchemaWire.schema(in,0);
            if(previousId!=null&&previousId.compareTo(schema.id())>=0)throw in.error("schema bundle schema order");
            schemas.add(schema);previousId=schema.id();
        }
        in.finished();if(in.usedSymbols.size()!=names.size())throw in.error("unused schema bundle string");
        validate(schemas,limits);return new SchemaBundle(schemas);
    }

    static void validate(List<Schema>schemas,SchemaBundleLimits limits)throws PhonException{
        if(schemas.size()>limits.schemas())throw SchemaWire.limit("schema bundle schema count");
        Map<SchemaId,Integer>arities=new HashMap<>();Set<SchemaId>primitives=new HashSet<>();
        for(Schema.Primitive primitive:Schema.Primitive.values())primitives.add(SchemaIdentity.primitiveId(primitive));
        for(Schema schema:schemas)if(arities.put(schema.id(),schema.typeParameters().size())!=null)throw invalid("duplicate schema id");
        int work=0;
        for(Schema schema:schemas){
            Set<String>parameters=new HashSet<>(schema.typeParameters());
            if(parameters.size()!=schema.typeParameters().size()||parameters.contains(""))throw invalid("schema type parameters");
            ArrayDeque<RefVisit>pending=new ArrayDeque<>();
            for(Schema.Ref ref:directRefs(schema.kind()))pending.addLast(new RefVisit(ref,0));
            while(!pending.isEmpty()){
                RefVisit visit=pending.removeLast();Schema.Ref ref=visit.ref;
                if(++work>limits.work()||visit.depth>limits.nesting())throw SchemaWire.limit("schema bundle validation work or depth");
                if(ref.isVariable()){
                    if(!parameters.contains(ref.variable()))throw invalid("undeclared schema variable");
                }else{
                    Integer arity=arities.get(ref.id());
                    if(primitives.contains(ref.id())){if(!ref.arguments().isEmpty())throw invalid("primitive schema arguments");}
                    else if(arity==null)throw invalid("missing schema reference");
                    else if(arity!=ref.arguments().size())throw invalid("schema argument arity");
                    for(Schema.Ref argument:ref.arguments())pending.addLast(new RefVisit(argument,visit.depth+1));
                }
            }
        }
        Map<SchemaId,SchemaId>actual=SchemaIdentity.recompute(schemas,limits.phonLimits());
        for(Schema schema:schemas)if(!schema.id().equals(actual.get(schema.id())))throw invalid("schema id mismatch");
    }
    private static final class RefVisit{
        final Schema.Ref ref;final int depth;RefVisit(Schema.Ref ref,int depth){this.ref=ref;this.depth=depth;}
    }
    static List<Schema.Ref> directRefs(Schema.Kind kind){
        List<Schema.Ref>refs=new ArrayList<>();
        if(kind instanceof Schema.RecordKind)for(Schema.Field field:((Schema.RecordKind)kind).fields())refs.add(field.schema());
        else if(kind instanceof Schema.EnumKind)for(Schema.Variant variant:((Schema.EnumKind)kind).variants())payloadRefs(variant.payload(),refs);
        else if(kind instanceof Schema.TupleKind)refs.addAll(((Schema.TupleKind)kind).elements());
        else if(kind instanceof Schema.ListKind)refs.add(((Schema.ListKind)kind).element());
        else if(kind instanceof Schema.SetKind)refs.add(((Schema.SetKind)kind).element());
        else if(kind instanceof Schema.OptionKind)refs.add(((Schema.OptionKind)kind).element());
        else if(kind instanceof Schema.ArrayKind)refs.add(((Schema.ArrayKind)kind).element());
        else if(kind instanceof Schema.TensorKind)refs.add(((Schema.TensorKind)kind).element());
        else if(kind instanceof Schema.ChannelKind)refs.add(((Schema.ChannelKind)kind).element());
        else if(kind instanceof Schema.MapKind){refs.add(((Schema.MapKind)kind).key());refs.add(((Schema.MapKind)kind).value());}
        else if(kind instanceof Schema.ExternalKind){Schema.Ref metadata=((Schema.ExternalKind)kind).metadata();if(metadata!=null)refs.add(metadata);}
        else if(kind instanceof Schema.SemanticKind){Schema.SemanticKind semantic=(Schema.SemanticKind)kind;refs.addAll(semantic.arguments());refs.add(semantic.representation());}
        return refs;
    }
    private static void payloadRefs(Schema.Payload payload,List<Schema.Ref>refs){
        if(payload instanceof Schema.NewtypePayload)refs.add(((Schema.NewtypePayload)payload).ref());
        else if(payload instanceof Schema.TuplePayload)refs.addAll(((Schema.TuplePayload)payload).refs());
        else if(payload instanceof Schema.RecordPayload)for(Schema.Field field:((Schema.RecordPayload)payload).fields())refs.add(field.schema());
    }
    private static void collectKind(Schema.Kind kind,Set<String>names){
        if(kind instanceof Schema.RecordKind){Schema.RecordKind record=(Schema.RecordKind)kind;names.add(record.name());for(Schema.Field field:record.fields())names.add(field.name());}
        else if(kind instanceof Schema.EnumKind){Schema.EnumKind enumeration=(Schema.EnumKind)kind;names.add(enumeration.name());for(Schema.Variant variant:enumeration.variants()){names.add(variant.name());if(variant.payload() instanceof Schema.RecordPayload)for(Schema.Field field:((Schema.RecordPayload)variant.payload()).fields())names.add(field.name());}}
        else if(kind instanceof Schema.ExternalKind)names.add(((Schema.ExternalKind)kind).name());
        else if(kind instanceof Schema.SemanticKind)names.add(((Schema.SemanticKind)kind).name().value());
        SchemaIdentity.visitRefs(kind,ref->{if(ref.isVariable())names.add(ref.variable());});
    }
    private static PhonException invalid(String message){return new PhonException(PhonException.Kind.SCHEMA,message);}
}
