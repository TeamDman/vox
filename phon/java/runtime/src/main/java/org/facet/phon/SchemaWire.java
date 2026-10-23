package org.facet.phon;

import java.io.ByteArrayOutputStream;
import java.nio.ByteBuffer;
import java.nio.CharBuffer;
import java.nio.charset.CharacterCodingException;
import java.nio.charset.CodingErrorAction;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.HashSet;

/** Rust-compatible self-describing schema wire form. */
final class SchemaWire {
    static final int UNIT=0x00,BOOL=0x01,U32=0x04,U64=0x05,STRING=0x0f,LIST=0x11,STRUCT=0x16,ENUM=0x17,NONE=0x18,SOME=0x19;
    private SchemaWire() {}

    static byte[] encode(Schema schema) throws PhonException {
        Out out=new Out();schema(out,schema);return out.bytes();
    }
    static Schema decode(byte[] bytes,PhonLimits limits)throws PhonException{
        if(bytes.length>limits.schemaBytes())throw new PhonException(PhonException.Kind.LIMIT,"schema bytes exceed schemaBytes");
        In in=new In(bytes,limits);Schema schema=schema(in,0);if(in.remaining()!=0)throw in.error("trailing schema bytes");return schema;
    }
    static void schema(Out o,Schema s)throws PhonException{
        o.struct("Schema",3);o.name("id");o.tag(U64);o.u64(s.id().asLong());o.name("type_params");o.list(s.typeParameters().size());
        for(String p:s.typeParameters())o.symbol(p);o.name("kind");kind(o,s.kind());
    }
    private static void kind(Out o,Schema.Kind k)throws PhonException{
        o.tag(ENUM);
        if(k instanceof Schema.PrimitiveKind){o.string("Primitive");primitive(o,((Schema.PrimitiveKind)k).primitive());}
        else if(k instanceof Schema.RecordKind){Schema.RecordKind x=(Schema.RecordKind)k;o.string("Struct");o.struct("Struct",2);o.name("name");o.symbol(x.name());o.name("fields");fields(o,x.fields());}
        else if(k instanceof Schema.EnumKind){Schema.EnumKind x=(Schema.EnumKind)k;o.string("Enum");o.struct("Enum",2);o.name("name");o.symbol(x.name());o.name("variants");o.list(x.variants().size());for(Schema.Variant v:x.variants())variant(o,v);}
        else if(k instanceof Schema.TupleKind){o.string("Tuple");o.struct("Tuple",1);o.name("elements");refs(o,((Schema.TupleKind)k).elements());}
        else if(k instanceof Schema.ListKind){o.string("List");singleRef(o,"List","element",((Schema.ListKind)k).element());}
        else if(k instanceof Schema.SetKind){o.string("Set");singleRef(o,"Set","element",((Schema.SetKind)k).element());}
        else if(k instanceof Schema.OptionKind){o.string("Option");singleRef(o,"Option","element",((Schema.OptionKind)k).element());}
        else if(k instanceof Schema.MapKind){Schema.MapKind x=(Schema.MapKind)k;o.string("Map");o.struct("Map",2);o.name("key");ref(o,x.key());o.name("value");ref(o,x.value());}
        else if(k instanceof Schema.ArrayKind){Schema.ArrayKind x=(Schema.ArrayKind)k;o.string("Array");o.struct("Array",2);o.name("element");ref(o,x.element());o.name("dimensions");o.list(x.dimensions().size());for(long d:x.dimensions()){o.tag(U64);o.u64(d);}}
        else if(k instanceof Schema.TensorKind){Schema.TensorKind x=(Schema.TensorKind)k;o.string("Tensor");o.struct("Tensor",2);o.name("element");ref(o,x.element());o.name("rank");if(x.rank()==null)o.tag(NONE);else{o.tag(SOME);o.tag(U32);o.u32(Integer.toUnsignedLong(x.rank()));}}
        else if(k instanceof Schema.ChannelKind){Schema.ChannelKind x=(Schema.ChannelKind)k;o.string("Channel");o.struct("Channel",2);o.name("direction");o.tag(ENUM);o.string(x.direction()==Schema.ChannelDirection.TX?"tx":"rx");o.tag(UNIT);o.name("element");ref(o,x.element());}
        else if(k instanceof Schema.ExternalKind){Schema.ExternalKind x=(Schema.ExternalKind)k;o.string("External");o.struct("External",2);o.name("kind");o.symbol(x.name());o.name("metadata");if(x.metadata()==null)o.tag(NONE);else{o.tag(SOME);ref(o,x.metadata());}}
        else if(k instanceof Schema.SemanticKind){Schema.SemanticKind x=(Schema.SemanticKind)k;o.string("Semantic");o.struct("Semantic",3);o.name("name");o.symbol(x.name().value());o.name("args");refs(o,x.arguments());o.name("representation");ref(o,x.representation());}
        else if(k instanceof Schema.DynamicKind){o.string("Dynamic");o.tag(UNIT);}
        else throw new PhonException(PhonException.Kind.SCHEMA,"cannot encode unsupported schema kind");
    }
    private static void singleRef(Out o,String type,String name,Schema.Ref r)throws PhonException{o.struct(type,1);o.name(name);ref(o,r);}
    private static void primitive(Out o,Schema.Primitive p)throws PhonException{o.tag(ENUM);o.string(p.tag());o.tag(UNIT);}
    private static void refs(Out o,List<Schema.Ref>rs)throws PhonException{o.list(rs.size());for(Schema.Ref r:rs)ref(o,r);}
    private static void fields(Out o,List<Schema.Field>fs)throws PhonException{o.list(fs.size());for(Schema.Field f:fs)field(o,f);}
    private static void ref(Out o,Schema.Ref r)throws PhonException{
        o.tag(ENUM);if(r.isVariable()){o.string("Var");o.struct("Var",1);o.name("name");o.symbol(r.variable());}
        else{o.string("Concrete");o.struct("Concrete",2);o.name("id");o.tag(U64);o.u64(r.id().asLong());o.name("args");refs(o,r.arguments());}
    }
    private static void field(Out o,Schema.Field f)throws PhonException{o.struct("Field",3);o.name("name");o.symbol(f.name());o.name("schema");ref(o,f.schema());o.name("required");o.tag(BOOL);o.tag(f.required()?1:0);}
    private static void variant(Out o,Schema.Variant v)throws PhonException{
        o.struct("Variant",3);o.name("name");o.symbol(v.name());o.name("index");o.tag(U32);o.u32(Integer.toUnsignedLong(v.index()));o.name("payload");o.tag(ENUM);Schema.Payload p=v.payload();
        if(p instanceof Schema.UnitPayload){o.string("Unit");o.tag(UNIT);}
        else if(p instanceof Schema.NewtypePayload){o.string("Newtype");ref(o,((Schema.NewtypePayload)p).ref());}
        else if(p instanceof Schema.TuplePayload){o.string("Tuple");refs(o,((Schema.TuplePayload)p).refs());}
        else{o.string("Struct");fields(o,((Schema.RecordPayload)p).fields());}
    }

