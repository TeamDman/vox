package org.facet.phon;

import java.io.ByteArrayOutputStream;
import java.nio.ByteBuffer;
import java.nio.charset.CharacterCodingException;
import java.nio.charset.CodingErrorAction;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;

/** Rust-compatible self-describing schema wire form. */
final class SchemaWire {
    private static final int UNIT=0x00,BOOL=0x01,U32=0x04,U64=0x05,STRING=0x0f,LIST=0x11,STRUCT=0x16,ENUM=0x17,NONE=0x18,SOME=0x19;
    private SchemaWire() {}

    static byte[] encode(Schema schema) throws PhonException {
        Out out=new Out();schema(out,schema);return out.bytes();
    }
    static Schema decode(byte[] bytes,PhonLimits limits)throws PhonException{
        if(bytes.length>limits.schemaBytes())throw new PhonException(PhonException.Kind.LIMIT,"schema bytes exceed schemaBytes");
        In in=new In(bytes,limits);Schema schema=schema(in,0);if(in.remaining()!=0)throw in.error("trailing schema bytes");return schema;
    }
    private static void schema(Out o,Schema s)throws PhonException{
        o.struct("Schema",3);o.name("id");o.tag(U64);o.u64(s.id().asLong());o.name("type_params");o.list(s.typeParameters().size());
        for(String p:s.typeParameters())o.valueString(p);o.name("kind");kind(o,s.kind());
    }
    private static void kind(Out o,Schema.Kind k)throws PhonException{
        o.tag(ENUM);
        if(k instanceof Schema.PrimitiveKind){o.string("Primitive");primitive(o,((Schema.PrimitiveKind)k).primitive());}
        else if(k instanceof Schema.RecordKind){Schema.RecordKind x=(Schema.RecordKind)k;o.string("Struct");o.struct("Struct",2);o.name("name");o.valueString(x.name());o.name("fields");fields(o,x.fields());}
        else if(k instanceof Schema.EnumKind){Schema.EnumKind x=(Schema.EnumKind)k;o.string("Enum");o.struct("Enum",2);o.name("name");o.valueString(x.name());o.name("variants");o.list(x.variants().size());for(Schema.Variant v:x.variants())variant(o,v);}
        else if(k instanceof Schema.TupleKind){o.string("Tuple");o.struct("Tuple",1);o.name("elements");refs(o,((Schema.TupleKind)k).elements());}
        else if(k instanceof Schema.ListKind){o.string("List");singleRef(o,"List","element",((Schema.ListKind)k).element());}
        else if(k instanceof Schema.SetKind){o.string("Set");singleRef(o,"Set","element",((Schema.SetKind)k).element());}
        else if(k instanceof Schema.OptionKind){o.string("Option");singleRef(o,"Option","element",((Schema.OptionKind)k).element());}
        else if(k instanceof Schema.MapKind){Schema.MapKind x=(Schema.MapKind)k;o.string("Map");o.struct("Map",2);o.name("key");ref(o,x.key());o.name("value");ref(o,x.value());}
        else if(k instanceof Schema.ArrayKind){Schema.ArrayKind x=(Schema.ArrayKind)k;o.string("Array");o.struct("Array",2);o.name("element");ref(o,x.element());o.name("dimensions");o.list(x.dimensions().size());for(long d:x.dimensions()){o.tag(U64);o.u64(d);}}
        else if(k instanceof Schema.TensorKind){Schema.TensorKind x=(Schema.TensorKind)k;o.string("Tensor");o.struct("Tensor",2);o.name("element");ref(o,x.element());o.name("rank");if(x.rank()==null)o.tag(NONE);else{o.tag(SOME);o.tag(U32);o.u32(x.rank());}}
        else if(k instanceof Schema.DynamicKind){o.string("Dynamic");o.tag(UNIT);}
        else throw new PhonException(PhonException.Kind.SCHEMA,"cannot encode unsupported schema kind");
    }
    private static void singleRef(Out o,String type,String name,Schema.Ref r)throws PhonException{o.struct(type,1);o.name(name);ref(o,r);}
    private static void primitive(Out o,Schema.Primitive p){o.tag(ENUM);o.string(p.tag());o.tag(UNIT);}
    private static void refs(Out o,List<Schema.Ref>rs)throws PhonException{o.list(rs.size());for(Schema.Ref r:rs)ref(o,r);}
    private static void fields(Out o,List<Schema.Field>fs)throws PhonException{o.list(fs.size());for(Schema.Field f:fs)field(o,f);}
    private static void ref(Out o,Schema.Ref r)throws PhonException{
        o.tag(ENUM);if(r.isVariable()){o.string("Var");o.struct("Var",1);o.name("name");o.valueString(r.variable());}
        else{o.string("Concrete");o.struct("Concrete",2);o.name("id");o.tag(U64);o.u64(r.id().asLong());o.name("args");refs(o,r.arguments());}
    }
    private static void field(Out o,Schema.Field f)throws PhonException{o.struct("Field",3);o.name("name");o.valueString(f.name());o.name("schema");ref(o,f.schema());o.name("required");o.tag(BOOL);o.tag(f.required()?1:0);}
    private static void variant(Out o,Schema.Variant v)throws PhonException{
        o.struct("Variant",3);o.name("name");o.valueString(v.name());o.name("index");o.tag(U32);o.u32(v.index());o.name("payload");o.tag(ENUM);Schema.Payload p=v.payload();
        if(p instanceof Schema.UnitPayload){o.string("Unit");o.tag(UNIT);}
        else if(p instanceof Schema.NewtypePayload){o.string("Newtype");ref(o,((Schema.NewtypePayload)p).ref());}
        else if(p instanceof Schema.TuplePayload){o.string("Tuple");refs(o,((Schema.TuplePayload)p).refs());}
        else{o.string("Struct");fields(o,((Schema.RecordPayload)p).fields());}
    }

