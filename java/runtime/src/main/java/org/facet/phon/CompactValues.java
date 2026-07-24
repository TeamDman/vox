package org.facet.phon;

import java.math.BigInteger;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;

/** Schema-driven language-neutral compact codec used by conformance and planning. */
final class CompactValues {
    private CompactValues() {}

    static Value decode(SchemaClosure closure, SchemaId id, PhonDecoder decoder, int depth) throws PhonException {
        return decodeRef(closure, Schema.Ref.concrete(id), decoder, depth);
    }
    static byte[] encode(SchemaClosure closure, Value value, PhonLimits limits) throws PhonException {
        PhonEncoder encoder=new PhonEncoder(limits);encodeRef(closure,Schema.Ref.concrete(closure.id()),value,encoder,0);return encoder.finish();
    }
    private static Value decodeRef(SchemaClosure c,Schema.Ref ref,PhonDecoder d,int depth)throws PhonException{
        if(depth>128)throw new PhonException(PhonException.Kind.LIMIT,"compact nesting exceeded");
        Schema schema=c.resolve(ref);Schema.Kind kind=substitute(schema.kind(),schema.typeParameters(),ref.arguments());
        return decodeKind(c,kind,d,depth+1);
    }
    private static Value decodeKind(SchemaClosure c,Schema.Kind k,PhonDecoder d,int depth)throws PhonException{
        if(k instanceof Schema.PrimitiveKind)return primitive(((Schema.PrimitiveKind)k).primitive(),d);
        if(k instanceof Schema.RecordKind){Map<String,Value>m=new LinkedHashMap<>();for(Schema.Field f:((Schema.RecordKind)k).fields())m.put(f.name(),decodeRef(c,f.schema(),d,depth));return Value.map(m);}
        if(k instanceof Schema.TupleKind){List<Value>v=new ArrayList<>();for(Schema.Ref r:((Schema.TupleKind)k).elements())v.add(decodeRef(c,r,d,depth));return Value.list(v);}
        if(k instanceof Schema.ListKind||k instanceof Schema.SetKind){Schema.Ref e=k instanceof Schema.ListKind?((Schema.ListKind)k).element():((Schema.SetKind)k).element();int n=d.readCount();List<Value>v=new ArrayList<>();Set<Value>seen=new LinkedHashSet<>();for(int x=0;x<n;x++){Value item=decodeRef(c,e,d,depth);if(k instanceof Schema.SetKind&&!seen.add(item))throw new PhonException(PhonException.Kind.MALFORMED,"duplicate set element",d.position(),null);v.add(item);}return Value.list(v);}
        if(k instanceof Schema.MapKind){Schema.MapKind x=(Schema.MapKind)k;int n=d.readCount();Map<String,Value>m=new LinkedHashMap<>();for(int q=0;q<n;q++){Value key=decodeRef(c,x.key(),d,depth);if(key.type()!=Value.Type.STRING)throw new PhonException(PhonException.Kind.DECODE,"only string map keys are supported",d.position(),null);Value old=m.put(key.asString(),decodeRef(c,x.value(),d,depth));if(old!=null)throw new PhonException(PhonException.Kind.MALFORMED,"duplicate map key",d.position(),null);}return Value.map(m);}
        if(k instanceof Schema.OptionKind)return d.readPresence()?decodeRef(c,((Schema.OptionKind)k).element(),d,depth):Value.nullValue();
        if(k instanceof Schema.ArrayKind){Schema.ArrayKind x=(Schema.ArrayKind)k;long n=1;for(long dim:x.dimensions()){try{n=Math.multiplyExact(n,dim);}catch(ArithmeticException e){throw new PhonException(PhonException.Kind.LIMIT,"array dimensions overflow");}}if(n>Integer.MAX_VALUE)throw new PhonException(PhonException.Kind.LIMIT,"array too large");List<Value>v=new ArrayList<>();for(int q=0;q<(int)n;q++)v.add(decodeRef(c,x.element(),d,depth));return Value.list(v);}
        if(k instanceof Schema.EnumKind){long index=d.readU32();for(Schema.Variant v:((Schema.EnumKind)k).variants())if(Integer.toUnsignedLong(v.index())==index)return Value.enumValue(v.name(),decodePayload(c,v.payload(),d,depth));throw new PhonException(PhonException.Kind.MALFORMED,"invalid enum discriminant "+index,d.position(),null);}
        if(k instanceof Schema.DynamicKind)return d.readDynamic();
        throw new PhonException(PhonException.Kind.DECODE,"unsupported compact kind");
    }
    private static Value decodePayload(SchemaClosure c,Schema.Payload p,PhonDecoder d,int depth)throws PhonException{
        if(p instanceof Schema.UnitPayload)return Value.nullValue();
        if(p instanceof Schema.NewtypePayload)return decodeRef(c,((Schema.NewtypePayload)p).ref(),d,depth);
        if(p instanceof Schema.TuplePayload){List<Value>v=new ArrayList<>();for(Schema.Ref r:((Schema.TuplePayload)p).refs())v.add(decodeRef(c,r,d,depth));return Value.list(v);}
        Map<String,Value>m=new LinkedHashMap<>();for(Schema.Field f:((Schema.RecordPayload)p).fields())m.put(f.name(),decodeRef(c,f.schema(),d,depth));return Value.map(m);
    }
    private static Value primitive(Schema.Primitive p,PhonDecoder d)throws PhonException{
        switch(p){
            case BOOL:return Value.bool(d.readBool());case U8:return Value.unsigned(d.readU8());case U16:return Value.unsigned(d.readU16());case U32:return Value.unsigned(d.readU32());
            case U64:return Value.unsigned(d.readU64());case U128:return Value.unsigned(d.readU128());case I8:return Value.signed(d.readI8());case I16:return Value.signed(d.readI16());
            case I32:return Value.signed(d.readI32());case I64:return Value.signed(d.readI64());case I128:return Value.signed(d.readI128());case F32:return Value.floating(d.readF32());case F64:return Value.floating(d.readF64());
            case CHAR:return Value.character(d.readChar());case STRING:case DATETIME:case UUID:case QNAME:return Value.string(d.readString());case BYTES:return Value.bytes(d.readBytes());case UNIT:return Value.nullValue();
            default:throw new PhonException(PhonException.Kind.DECODE,"never is uninhabited");
        }
    }
    private static void encodeRef(SchemaClosure c,Schema.Ref ref,Value v,PhonEncoder e,int depth)throws PhonException{
        if(depth>128)throw new PhonException(PhonException.Kind.LIMIT,"compact nesting exceeded");Schema s=c.resolve(ref);encodeKind(c,substitute(s.kind(),s.typeParameters(),ref.arguments()),v,e,depth+1);
    }
    private static void encodeKind(SchemaClosure c,Schema.Kind k,Value v,PhonEncoder e,int depth)throws PhonException{
        if(k instanceof Schema.PrimitiveKind){encodePrimitive(((Schema.PrimitiveKind)k).primitive(),v,e);return;}
        if(k instanceof Schema.RecordKind){require(v,Value.Type.MAP);for(Schema.Field f:((Schema.RecordKind)k).fields()){Value x=v.asMap().get(f.name());if(x==null)throw new PhonException(PhonException.Kind.ENCODE,"missing field "+f.name());encodeRef(c,f.schema(),x,e,depth);}return;}
        if(k instanceof Schema.TupleKind){require(v,Value.Type.LIST);List<Schema.Ref>rs=((Schema.TupleKind)k).elements();if(v.asList().size()!=rs.size())throw new PhonException(PhonException.Kind.ENCODE,"tuple arity");for(int q=0;q<rs.size();q++)encodeRef(c,rs.get(q),v.asList().get(q),e,depth);return;}
        if(k instanceof Schema.ListKind||k instanceof Schema.SetKind){require(v,Value.Type.LIST);Schema.Ref r=k instanceof Schema.ListKind?((Schema.ListKind)k).element():((Schema.SetKind)k).element();e.writeCount(v.asList().size());for(Value x:v.asList())encodeRef(c,r,x,e,depth);return;}
        if(k instanceof Schema.MapKind){require(v,Value.Type.MAP);Schema.MapKind x=(Schema.MapKind)k;e.writeCount(v.asMap().size());for(Map.Entry<String,Value>q:v.asMap().entrySet()){encodeRef(c,x.key(),Value.string(q.getKey()),e,depth);encodeRef(c,x.value(),q.getValue(),e,depth);}return;}
        if(k instanceof Schema.OptionKind){boolean present=v.type()!=Value.Type.NULL;e.writePresence(present);if(present)encodeRef(c,((Schema.OptionKind)k).element(),v,e,depth);return;}
        if(k instanceof Schema.EnumKind){require(v,Value.Type.ENUM);for(Schema.Variant x:((Schema.EnumKind)k).variants())if(x.name().equals(v.asEnum().variant())){e.writeU32(Integer.toUnsignedLong(x.index()));encodePayload(c,x.payload(),v.asEnum().payload(),e,depth);return;}throw new PhonException(PhonException.Kind.ENCODE,"unknown enum variant");}
        if(k instanceof Schema.DynamicKind){e.writeDynamic(v);return;}
        throw new PhonException(PhonException.Kind.ENCODE,"unsupported compact kind");
    }
    private static void encodePayload(SchemaClosure c,Schema.Payload p,Value v,PhonEncoder e,int depth)throws PhonException{
        if(p instanceof Schema.UnitPayload)return;if(p instanceof Schema.NewtypePayload){encodeRef(c,((Schema.NewtypePayload)p).ref(),v,e,depth);return;}
        if(p instanceof Schema.TuplePayload){require(v,Value.Type.LIST);List<Schema.Ref>rs=((Schema.TuplePayload)p).refs();for(int q=0;q<rs.size();q++)encodeRef(c,rs.get(q),v.asList().get(q),e,depth);return;}
        require(v,Value.Type.MAP);for(Schema.Field f:((Schema.RecordPayload)p).fields())encodeRef(c,f.schema(),v.asMap().get(f.name()),e,depth);
    }
    private static void encodePrimitive(Schema.Primitive p,Value v,PhonEncoder e)throws PhonException{
        switch(p){case BOOL:require(v,Value.Type.BOOL);e.writeBool(v.asBool());break;case U8:e.writeU8(v.asInteger().intValueExact());break;case U16:e.writeU16(v.asInteger().intValueExact());break;case U32:e.writeU32(v.asInteger().longValueExact());break;case U64:e.writeU64(v.asInteger());break;case U128:e.writeU128(v.asInteger());break;case I8:e.writeI8(v.asInteger().byteValueExact());break;case I16:e.writeI16(v.asInteger().shortValueExact());break;case I32:e.writeI32(v.asInteger().intValueExact());break;case I64:e.writeI64(v.asInteger().longValueExact());break;case I128:e.writeI128(v.asInteger());break;case F32:e.writeF32((float)v.asDouble());break;case F64:e.writeF64(v.asDouble());break;case CHAR:e.writeChar(v.asCodePoint());break;case STRING:case DATETIME:case UUID:case QNAME:e.writeString(v.asString());break;case BYTES:e.writeBytes(v.asBytes());break;case UNIT:require(v,Value.Type.NULL);break;default:throw new PhonException(PhonException.Kind.ENCODE,"never is uninhabited");}
    }
    private static void require(Value v,Value.Type t)throws PhonException{if(v==null||v.type()!=t)throw new PhonException(PhonException.Kind.ENCODE,"expected "+t);}

