package org.facet.phon;

import java.util.ArrayList;
import java.util.HashMap;
import java.util.HashSet;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;

/** Reusable, eagerly validated writer-to-reader compatibility plan. */
public final class CompatibilityPlan {
    private final SchemaClosure writer;
    private final SchemaClosure reader;
    private final PhonLimits planningLimits;

    private CompatibilityPlan(SchemaClosure writer, SchemaClosure reader, PhonLimits limits) throws PhonException {
        this.writer=writer;this.reader=reader;this.planningLimits=limits;
        Planner planner=new Planner(writer,reader,limits);planner.check(Schema.Ref.concrete(writer.id()),Schema.Ref.concrete(reader.id()),"$");
    }

    public static CompatibilityPlan plan(SchemaClosure writer, SchemaClosure reader, PhonLimits limits)
            throws PhonException {
        return new CompatibilityPlan(writer,reader,limits);
    }

    public <T> T decode(byte[] bytes, PhonAdapter<T> readerAdapter, PhonLimits limits)
            throws PhonException {
        if(!reader.id().equals(readerAdapter.schema().id()))
            throw new PhonException(PhonException.Kind.INCOMPATIBLE,"adapter schema differs from planned reader");
        Value source=PhonCodec.decodeValue(writer,bytes,limits);
        Translator translator=new Translator(writer,reader,planningLimits);
        Value translated=translator.translate(Schema.Ref.concrete(writer.id()),Schema.Ref.concrete(reader.id()),source,"$");
        return PhonCodec.decode(readerAdapter,CompactValues.encode(reader,translated,limits),limits);
    }

    public Value translate(Value source) throws PhonException {
        Translator translator = new Translator(writer, reader, planningLimits);
        return translator.translate(
                Schema.Ref.concrete(writer.id()),
                Schema.Ref.concrete(reader.id()),
                source,
                "$");
    }