    private static Schema schema(In i,int depth)throws PhonException{
        i.depth(depth);i.struct(3);i.name();SchemaId id=SchemaId.fromLong(i.valueU64());i.name();int n=i.list();
        List<String>params=new ArrayList<>();for(int x=0;x<n;x++)params.add(i.valueString());i.name();return new Schema(id,params,kind(i,depth+1));
    }
    private static Schema.Kind kind(In i,int depth)throws PhonException{
        i.depth(depth);i.expect(ENUM);String variant=i.string();
        switch(variant){
            case"Primitive":return new Schema.PrimitiveKind(primitive(i));
            case"Struct":{i.struct(2);i.name();String name=i.valueString();i.name();return new Schema.RecordKind(name,fields(i,depth+1));}
            case"Enum":{i.struct(2);i.name();String name=i.valueString();i.name();int n=i.list();List<Schema.Variant>vs=new ArrayList<>();for(int x=0;x<n;x++)vs.add(variant(i,depth+1));return new Schema.EnumKind(name,vs);}
            case"Tuple":i.struct(1);i.name();return new Schema.TupleKind(refs(i,depth+1));
            case"List":return new Schema.ListKind(singleRef(i,depth));
            case"Set":return new Schema.SetKind(singleRef(i,depth));
            case"Option":return new Schema.OptionKind(singleRef(i,depth));
            case"Map":{i.struct(2);i.name();Schema.Ref key=ref(i,depth+1);i.name();return new Schema.MapKind(key,ref(i,depth+1));}
            case"Array":{i.struct(2);i.name();Schema.Ref e=ref(i,depth+1);i.name();int n=i.list();List<Long>ds=new ArrayList<>();for(int x=0;x<n;x++)ds.add(i.valueU64());return new Schema.ArrayKind(e,ds);}
            case"Tensor":{i.struct(2);i.name();Schema.Ref e=ref(i,depth+1);i.name();int tag=i.tag();Integer rank=tag==NONE?null:tag==SOME?(int)i.valueU32():null;if(tag!=NONE&&tag!=SOME)throw i.error("expected option");return new Schema.TensorKind(e,rank);}
            case"Dynamic":i.expect(UNIT);return Schema.DynamicKind.INSTANCE;
            default:throw i.error("unsupported schema kind "+variant);
        }
    }
    private static Schema.Ref singleRef(In i,int depth)throws PhonException{i.struct(1);i.name();return ref(i,depth+1);}
    private static Schema.Primitive primitive(In i)throws PhonException{i.expect(ENUM);String tag=i.string();i.expect(UNIT);return Schema.Primitive.fromTag(tag);}
    private static List<Schema.Ref>refs(In i,int depth)throws PhonException{int n=i.list();List<Schema.Ref>rs=new ArrayList<>();for(int x=0;x<n;x++)rs.add(ref(i,depth+1));return rs;}
    private static List<Schema.Field>fields(In i,int depth)throws PhonException{int n=i.list();List<Schema.Field>fs=new ArrayList<>();for(int x=0;x<n;x++)fs.add(field(i,depth+1));return fs;}
    private static Schema.Ref ref(In i,int depth)throws PhonException{
        i.depth(depth);i.expect(ENUM);String v=i.string();if(v.equals("Var")){i.struct(1);i.name();return Schema.Ref.variable(i.valueString());}
        if(!v.equals("Concrete"))throw i.error("unknown ref "+v);i.struct(2);i.name();SchemaId id=SchemaId.fromLong(i.valueU64());i.name();return Schema.Ref.concrete(id,refs(i,depth+1));
    }
    private static Schema.Field field(In i,int depth)throws PhonException{i.depth(depth);i.struct(3);i.name();String n=i.valueString();i.name();Schema.Ref r=ref(i,depth+1);i.name();return new Schema.Field(n,r,i.valueBool());}
    private static Schema.Variant variant(In i,int depth)throws PhonException{
        i.depth(depth);i.struct(3);i.name();String name=i.valueString();i.name();int index=(int)i.valueU32();i.name();i.expect(ENUM);String p=i.string();Schema.Payload payload;
        switch(p){case"Unit":i.expect(UNIT);payload=Schema.Payload.unit();break;case"Newtype":payload=Schema.Payload.newtype(ref(i,depth+1));break;case"Tuple":payload=Schema.Payload.tuple(refs(i,depth+1));break;case"Struct":payload=Schema.Payload.record(fields(i,depth+1));break;default:throw i.error("unknown payload "+p);}
        return new Schema.Variant(name,index,payload);
    }

