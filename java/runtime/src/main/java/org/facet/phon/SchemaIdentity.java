package org.facet.phon;

import java.io.ByteArrayOutputStream;
import java.nio.charset.StandardCharsets;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.HashSet;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;

/** Canonical structural encoding and recursive schema-id computation. */
final class SchemaIdentity {
    private SchemaIdentity() {}

    static SchemaId primitiveId(Schema.Primitive primitive) {
        Sink sink = new Sink(); sink.string(primitive.tag()); return hash(sink.bytes());
    }

    static Map<SchemaId, SchemaId> recompute(List<Schema> schemas, PhonLimits limits) throws PhonException {
        if (schemas.size() > limits.referencedSchemas()) throw new PhonException(PhonException.Kind.LIMIT, "schema count exceeds referencedSchemas");
        Map<SchemaId, Integer> index = new HashMap<>();
        for (int i = 0; i < schemas.size(); i++) {
            if (index.put(schemas.get(i).id(), i) != null) throw new PhonException(PhonException.Kind.SCHEMA, "duplicate schema id");
        }
        List<List<Integer>> edges = new ArrayList<>();
        for (Schema schema : schemas) {
            Set<Integer> unique = new HashSet<>();
            visitRefs(schema.kind(), ref -> { if (!ref.isVariable() && index.containsKey(ref.id())) unique.add(index.get(ref.id())); });
            edges.add(new ArrayList<>(unique));
        }
        List<List<Integer>> components = tarjan(edges);
        Map<Integer, SchemaId> assigned = new HashMap<>();
        for (List<Integer> componentList : components) {
            Set<Integer> component = new HashSet<>(componentList);
            for (int node : componentList) {
                Sink sink = new Sink();
                walkSchema(schemas, index, component, assigned, node, new ArrayList<>(List.of(node)), sink);
                assigned.put(node, hash(sink.bytes()));
            }
        }
        Map<SchemaId, SchemaId> result = new LinkedHashMap<>();
        for (int i = 0; i < schemas.size(); i++) result.put(schemas.get(i).id(), assigned.get(i));
        return result;
    }

    private interface RefConsumer { void accept(Schema.Ref ref); }
    private static void visitRefs(Schema.Kind kind, RefConsumer consumer) {
        if (kind instanceof Schema.RecordKind) for (Schema.Field f : ((Schema.RecordKind) kind).fields()) visitRef(f.schema(), consumer);
        else if (kind instanceof Schema.EnumKind) for (Schema.Variant v : ((Schema.EnumKind) kind).variants()) visitPayload(v.payload(), consumer);
        else if (kind instanceof Schema.TupleKind) for (Schema.Ref r : ((Schema.TupleKind) kind).elements()) visitRef(r, consumer);
        else if (kind instanceof Schema.ListKind) visitRef(((Schema.ListKind) kind).element(), consumer);
        else if (kind instanceof Schema.SetKind) visitRef(((Schema.SetKind) kind).element(), consumer);
        else if (kind instanceof Schema.OptionKind) visitRef(((Schema.OptionKind) kind).element(), consumer);
        else if (kind instanceof Schema.MapKind) { visitRef(((Schema.MapKind) kind).key(), consumer); visitRef(((Schema.MapKind) kind).value(), consumer); }
        else if (kind instanceof Schema.ArrayKind) visitRef(((Schema.ArrayKind) kind).element(), consumer);
        else if (kind instanceof Schema.TensorKind) visitRef(((Schema.TensorKind) kind).element(), consumer);
    }
    private static void visitPayload(Schema.Payload payload, RefConsumer consumer) {
        if (payload instanceof Schema.NewtypePayload) visitRef(((Schema.NewtypePayload) payload).ref(), consumer);
        else if (payload instanceof Schema.TuplePayload) for (Schema.Ref r : ((Schema.TuplePayload) payload).refs()) visitRef(r, consumer);
        else if (payload instanceof Schema.RecordPayload) for (Schema.Field f : ((Schema.RecordPayload) payload).fields()) visitRef(f.schema(), consumer);
    }
    private static void visitRef(Schema.Ref ref, RefConsumer consumer) {
        consumer.accept(ref); for (Schema.Ref arg : ref.arguments()) visitRef(arg, consumer);
    }