    private static Schema.Kind substitute(Schema.Kind k,List<String>params,List<Schema.Ref>args)throws PhonException{
        if(params.size()!=args.size())throw new PhonException(PhonException.Kind.SCHEMA,"generic arity mismatch");if(args.isEmpty())return k;
        if(k instanceof Schema.RecordKind){Schema.RecordKind x=(Schema.RecordKind)k;List<Schema.Field>fs=new ArrayList<>();for(Schema.Field f:x.fields())fs.add(new Schema.Field(f.name(),sub(f.schema(),params,args),f.required()));return new Schema.RecordKind(x.name(),fs);}
        if(k instanceof Schema.TupleKind){List<Schema.Ref>rs=new ArrayList<>();for(Schema.Ref r:((Schema.TupleKind)k).elements())rs.add(sub(r,params,args));return new Schema.TupleKind(rs);}
        if(k instanceof Schema.ListKind)return new Schema.ListKind(sub(((Schema.ListKind)k).element(),params,args));
        if(k instanceof Schema.SetKind)return new Schema.SetKind(sub(((Schema.SetKind)k).element(),params,args));
        if(k instanceof Schema.OptionKind)return new Schema.OptionKind(sub(((Schema.OptionKind)k).element(),params,args));
        if(k instanceof Schema.MapKind)return new Schema.MapKind(sub(((Schema.MapKind)k).key(),params,args),sub(((Schema.MapKind)k).value(),params,args));
        return k;
    }
    private static Schema.Ref sub(Schema.Ref r,List<String>p,List<Schema.Ref>a){if(r.isVariable()){int i=p.indexOf(r.variable());return i<0?r:a.get(i);}List<Schema.Ref>as=new ArrayList<>();for(Schema.Ref x:r.arguments())as.add(sub(x,p,a));return Schema.Ref.concrete(r.id(),as);}
}