    static Schema schema(In i,int depth)throws PhonException{
        i.depth(depth);i.struct("Schema",3);i.field("id");SchemaId id=SchemaId.fromLong(i.valueU64());i.field("type_params");int n=i.list();
        List<String>params=new ArrayList<>();for(int x=0;x<n;x++)params.add(i.symbol());i.field("kind");return new Schema(id,params,kind(i,depth+1));
    }
    private static Schema.Kind kind(In i,int depth)throws PhonException{
        i.depth(depth);i.expect(ENUM);String variant=i.string();
        switch(variant){
            case"Primitive":return new Schema.PrimitiveKind(primitive(i));
            case"Struct":{i.struct("Struct",2);i.field("name");String name=i.symbol();i.field("fields");return new Schema.RecordKind(name,fields(i,depth+1));}
            case"Enum":{i.struct("Enum",2);i.field("name");String name=i.symbol();i.field("variants");int n=i.list();List<Schema.Variant>vs=new ArrayList<>();for(int x=0;x<n;x++)vs.add(variant(i,depth+1));return new Schema.EnumKind(name,vs);}
            case"Tuple":i.struct("Tuple",1);i.field("elements");return new Schema.TupleKind(refs(i,depth+1));
            case"List":return new Schema.ListKind(singleRef(i,"List",depth));
            case"Set":return new Schema.SetKind(singleRef(i,"Set",depth));
            case"Option":return new Schema.OptionKind(singleRef(i,"Option",depth));
            case"Map":{i.struct("Map",2);i.field("key");Schema.Ref key=ref(i,depth+1);i.field("value");return new Schema.MapKind(key,ref(i,depth+1));}
            case"Array":{i.struct("Array",2);i.field("element");Schema.Ref e=ref(i,depth+1);i.field("dimensions");int n=i.list();List<Long>ds=new ArrayList<>();for(int x=0;x<n;x++)ds.add(i.valueU64());return new Schema.ArrayKind(e,ds);}
            case"Tensor":{i.struct("Tensor",2);i.field("element");Schema.Ref e=ref(i,depth+1);i.field("rank");int tag=i.tag();Integer rank=tag==NONE?null:tag==SOME?(int)i.valueU32():null;if(tag!=NONE&&tag!=SOME)throw i.error("expected option");return new Schema.TensorKind(e,rank);}
            case"Channel":{i.struct("Channel",2);i.field("direction");i.expect(ENUM);String direction=i.string();i.expect(UNIT);if(!direction.equals("tx")&&!direction.equals("rx"))throw i.error("invalid channel direction");i.field("element");return new Schema.ChannelKind(direction.equals("tx")?Schema.ChannelDirection.TX:Schema.ChannelDirection.RX,ref(i,depth+1));}
            case"External":{i.struct("External",2);i.field("kind");String name=i.symbol();i.field("metadata");int tag=i.tag();if(tag!=NONE&&tag!=SOME)throw i.error("expected option");return new Schema.ExternalKind(name,tag==NONE?null:ref(i,depth+1));}
            case"Semantic":{i.struct("Semantic",3);i.field("name");String name=i.symbol();i.field("args");List<Schema.Ref>args=refs(i,depth+1);i.field("representation");Schema.Ref representation=ref(i,depth+1);try{return new Schema.SemanticKind(new QualifiedName(name),args,representation);}catch(IllegalArgumentException failure){throw i.error("invalid qualified name");}}
            case"Dynamic":i.expect(UNIT);return Schema.DynamicKind.INSTANCE;
            default:throw i.error("unsupported schema kind "+variant);
        }
    }
    private static Schema.Ref singleRef(In i,String type,int depth)throws PhonException{i.struct(type,1);i.field("element");return ref(i,depth+1);}
    private static Schema.Primitive primitive(In i)throws PhonException{i.expect(ENUM);String tag=i.string();i.expect(UNIT);return Schema.Primitive.fromTag(tag);}
    private static List<Schema.Ref>refs(In i,int depth)throws PhonException{int n=i.list();List<Schema.Ref>rs=new ArrayList<>();for(int x=0;x<n;x++)rs.add(ref(i,depth+1));return rs;}
    private static List<Schema.Field>fields(In i,int depth)throws PhonException{int n=i.list();List<Schema.Field>fs=new ArrayList<>();for(int x=0;x<n;x++)fs.add(field(i,depth+1));return fs;}
    private static Schema.Ref ref(In i,int depth)throws PhonException{
        i.depth(depth);i.expect(ENUM);String v=i.string();if(v.equals("Var")){i.struct("Var",1);i.field("name");return Schema.Ref.variable(i.symbol());}
        if(!v.equals("Concrete"))throw i.error("unknown ref "+v);i.struct("Concrete",2);i.field("id");SchemaId id=SchemaId.fromLong(i.valueU64());i.field("args");return Schema.Ref.concrete(id,refs(i,depth+1));
    }
    private static Schema.Field field(In i,int depth)throws PhonException{i.depth(depth);i.struct("Field",3);i.field("name");String n=i.symbol();i.field("schema");Schema.Ref r=ref(i,depth+1);i.field("required");return new Schema.Field(n,r,i.valueBool());}
    private static Schema.Variant variant(In i,int depth)throws PhonException{
        i.depth(depth);i.struct("Variant",3);i.field("name");String name=i.symbol();i.field("index");int index=(int)i.valueU32();i.field("payload");i.expect(ENUM);String p=i.string();Schema.Payload payload;
        switch(p){case"Unit":i.expect(UNIT);payload=Schema.Payload.unit();break;case"Newtype":payload=Schema.Payload.newtype(ref(i,depth+1));break;case"Tuple":payload=Schema.Payload.tuple(refs(i,depth+1));break;case"Struct":payload=Schema.Payload.record(fields(i,depth+1));break;default:throw i.error("unknown payload "+p);}
        return new Schema.Variant(name,index,payload);
    }

