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
        try {
        Budget budget = new Budget(limits);
        if (schemas.size() > limits.referencedSchemas()) throw new PhonException(PhonException.Kind.LIMIT, "schema count exceeds referencedSchemas");
        Map<SchemaId, Integer> index = new HashMap<>();
        for (int i = 0; i < schemas.size(); i++) {
            if (index.put(schemas.get(i).id(), i) != null) throw new PhonException(PhonException.Kind.SCHEMA, "duplicate schema id");
        }
        List<List<Integer>> edges = new ArrayList<>();
        for (Schema schema : schemas) {
            Set<Integer> unique = new HashSet<>();
            ArrayDeque<RefAtDepth> pending = new ArrayDeque<>();
            for (Schema.Ref ref : SchemaBundle.directRefs(schema.kind())) pending.addLast(new RefAtDepth(ref, 0));
            while (!pending.isEmpty()) {
                RefAtDepth item = pending.removeLast(); budget.step();
                if (item.depth > limits.nestingDepth()) throw new LimitFailure("schema reference nesting limit");
                Schema.Ref ref = item.ref;
                if (!ref.isVariable() && index.containsKey(ref.id())) unique.add(index.get(ref.id()));
                for (Schema.Ref arg : ref.arguments()) pending.addLast(new RefAtDepth(arg, item.depth + 1));
            }
            edges.add(new ArrayList<>(unique));
        }
        List<List<Integer>> components = tarjan(edges);
        Map<Integer, SchemaId> assigned = new HashMap<>();
        for (List<Integer> componentList : components) {
            Set<Integer> component = new HashSet<>(componentList);
            for (int node : componentList) {
                Sink sink = new Sink(budget);
                walkSchema(schemas, index, component, assigned, node, new ArrayList<>(List.of(node)), sink);
                assigned.put(node, hash(sink.bytes()));
            }
        }
        Map<SchemaId, SchemaId> result = new LinkedHashMap<>();
        for (int i = 0; i < schemas.size(); i++) result.put(schemas.get(i).id(), assigned.get(i));
        return result;
        } catch (LimitFailure failure) {
            throw new PhonException(PhonException.Kind.LIMIT, failure.getMessage());
        }
    }
    private record RefAtDepth(Schema.Ref ref, int depth) {}


    interface RefConsumer { void accept(Schema.Ref ref); }
    static void visitRefs(Schema.Kind kind, RefConsumer consumer) {
        if (kind instanceof Schema.RecordKind) for (Schema.Field f : ((Schema.RecordKind) kind).fields()) visitRef(f.schema(), consumer);
        else if (kind instanceof Schema.EnumKind) for (Schema.Variant v : ((Schema.EnumKind) kind).variants()) visitPayload(v.payload(), consumer);
        else if (kind instanceof Schema.TupleKind) for (Schema.Ref r : ((Schema.TupleKind) kind).elements()) visitRef(r, consumer);
        else if (kind instanceof Schema.ListKind) visitRef(((Schema.ListKind) kind).element(), consumer);
        else if (kind instanceof Schema.SetKind) visitRef(((Schema.SetKind) kind).element(), consumer);
        else if (kind instanceof Schema.OptionKind) visitRef(((Schema.OptionKind) kind).element(), consumer);
        else if (kind instanceof Schema.MapKind) { visitRef(((Schema.MapKind) kind).key(), consumer); visitRef(((Schema.MapKind) kind).value(), consumer); }
        else if (kind instanceof Schema.ArrayKind) visitRef(((Schema.ArrayKind) kind).element(), consumer);
        else if (kind instanceof Schema.TensorKind) visitRef(((Schema.TensorKind) kind).element(), consumer);
        else if (kind instanceof Schema.ChannelKind) visitRef(((Schema.ChannelKind) kind).element(), consumer);
        else if (kind instanceof Schema.ExternalKind) {
            Schema.Ref metadata = ((Schema.ExternalKind) kind).metadata();
            if (metadata != null) visitRef(metadata, consumer);
        } else if (kind instanceof Schema.SemanticKind) {
            Schema.SemanticKind semantic = (Schema.SemanticKind) kind;
            for (Schema.Ref argument : semantic.arguments()) visitRef(argument, consumer);
            visitRef(semantic.representation(), consumer);
        }
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
        out.budget.enter(); try {
        Schema schema = schemas.get(node); Schema.Kind kind = schema.kind();
        typeParams(schema, out);
        if (kind instanceof Schema.PrimitiveKind) out.string(((Schema.PrimitiveKind) kind).primitive().tag());
        else if (kind instanceof Schema.RecordKind) {
            Schema.RecordKind k = (Schema.RecordKind) kind; out.string("struct"); out.string(k.name());
            out.u32(k.fields().size()); for (Schema.Field f : k.fields()) field(schemas,index,component,assigned,f,path,out);
        } else if (kind instanceof Schema.EnumKind) {
            Schema.EnumKind k = (Schema.EnumKind) kind; out.string("enum"); out.string(k.name()); out.u32(k.variants().size());
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
        } else if (kind instanceof Schema.ChannelKind) {
            Schema.ChannelKind k = (Schema.ChannelKind) kind;
            out.string("channel"); out.string(k.direction() == Schema.ChannelDirection.TX ? "tx" : "rx");
            ref(schemas,index,component,assigned,k.element(),path,out);
        } else if (kind instanceof Schema.ExternalKind) {
            Schema.ExternalKind k = (Schema.ExternalKind) kind;
            out.string("external"); out.string(k.name()); out.u8(k.metadata() == null ? 0 : 1);
            if (k.metadata() != null) ref(schemas,index,component,assigned,k.metadata(),path,out);
        } else if (kind instanceof Schema.SemanticKind) {
            Schema.SemanticKind k = (Schema.SemanticKind) kind;
            out.string("semantic"); out.string("name"); out.string(k.name().value());
            out.string("args"); out.u32(k.arguments().size());
            for (Schema.Ref argument : k.arguments()) ref(schemas,index,component,assigned,argument,path,out);
            out.string("representation"); ref(schemas,index,component,assigned,k.representation(),path,out);
        } else if (kind instanceof Schema.DynamicKind) out.string("dynamic");
        else throw new IllegalArgumentException("unsupported canonical kind " + ((Schema.UnsupportedKind)kind).name());
        } finally { out.budget.leave(); }
    }
    private static void typeParams(Schema schema,Sink out){
        if(schema.typeParameters().isEmpty())return;
        out.string("type-params");out.u32(schema.typeParameters().size());
        for(String p:schema.typeParameters())out.string(p);
    }
    private static void field(List<Schema>s,Map<SchemaId,Integer>i,Set<Integer>c,Map<Integer,SchemaId>a,Schema.Field f,List<Integer>p,Sink o){
        o.string(f.name());o.u8(f.required()?1:0);ref(s,i,c,a,f.schema(),p,o);
    }
    private static void refs(List<Schema>s,Map<SchemaId,Integer>i,Set<Integer>c,Map<Integer,SchemaId>a,List<Schema.Ref>rs,List<Integer>p,Sink o){
        o.u32(rs.size());for(Schema.Ref r:rs)ref(s,i,c,a,r,p,o);
    }
    private static void ref(List<Schema>s,Map<SchemaId,Integer>i,Set<Integer>c,Map<Integer,SchemaId>a,Schema.Ref r,List<Integer>p,Sink o){
        o.budget.enter(); try {
        if(r.isVariable()){o.string("var");o.string(r.variable());return;}
        Integer target=i.get(r.id());
        if(target!=null&&c.contains(target)){
            int depth=p.indexOf(target);
            if(depth>=0){o.string("backref");o.u32(depth);}
            else{o.string("inline");List<Integer>next=new ArrayList<>(p);next.add(target);walkSchema(s,i,c,a,target,next,o);}
        }else{o.string("concrete");o.u64(target==null?r.id().asLong():a.get(target).asLong());}
        refs(s,i,c,a,r.arguments(),p,o);
        } finally { o.budget.leave(); }
    }
    private static SchemaId hash(byte[] bytes){return new SchemaId(java.util.Arrays.copyOf(Blake3.hash(bytes),8));}

    /** Iterative DFS avoids using the Java call stack for attacker-sized graphs. */
    private static List<List<Integer>> tarjan(List<List<Integer>> graph) {
        int n=graph.size(), next=0;
        int[] order=new int[n],low=new int[n],edge=new int[n];
        java.util.Arrays.fill(order,-1);
        boolean[] on=new boolean[n];
        ArrayDeque<Integer> stack=new ArrayDeque<>(), dfs=new ArrayDeque<>();
        List<List<Integer>> result=new ArrayList<>();
        for(int root=0;root<n;root++){
            if(order[root]>=0)continue;
            order[root]=low[root]=next++;stack.addLast(root);on[root]=true;dfs.addLast(root);
            while(!dfs.isEmpty()){
                int v=dfs.peekLast();
                if(edge[v]<graph.get(v).size()){
                    int w=graph.get(v).get(edge[v]++);
                    if(order[w]<0){order[w]=low[w]=next++;stack.addLast(w);on[w]=true;dfs.addLast(w);}
                    else if(on[w])low[v]=Math.min(low[v],order[w]);
                }else{
                    dfs.removeLast();
                    if(!dfs.isEmpty()){int parent=dfs.peekLast();low[parent]=Math.min(low[parent],low[v]);}
                    if(low[v]==order[v]){
                        List<Integer> component=new ArrayList<>();int w;
                        do{w=stack.removeLast();on[w]=false;component.add(w);}while(w!=v);
                        result.add(component);
                    }
                }
            }
        }
        return result;
    }

    private static final class LimitFailure extends RuntimeException {
        private static final long serialVersionUID=1L;
        LimitFailure(String message){super(message);}
    }
    private static final class Budget {
        final PhonLimits limits;long work,bytes;int depth;
        Budget(PhonLimits limits){this.limits=limits;}
        void step(){if(++work>limits.planningWork())throw new LimitFailure("schema identity work limit");}
        void enter(){step();if(++depth>limits.nestingDepth())throw new LimitFailure("schema identity nesting limit");}
        void leave(){depth--;}
        void reserve(int count){step();bytes+=count;if(bytes>limits.inputBytes())throw new LimitFailure("schema identity byte limit");}
    }
    static final class Sink {
        private final ByteArrayOutputStream out=new ByteArrayOutputStream();
        final Budget budget;
        Sink(){this(new Budget(PhonLimits.DEFAULT));}
        Sink(Budget budget){this.budget=budget;}
        void u8(int n){budget.reserve(1);out.write(n);}
        void u32(long n){budget.reserve(4);for(int i=0;i<4;i++)out.write((int)(n>>>(8*i))&255);}
        void u64(long n){budget.reserve(8);for(int i=0;i<8;i++)out.write((int)(n>>>(8*i))&255);}
        void string(String value){byte[]b=value.getBytes(StandardCharsets.UTF_8);u32(b.length);budget.reserve(b.length);out.write(b,0,b.length);}
        byte[] bytes(){return out.toByteArray();}
    }
}