    private static final class Planner {
        final SchemaClosure w,r;final PhonLimits limits;int work;final Set<String>seen=new HashSet<>();
        Planner(SchemaClosure w,SchemaClosure r,PhonLimits l){this.w=w;this.r=r;this.limits=l;}
        void check(Schema.Ref wr,Schema.Ref rr,String path)throws PhonException{
            if(++work>limits.planningWork())throw new PhonException(PhonException.Kind.LIMIT,"compatibility planning exceeds planningWork");
            String key=refKey(wr)+"->"+refKey(rr);if(!seen.add(key))return;Schema.Kind wk=resolvedKind(w,wr),rk=resolvedKind(r,rr);
            if(wk instanceof Schema.PrimitiveKind&&rk instanceof Schema.PrimitiveKind){
                if(((Schema.PrimitiveKind)wk).primitive()!=((Schema.PrimitiveKind)rk).primitive())bad(path,"primitive mismatch");return;
            }
            if(wk instanceof Schema.DynamicKind&&rk instanceof Schema.DynamicKind)return;
            if(wk instanceof Schema.RecordKind&&rk instanceof Schema.RecordKind){
                Map<String,Schema.Field>wf=fields(((Schema.RecordKind)wk).fields());
                for(Schema.Field f:((Schema.RecordKind)rk).fields()){Schema.Field from=wf.get(f.name());if(from==null){if(f.required()||!acceptsMissing(r,f.schema()))bad(path+"."+f.name(),"reader field absent from writer and has no wire default");}else check(from.schema(),f.schema(),path+"."+f.name());}return;
            }
            if(wk instanceof Schema.TupleKind&&rk instanceof Schema.TupleKind){
                List<Schema.Ref>a=((Schema.TupleKind)wk).elements(),b=((Schema.TupleKind)rk).elements();if(a.size()!=b.size())bad(path,"tuple arity mismatch");for(int i=0;i<a.size();i++)check(a.get(i),b.get(i),path+"["+i+"]");return;
            }
            if(wk instanceof Schema.ListKind&&rk instanceof Schema.ListKind){check(((Schema.ListKind)wk).element(),((Schema.ListKind)rk).element(),path+"[]");return;}
            if(wk instanceof Schema.SetKind&&rk instanceof Schema.SetKind){check(((Schema.SetKind)wk).element(),((Schema.SetKind)rk).element(),path+"{}");return;}
            if(wk instanceof Schema.OptionKind&&rk instanceof Schema.OptionKind){check(((Schema.OptionKind)wk).element(),((Schema.OptionKind)rk).element(),path+"?");return;}
            if(wk instanceof Schema.MapKind&&rk instanceof Schema.MapKind){check(((Schema.MapKind)wk).key(),((Schema.MapKind)rk).key(),path+"{key}");check(((Schema.MapKind)wk).value(),((Schema.MapKind)rk).value(),path+"{value}");return;}
            if(wk instanceof Schema.EnumKind&&rk instanceof Schema.EnumKind){
                Map<String,Schema.Variant>rv=variants(((Schema.EnumKind)rk).variants());
                for(Schema.Variant v:((Schema.EnumKind)wk).variants())if(rv.containsKey(v.name()))checkPayload(v.payload(),rv.get(v.name()).payload(),path+"::"+v.name());return;
            }
            bad(path,"incompatible schema kinds");
        }
        void checkPayload(Schema.Payload a,Schema.Payload b,String p)throws PhonException{
            if(a instanceof Schema.UnitPayload&&b instanceof Schema.UnitPayload)return;
            if(a instanceof Schema.NewtypePayload&&b instanceof Schema.NewtypePayload){check(((Schema.NewtypePayload)a).ref(),((Schema.NewtypePayload)b).ref(),p);return;}
            if(a instanceof Schema.TuplePayload&&b instanceof Schema.TuplePayload){List<Schema.Ref>x=((Schema.TuplePayload)a).refs(),y=((Schema.TuplePayload)b).refs();if(x.size()!=y.size())bad(p,"variant tuple arity");for(int i=0;i<x.size();i++)check(x.get(i),y.get(i),p+"["+i+"]");return;}
            if(a instanceof Schema.RecordPayload&&b instanceof Schema.RecordPayload){Map<String,Schema.Field>x=fields(((Schema.RecordPayload)a).fields());for(Schema.Field f:((Schema.RecordPayload)b).fields()){Schema.Field q=x.get(f.name());if(q==null){if(f.required()||!acceptsMissing(r,f.schema()))bad(p+"."+f.name(),"reader variant field absent and has no wire default");}else check(q.schema(),f.schema(),p+"."+f.name());}return;}
            bad(p,"variant payload mismatch");
        }
        void bad(String path,String message)throws PhonException{throw new PhonException(PhonException.Kind.INCOMPATIBLE,message,-1,path);}
    }
    private static final class Translator {
        final SchemaClosure w,r;final PhonLimits limits;int work;
        Translator(SchemaClosure w,SchemaClosure r,PhonLimits l){this.w=w;this.r=r;this.limits=l;}
        Value translate(Schema.Ref wr,Schema.Ref rr,Value value,String path)throws PhonException{
            if(++work>limits.planningWork())throw new PhonException(PhonException.Kind.LIMIT,"translation exceeds planningWork");
            Schema.Kind wk=resolvedKind(w,wr),rk=resolvedKind(r,rr);
            if(wk instanceof Schema.PrimitiveKind)return value;
            if(wk instanceof Schema.DynamicKind&&rk instanceof Schema.DynamicKind)return value;
            if(wk instanceof Schema.RecordKind){Map<String,Value>out=new LinkedHashMap<>(),input=value.asMap();Map<String,Schema.Field>wf=fields(((Schema.RecordKind)wk).fields());for(Schema.Field f:((Schema.RecordKind)rk).fields()){Schema.Field source=wf.get(f.name());out.put(f.name(),source==null?Value.nullValue():translate(source.schema(),f.schema(),input.get(f.name()),path+"."+f.name()));}return Value.map(out);}
            if(wk instanceof Schema.TupleKind){List<Value>out=new ArrayList<>(),in=value.asList();List<Schema.Ref>a=((Schema.TupleKind)wk).elements(),b=((Schema.TupleKind)rk).elements();for(int i=0;i<a.size();i++)out.add(translate(a.get(i),b.get(i),in.get(i),path+"["+i+"]"));return Value.list(out);}
            if(wk instanceof Schema.ListKind){List<Value>out=new ArrayList<>();for(Value x:value.asList())out.add(translate(((Schema.ListKind)wk).element(),((Schema.ListKind)rk).element(),x,path+"[]"));return Value.list(out);}
            if(wk instanceof Schema.SetKind){List<Value>out=new ArrayList<>();for(Value x:value.asList())out.add(translate(((Schema.SetKind)wk).element(),((Schema.SetKind)rk).element(),x,path+"{}"));return Value.list(out);}
            if(wk instanceof Schema.OptionKind)return value.type()==Value.Type.NULL?value:translate(((Schema.OptionKind)wk).element(),((Schema.OptionKind)rk).element(),value,path+"?");
            if(wk instanceof Schema.MapKind){Map<String,Value>out=new LinkedHashMap<>();for(Map.Entry<String,Value>x:value.asMap().entrySet())out.put(x.getKey(),translate(((Schema.MapKind)wk).value(),((Schema.MapKind)rk).value(),x.getValue(),path+"."+x.getKey()));return Value.map(out);}
            if(wk instanceof Schema.EnumKind){Schema.Variant a=variants(((Schema.EnumKind)wk).variants()).get(value.asEnum().variant()),b=variants(((Schema.EnumKind)rk).variants()).get(value.asEnum().variant());if(b==null)throw new PhonException(PhonException.Kind.INCOMPATIBLE,"writer-only enum variant",-1,path);return Value.enumValue(b.name(),payload(a.payload(),b.payload(),value.asEnum().payload(),path));}
            throw new PhonException(PhonException.Kind.INCOMPATIBLE,"unsupported translation",-1,path);
        }
        Value payload(Schema.Payload a,Schema.Payload b,Value v,String p)throws PhonException{
            if(a instanceof Schema.UnitPayload)return Value.nullValue();if(a instanceof Schema.NewtypePayload)return translate(((Schema.NewtypePayload)a).ref(),((Schema.NewtypePayload)b).ref(),v,p);
            if(a instanceof Schema.TuplePayload){List<Value>out=new ArrayList<>(),in=v.asList();List<Schema.Ref>x=((Schema.TuplePayload)a).refs(),y=((Schema.TuplePayload)b).refs();for(int i=0;i<x.size();i++)out.add(translate(x.get(i),y.get(i),in.get(i),p));return Value.list(out);}
            Map<String,Value>out=new LinkedHashMap<>(),in=v.asMap();Map<String,Schema.Field>x=fields(((Schema.RecordPayload)a).fields());for(Schema.Field f:((Schema.RecordPayload)b).fields()){Schema.Field q=x.get(f.name());out.put(f.name(),q==null?Value.nullValue():translate(q.schema(),f.schema(),in.get(f.name()),p+"."+f.name()));}return Value.map(out);
        }
    }
    private static boolean acceptsMissing(SchemaClosure closure,Schema.Ref ref)throws PhonException{return resolvedKind(closure,ref) instanceof Schema.OptionKind;}
    private static String refKey(Schema.Ref ref){if(ref.isVariable())return "$"+ref.variable();StringBuilder out=new StringBuilder(ref.id().toString());if(!ref.arguments().isEmpty()){out.append('<');for(Schema.Ref arg:ref.arguments())out.append(refKey(arg)).append(';');out.append('>');}return out.toString();}
    private static Schema.Kind resolvedKind(SchemaClosure closure,Schema.Ref ref)throws PhonException{Schema schema=closure.resolve(ref);return substitute(schema.kind(),schema.typeParameters(),ref.arguments());}
    private static Schema.Kind substitute(Schema.Kind kind,List<String>parameters,List<Schema.Ref>arguments)throws PhonException{
        if(parameters.size()!=arguments.size())throw new PhonException(PhonException.Kind.SCHEMA,"generic arity mismatch");
        if(arguments.isEmpty())return kind;
        if(kind instanceof Schema.RecordKind){Schema.RecordKind value=(Schema.RecordKind)kind;List<Schema.Field>fields=new ArrayList<>();for(Schema.Field field:value.fields())fields.add(new Schema.Field(field.name(),sub(field.schema(),parameters,arguments),field.required()));return new Schema.RecordKind(value.name(),fields);}
        if(kind instanceof Schema.EnumKind){Schema.EnumKind value=(Schema.EnumKind)kind;List<Schema.Variant>variants=new ArrayList<>();for(Schema.Variant variant:value.variants())variants.add(new Schema.Variant(variant.name(),variant.index(),substitutePayload(variant.payload(),parameters,arguments)));return new Schema.EnumKind(value.name(),variants);}
        if(kind instanceof Schema.TupleKind){List<Schema.Ref>refs=new ArrayList<>();for(Schema.Ref item:((Schema.TupleKind)kind).elements())refs.add(sub(item,parameters,arguments));return new Schema.TupleKind(refs);}
        if(kind instanceof Schema.ListKind)return new Schema.ListKind(sub(((Schema.ListKind)kind).element(),parameters,arguments));
        if(kind instanceof Schema.SetKind)return new Schema.SetKind(sub(((Schema.SetKind)kind).element(),parameters,arguments));
        if(kind instanceof Schema.OptionKind)return new Schema.OptionKind(sub(((Schema.OptionKind)kind).element(),parameters,arguments));
        if(kind instanceof Schema.MapKind)return new Schema.MapKind(sub(((Schema.MapKind)kind).key(),parameters,arguments),sub(((Schema.MapKind)kind).value(),parameters,arguments));
        if(kind instanceof Schema.ArrayKind){Schema.ArrayKind value=(Schema.ArrayKind)kind;return new Schema.ArrayKind(sub(value.element(),parameters,arguments),value.dimensions());}
        if(kind instanceof Schema.TensorKind){Schema.TensorKind value=(Schema.TensorKind)kind;return new Schema.TensorKind(sub(value.element(),parameters,arguments),value.rank());}
        return kind;
    }
    private static Schema.Payload substitutePayload(Schema.Payload payload,List<String>parameters,List<Schema.Ref>arguments){
        if(payload instanceof Schema.UnitPayload)return payload;
        if(payload instanceof Schema.NewtypePayload)return Schema.Payload.newtype(sub(((Schema.NewtypePayload)payload).ref(),parameters,arguments));
        if(payload instanceof Schema.TuplePayload){List<Schema.Ref>refs=new ArrayList<>();for(Schema.Ref ref:((Schema.TuplePayload)payload).refs())refs.add(sub(ref,parameters,arguments));return Schema.Payload.tuple(refs);}
        List<Schema.Field>fields=new ArrayList<>();for(Schema.Field field:((Schema.RecordPayload)payload).fields())fields.add(new Schema.Field(field.name(),sub(field.schema(),parameters,arguments),field.required()));return Schema.Payload.record(fields);
    }
    private static Schema.Ref sub(Schema.Ref ref,List<String>parameters,List<Schema.Ref>arguments){if(ref.isVariable()){int index=parameters.indexOf(ref.variable());return index<0?ref:arguments.get(index);}List<Schema.Ref>nested=new ArrayList<>();for(Schema.Ref argument:ref.arguments())nested.add(sub(argument,parameters,arguments));return Schema.Ref.concrete(ref.id(),nested);}
    private static Map<String,Schema.Field>fields(List<Schema.Field>fs){Map<String,Schema.Field>m=new HashMap<>();for(Schema.Field f:fs)m.put(f.name(),f);return m;}
    private static Map<String,Schema.Variant>variants(List<Schema.Variant>vs){Map<String,Schema.Variant>m=new HashMap<>();for(Schema.Variant v:vs)m.put(v.name(),v);return m;}
}