    static byte[] utf8(String value)throws PhonException{
        try{ByteBuffer b=StandardCharsets.UTF_8.newEncoder().onMalformedInput(CodingErrorAction.REPORT)
                .onUnmappableCharacter(CodingErrorAction.REPORT).encode(CharBuffer.wrap(value));
            byte[]bytes=new byte[b.remaining()];b.get(bytes);return bytes;
        }catch(CharacterCodingException failure){throw new PhonException(PhonException.Kind.ENCODE,"invalid Unicode string",failure);}
    }
    static PhonException limit(String message){return new PhonException(PhonException.Kind.LIMIT,message);}
    static final class Out{
        final ByteArrayOutputStream output=new ByteArrayOutputStream();final int maximum;final Map<String,Integer> symbols;
        Out(){this(PhonLimits.DEFAULT.schemaBytes(),null);}
        Out(int maximum,Map<String,Integer>symbols){this.maximum=maximum;this.symbols=symbols;}
        void reserve(int n)throws PhonException{if(n<0||n>maximum-output.size())throw limit("schema output byte limit");}
        void tag(int b)throws PhonException{reserve(1);output.write(b);}
        void raw(byte[]bytes)throws PhonException{reserve(bytes.length);output.write(bytes,0,bytes.length);}
        void u32(long n)throws PhonException{for(int x=0;x<4;x++)tag((int)(n>>>(8*x)));}
        void u64(long n)throws PhonException{for(int x=0;x<8;x++)tag((int)(n>>>(8*x)));}
        void string(String s)throws PhonException{byte[]b=utf8(s);reserve(4+b.length);u32(b.length);raw(b);}
        void valueString(String s)throws PhonException{tag(STRING);string(s);}
        void valueU32(long n)throws PhonException{tag(U32);u32(n);}
        void valueU64(long n)throws PhonException{tag(U64);u64(n);}
        void name(String s)throws PhonException{string(s);}
        void symbol(String s)throws PhonException{
            if(symbols==null){valueString(s);return;}
            Integer index=symbols.get(s);if(index==null)throw new PhonException(PhonException.Kind.SCHEMA,"missing interned string");
            valueU32(index);
        }
        void struct(String n,int fields)throws PhonException{tag(STRUCT);string(n);u32(fields);}
        void list(int n)throws PhonException{tag(LIST);u32(n);}
        byte[]bytes(){return output.toByteArray();}
    }
    static final class In{
        final byte[]bytes;final PhonLimits limits;final long maximumOwned;long owned;int position;
        List<String>symbols;final Set<Integer>usedSymbols=new HashSet<>();
        In(byte[]bytes,PhonLimits limits){this(bytes,limits,limits.schemaBytes());}
        In(byte[]bytes,PhonLimits limits,long maximumOwned){this.bytes=bytes;this.limits=limits;this.maximumOwned=maximumOwned;}
        int remaining(){return bytes.length-position;}
        int tag()throws PhonException{need(1);return bytes[position++]&255;}
        void expect(int expected)throws PhonException{int got=tag();if(got!=expected)throw error("expected tag "+expected+", got "+got);}
        void need(int n)throws PhonException{if(n<0||remaining()<n)throw new PhonException(PhonException.Kind.TRUNCATED,"truncated schema",position,null);}
        long u32()throws PhonException{need(4);long n=0;for(int x=0;x<4;x++)n|=(long)tag()<<(8*x);return n;}
        long u64()throws PhonException{need(8);long n=0;for(int x=0;x<8;x++)n|=(long)tag()<<(8*x);return n;}
        int count()throws PhonException{long n=u32();if(n>limits.collectionEntries()||n>remaining())throw error("unbounded count "+n);return(int)n;}
        String readString(boolean retained)throws PhonException{
            long length=u32();if(length>limits.byteRunLength()||length>remaining())throw error("unbounded byte run "+length);
            if(!retained&&length>64)throw error("invalid grammar token length");
            if(retained){if(length>maximumOwned-owned)throw limit("schema owned string byte limit");owned+=length;}
            try{String s=StandardCharsets.UTF_8.newDecoder().onMalformedInput(CodingErrorAction.REPORT)
                    .onUnmappableCharacter(CodingErrorAction.REPORT).decode(ByteBuffer.wrap(bytes,position,(int)length)).toString();position+=(int)length;return s;
            }catch(CharacterCodingException failure){throw error("invalid UTF-8");}
        }
        String string()throws PhonException{return readString(false);}
        void field(String expected)throws PhonException{if(!string().equals(expected))throw error("wrong schema field name");}
        void struct(String expected,int count)throws PhonException{expect(STRUCT);if(!string().equals(expected)||u32()!=count)throw error("wrong schema struct shape");}
        int list()throws PhonException{expect(LIST);return count();}
        String valueString()throws PhonException{expect(STRING);return readString(true);}
        String symbol()throws PhonException{
            if(symbols==null)return valueString();long index=valueU32();
            if(index>=symbols.size())throw error("string index out of range");
            usedSymbols.add((int)index);return symbols.get((int)index);
        }
        long valueU32()throws PhonException{expect(U32);return u32();}
        long valueU64()throws PhonException{expect(U64);return u64();}
        boolean valueBool()throws PhonException{expect(BOOL);int x=tag();if(x>1)throw error("invalid bool");return x==1;}
        void depth(int depth)throws PhonException{if(depth>limits.nestingDepth())throw limit("schema nesting exceeds nestingDepth");}
        void finished()throws PhonException{if(remaining()!=0)throw error("trailing schema bytes");}
        PhonException error(String message){return new PhonException(PhonException.Kind.MALFORMED,message,position,null);}
    }
}