    private static final class Out{
        final ByteArrayOutputStream o=new ByteArrayOutputStream();void tag(int b){o.write(b);}void u32(long n){for(int x=0;x<4;x++)tag((int)(n>>>(8*x)));}void u64(long n){for(int x=0;x<8;x++)tag((int)(n>>>(8*x)));}
        void string(String s){byte[]b=s.getBytes(StandardCharsets.UTF_8);u32(b.length);o.write(b,0,b.length);}void valueString(String s){tag(STRING);string(s);}void name(String s){string(s);}
        void struct(String n,int fields){tag(STRUCT);string(n);u32(fields);}void list(int n){tag(LIST);u32(n);}byte[]bytes(){return o.toByteArray();}
    }
    private static final class In{
        final byte[]b;final PhonLimits l;int p;In(byte[]b,PhonLimits l){this.b=b;this.l=l;}int remaining(){return b.length-p;}int tag()throws PhonException{need(1);return b[p++]&255;}
        void expect(int t)throws PhonException{int g=tag();if(g!=t)throw error("expected tag "+t+", got "+g);}void need(int n)throws PhonException{if(n<0||remaining()<n)throw new PhonException(PhonException.Kind.TRUNCATED,"truncated schema",p,null);}
        long u32()throws PhonException{need(4);long n=0;for(int x=0;x<4;x++)n|=(long)tag()<<(8*x);return n;}long u64()throws PhonException{need(8);long n=0;for(int x=0;x<8;x++)n|=(long)tag()<<(8*x);return n;}
        int count()throws PhonException{long n=u32();if(n>l.collectionEntries()||n>remaining())throw error("unbounded count "+n);return(int)n;}String string()throws PhonException{int n=count();if(n>l.byteRunLength())throw error("string too long");need(n);try{String s=StandardCharsets.UTF_8.newDecoder().onMalformedInput(CodingErrorAction.REPORT).decode(ByteBuffer.wrap(b,p,n)).toString();p+=n;return s;}catch(CharacterCodingException e){throw error("invalid UTF-8");}}
        void struct(int n)throws PhonException{expect(STRUCT);string();if(u32()!=n)throw error("wrong struct field count");}void name()throws PhonException{string();}int list()throws PhonException{expect(LIST);return count();}
        String valueString()throws PhonException{expect(STRING);return string();}long valueU32()throws PhonException{expect(U32);return u32();}long valueU64()throws PhonException{expect(U64);return u64();}boolean valueBool()throws PhonException{expect(BOOL);int x=tag();if(x>1)throw error("invalid bool");return x==1;}
        void depth(int d)throws PhonException{if(d>l.nestingDepth())throw new PhonException(PhonException.Kind.LIMIT,"schema nesting exceeds nestingDepth",p,null);}PhonException error(String m){return new PhonException(PhonException.Kind.MALFORMED,m,p,null);}
    }
}