    private static void walkSchema(List<Schema> schemas, Map<SchemaId,Integer> index, Set<Integer> component,
                                   Map<Integer,SchemaId> assigned, int node, List<Integer> path, Sink out) {
        Schema schema = schemas.get(node); Schema.Kind kind = schema.kind();
        if (kind instanceof Schema.PrimitiveKind) out.string(((Schema.PrimitiveKind) kind).primitive().tag());
        else if (kind instanceof Schema.RecordKind) {
            Schema.RecordKind k = (Schema.RecordKind) kind; out.string("struct"); out.string(k.name()); typeParams(schema, out);
            out.u32(k.fields().size()); for (Schema.Field f : k.fields()) field(schemas,index,component,assigned,f,path,out);
        } else if (kind instanceof Schema.EnumKind) {
            Schema.EnumKind k = (Schema.EnumKind) kind; out.string("enum"); out.string(k.name()); typeParams(schema,out); out.u32(k.variants().size());
            for (Schema.Variant v : k.variants()) {
                out.string(v.name()); out.u32(v.index()); Schema.Payload p = v.payload();
                if (p instanceof Schema.UnitPayload) out.string("unit");
                else if (p instanceof Schema.NewtypePayload) { out.string("newtype"); ref(schemas,index,component,assigned,((Schema.NewtypePayload)p).ref(),path,out); }
                else if (p instanceof Schema.TuplePayload) {
                    out.string("tuple"); List<Schema.Ref> rs=((Schema.TuplePayload)p).refs(); out.u32(rs.size());
                    for (Schema.Ref r:rs) ref(schemas,index,component,assigned,r,path,out);
                } else {
                    out.string("struct"); List<Schema.Field> fs=((Schema.RecordPayload)p).fields(); out.u32(fs.size());
                    for (Schema.Field f:fs) field(schemas,index,component,assigned,f,path,out);
                }
            }
        } else if (kind instanceof Schema.TupleKind) { out.string("tuple"); refs(schemas,index,component,assigned,((Schema.TupleKind)kind).elements(),path,out); }
        else if (kind instanceof Schema.ListKind) { out.string("list"); ref(schemas,index,component,assigned,((Schema.ListKind)kind).element(),path,out); }
        else if (kind instanceof Schema.SetKind) { out.string("set"); ref(schemas,index,component,assigned,((Schema.SetKind)kind).element(),path,out); }
        else if (kind instanceof Schema.OptionKind) { out.string("option"); ref(schemas,index,component,assigned,((Schema.OptionKind)kind).element(),path,out); }
        else if (kind instanceof Schema.MapKind) {
            out.string("map"); ref(schemas,index,component,assigned,((Schema.MapKind)kind).key(),path,out);
            ref(schemas,index,component,assigned,((Schema.MapKind)kind).value(),path,out);
        } else if (kind instanceof Schema.ArrayKind) {
            Schema.ArrayKind k=(Schema.ArrayKind)kind; out.string("array"); ref(schemas,index,component,assigned,k.element(),path,out);
            out.u32(k.dimensions().size()); for(long d:k.dimensions()) out.u64(d);
        } else if (kind instanceof Schema.TensorKind) {
            Schema.TensorKind k=(Schema.TensorKind)kind; out.string("tensor"); ref(schemas,index,component,assigned,k.element(),path,out);
            out.u8(k.rank()==null?0:1); if(k.rank()!=null)out.u32(k.rank());
        } else if (kind instanceof Schema.DynamicKind) out.string("dynamic");
        else throw new IllegalArgumentException("unsupported canonical kind " + ((Schema.UnsupportedKind)kind).name());
    }
    private static void typeParams(Schema schema,Sink out){out.u32(schema.typeParameters().size());for(String p:schema.typeParameters())out.string(p);}
    private static void field(List<Schema>s,Map<SchemaId,Integer>i,Set<Integer>c,Map<Integer,SchemaId>a,Schema.Field f,List<Integer>p,Sink o){
        o.string(f.name());o.u8(f.required()?1:0);ref(s,i,c,a,f.schema(),p,o);
    }
    private static void refs(List<Schema>s,Map<SchemaId,Integer>i,Set<Integer>c,Map<Integer,SchemaId>a,List<Schema.Ref>rs,List<Integer>p,Sink o){
        o.u32(rs.size());for(Schema.Ref r:rs)ref(s,i,c,a,r,p,o);
    }
    private static void ref(List<Schema>s,Map<SchemaId,Integer>i,Set<Integer>c,Map<Integer,SchemaId>a,Schema.Ref r,List<Integer>p,Sink o){
        if(r.isVariable()){o.string("var");o.string(r.variable());return;}
        Integer target=i.get(r.id());
        if(target!=null&&c.contains(target)){
            int depth=p.indexOf(target);
            if(depth>=0){o.string("backref");o.u32(depth);}
            else{o.string("inline");List<Integer>next=new ArrayList<>(p);next.add(target);walkSchema(s,i,c,a,target,next,o);}
        }else{o.string("concrete");o.u64(target==null?r.id().asLong():a.get(target).asLong());}
        refs(s,i,c,a,r.arguments(),p,o);
    }
    private static SchemaId hash(byte[] bytes){return new SchemaId(java.util.Arrays.copyOf(Blake3.hash(bytes),8));}

    private static List<List<Integer>> tarjan(List<List<Integer>> graph) {
        int n=graph.size();int[] order=new int[n],low=new int[n];java.util.Arrays.fill(order,-1);
        boolean[] on=new boolean[n];ArrayDeque<Integer> stack=new ArrayDeque<>();List<List<Integer>> result=new ArrayList<>();int[] next={0};
        for(int i=0;i<n;i++)if(order[i]<0)strong(i,graph,order,low,on,stack,result,next);return result;
    }
    private static void strong(int v,List<List<Integer>>g,int[]o,int[]l,boolean[]on,ArrayDeque<Integer>s,List<List<Integer>>r,int[]next){
        o[v]=l[v]=next[0]++;s.addLast(v);on[v]=true;
        for(int w:g.get(v)){if(o[w]<0){strong(w,g,o,l,on,s,r,next);l[v]=Math.min(l[v],l[w]);}else if(on[w])l[v]=Math.min(l[v],o[w]);}
        if(l[v]==o[v]){List<Integer>c=new ArrayList<>();int w;do{w=s.removeLast();on[w]=false;c.add(w);}while(w!=v);r.add(c);}
    }

    static final class Sink {
        private final ByteArrayOutputStream out=new ByteArrayOutputStream();
        void u8(int n){out.write(n);}
        void u32(long n){for(int i=0;i<4;i++)out.write((int)(n>>>(8*i))&255);}
        void u64(long n){for(int i=0;i<8;i++)out.write((int)(n>>>(8*i))&255);}
        void string(String value){byte[]b=value.getBytes(StandardCharsets.UTF_8);u32(b.length);out.write(b,0,b.length);}
        byte[] bytes(){return out.toByteArray();}
    }
}
