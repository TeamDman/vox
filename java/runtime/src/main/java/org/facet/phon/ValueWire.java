package org.facet.phon;

import java.io.ByteArrayOutputStream;
import java.math.BigInteger;
import java.nio.ByteBuffer;
import java.nio.charset.CharacterCodingException;
import java.nio.charset.CodingErrorAction;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/** Self-describing Value codec used by the cross-language value corpus. */
final class ValueWire {
    private static final int UNIT=0,BOOL=1,U8=2,U16=3,U32=4,U64=5,U128=6,I8=7,I16=8,I32=9,I64=10,I128=11,F32=12,F64=13,CHAR=14,STRING=15,BYTES=16,LIST=17,SET=18,MAP=19,ARRAY=20,TUPLE=21,STRUCT=22,ENUM=23,NONE=24,SOME=25,TENSOR=26,DATETIME=27,UUID=28,QNAME=29;
    private ValueWire(){}
    static Value decode(byte[]bytes,PhonLimits limits)throws PhonException{In i=new In(bytes,limits);Value v=read(i,0);if(i.left()!=0)throw i.bad("trailing bytes");return v;}
    static byte[]encode(Value value,PhonLimits limits)throws PhonException{Out o=new Out(limits);write(o,value,0);return o.bytes();}
    private static Value read(In i,int depth)throws PhonException{
        if(depth>i.l.nestingDepth())throw i.limit("nesting exceeds nestingDepth");int tag=i.u8();
        switch(tag){
            case UNIT:case NONE:return Value.wire(Value.Type.NULL,null,tag);
            case BOOL:{int b=i.u8();if(b>1)throw i.bad("invalid bool");return Value.wire(Value.Type.BOOL,b==1,tag);}
            case U8:return Value.wire(Value.Type.UNSIGNED,BigInteger.valueOf(i.u8()),tag);
            case U16:return Value.wire(Value.Type.UNSIGNED,BigInteger.valueOf(i.leLong(2)),tag);
            case U32:return Value.wire(Value.Type.UNSIGNED,BigInteger.valueOf(i.leLong(4)),tag);
            case U64:case U128:return Value.wire(Value.Type.UNSIGNED,i.big(tag==U64?8:16,false),tag);
            case I8:return Value.wire(Value.Type.SIGNED,BigInteger.valueOf((byte)i.u8()),tag);
            case I16:return Value.wire(Value.Type.SIGNED,BigInteger.valueOf((short)i.leLong(2)),tag);
            case I32:return Value.wire(Value.Type.SIGNED,BigInteger.valueOf((int)i.leLong(4)),tag);
            case I64:return Value.wire(Value.Type.SIGNED,BigInteger.valueOf(i.leLong(8)),tag);
            case I128:return Value.wire(Value.Type.SIGNED,i.big(16,true),tag);
            case F32:return Value.wire(Value.Type.FLOAT,(double)Float.intBitsToFloat((int)i.leLong(4)),tag);
            case F64:return Value.wire(Value.Type.FLOAT,Double.longBitsToDouble(i.leLong(8)),tag);
            case CHAR:{long cp=i.leLong(4);if(!Character.isValidCodePoint((int)cp)||cp>=0xd800&&cp<=0xdfff)throw i.bad("invalid char");return Value.wire(Value.Type.CHAR,(int)cp,tag);}
            case STRING:case DATETIME:case UUID:case QNAME:return Value.wire(Value.Type.STRING,i.string(),tag);
            case BYTES:return Value.wire(Value.Type.BYTES,i.run(),tag);
            case SOME:return read(i,depth+1);
            case LIST:case SET:case TUPLE:{int n=i.count();List<Value>v=new ArrayList<>();for(int x=0;x<n;x++){Value e=read(i,depth+1);if(tag==SET&&v.contains(e))throw i.bad("duplicate set element");v.add(e);}return Value.wire(Value.Type.LIST,List.copyOf(v),tag);}
            case MAP:{int n=i.count();Map<String,Value>m=new LinkedHashMap<>();for(int x=0;x<n;x++){Value key=read(i,depth+1);if(key.type()!=Value.Type.STRING)throw i.bad("non-string map key not supported by Value");if(m.put(key.asString(),read(i,depth+1))!=null)throw i.bad("duplicate map key");}return Value.wire(Value.Type.MAP,java.util.Collections.unmodifiableMap(m),tag);}
            case ARRAY:case TENSOR:{int rank=i.count();List<Long>dims=new ArrayList<>();long n=1;for(int x=0;x<rank;x++){long d=i.leLong(8);try{n=Math.multiplyExact(n,d);}catch(ArithmeticException e){throw i.bad("dimension overflow");}dims.add(d);}if(n>i.l.collectionEntries())throw i.limit("array exceeds collectionEntries");List<Value>v=new ArrayList<>();for(long x=0;x<n;x++)v.add(read(i,depth+1));return Value.wire(Value.Type.LIST,List.copyOf(v),tag,null,dims);}
            case STRUCT:{String name=i.string();int n=i.count();Map<String,Value>m=new LinkedHashMap<>();for(int x=0;x<n;x++){String field=i.string();if(m.put(field,read(i,depth+1))!=null)throw i.bad("duplicate field");}return Value.wire(Value.Type.MAP,java.util.Collections.unmodifiableMap(m),tag,name,List.of());}
            case ENUM:{String variant=i.string();return Value.wire(Value.Type.ENUM,new Value.EnumValue(variant,read(i,depth+1)),tag);}
            default:throw i.bad("unknown tag "+tag);
        }
    }
    private static void write(Out o,Value v,int depth)throws PhonException{
        if(depth>o.l.nestingDepth())throw o.limit("nesting exceeds nestingDepth");int tag=v.wireTag();
        if(tag<0){switch(v.type()){case NULL:tag=NONE;break;case BOOL:tag=BOOL;break;case SIGNED:tag=I64;break;case UNSIGNED:tag=U64;break;case FLOAT:tag=F64;break;case CHAR:tag=CHAR;break;case STRING:tag=STRING;break;case BYTES:tag=BYTES;break;case LIST:tag=LIST;break;case MAP:tag=MAP;break;case ENUM:tag=ENUM;break;default:throw o.bad("unsupported value");}}
        o.u8(tag);switch(tag){
            case UNIT:case NONE:return;case BOOL:o.u8(v.asBool()?1:0);return;
            case U8:o.integer(v.asInteger(),1,false);return;case U16:o.integer(v.asInteger(),2,false);return;case U32:o.integer(v.asInteger(),4,false);return;case U64:o.integer(v.asInteger(),8,false);return;case U128:o.integer(v.asInteger(),16,false);return;
            case I8:o.integer(v.asInteger(),1,true);return;case I16:o.integer(v.asInteger(),2,true);return;case I32:o.integer(v.asInteger(),4,true);return;case I64:o.integer(v.asInteger(),8,true);return;case I128:o.integer(v.asInteger(),16,true);return;
            case F32:o.le(Float.floatToRawIntBits((float)v.asDouble()),4);return;case F64:o.le(Double.doubleToRawLongBits(v.asDouble()),8);return;case CHAR:o.le(v.asCodePoint(),4);return;
            case STRING:case DATETIME:case UUID:case QNAME:o.string(v.asString());return;case BYTES:o.run(v.asBytes());return;case SOME:write(o,v,depth+1);return;
            case LIST:case SET:case TUPLE:o.count(v.asList().size());for(Value x:v.asList())write(o,x,depth+1);return;
            case MAP:o.count(v.asMap().size());for(Map.Entry<String,Value>x:v.asMap().entrySet()){o.u8(STRING);o.string(x.getKey());write(o,x.getValue(),depth+1);}return;
            case ARRAY:case TENSOR:o.count(v.wireDimensions().size());for(long d:v.wireDimensions())o.le(d,8);for(Value x:v.asList())write(o,x,depth+1);return;
            case STRUCT:o.string(v.wireName());o.count(v.asMap().size());for(Map.Entry<String,Value>x:v.asMap().entrySet()){o.string(x.getKey());write(o,x.getValue(),depth+1);}return;
            case ENUM:o.string(v.asEnum().variant());write(o,v.asEnum().payload(),depth+1);return;default:throw o.bad("unsupported tag");
        }
    }
    private static final class In{
        final byte[]b;final PhonLimits l;int p;In(byte[]b,PhonLimits l)throws PhonException{this.b=b;this.l=l;if(b.length>l.inputBytes())throw limit("input exceeds inputBytes");}int left(){return b.length-p;}void need(int n)throws PhonException{if(n<0||left()<n)throw new PhonException(PhonException.Kind.TRUNCATED,"truncated value",p,null);}int u8()throws PhonException{need(1);return b[p++]&255;}long leLong(int n)throws PhonException{need(n);long v=0;for(int x=0;x<n;x++)v|=(long)u8()<<(8*x);return v;}BigInteger big(int n,boolean signed)throws PhonException{byte[]x=new byte[n+(signed?0:1)];for(int q=0;q<n;q++)x[x.length-1-q]=(byte)u8();return new BigInteger(x);}int count()throws PhonException{long n=leLong(4)&0xffffffffL;if(n>l.collectionEntries())throw limit("count exceeds collectionEntries");return(int)n;}byte[]run()throws PhonException{int n=count();if(n>l.byteRunLength())throw limit("byte run exceeds byteRunLength");need(n);byte[]x=new byte[n];System.arraycopy(b,p,x,0,n);p+=n;return x;}String string()throws PhonException{byte[]x=run();try{return StandardCharsets.UTF_8.newDecoder().onMalformedInput(CodingErrorAction.REPORT).decode(ByteBuffer.wrap(x)).toString();}catch(CharacterCodingException e){throw bad("invalid UTF-8");}}PhonException bad(String m){return new PhonException(PhonException.Kind.MALFORMED,m,p,null);}PhonException limit(String m){return new PhonException(PhonException.Kind.LIMIT,m,p,null);}
    }
    private static final class Out{
        final ByteArrayOutputStream o=new ByteArrayOutputStream();final PhonLimits l;Out(PhonLimits l){this.l=l;}void cap(int n)throws PhonException{if(n<0||o.size()>l.inputBytes()-n)throw limit("output exceeds inputBytes");}void u8(int x)throws PhonException{cap(1);o.write(x);}void le(long v,int n)throws PhonException{for(int x=0;x<n;x++)u8((int)(v>>>(8*x)));}void integer(BigInteger v,int n,boolean signed)throws PhonException{int bits=n*8;if(signed){if(v.bitLength()>bits-1)throw bad("integer out of range");if(v.signum()<0)v=v.add(BigInteger.ONE.shiftLeft(bits));}else if(v.signum()<0||v.bitLength()>bits)throw bad("integer out of range");for(int x=0;x<n;x++)u8(v.shiftRight(8*x).intValue());}void count(int n)throws PhonException{if(n<0||n>l.collectionEntries())throw limit("count exceeds collectionEntries");le(n,4);}void run(byte[]b)throws PhonException{if(b.length>l.byteRunLength())throw limit("byte run exceeds byteRunLength");count(b.length);cap(b.length);o.write(b,0,b.length);}void string(String s)throws PhonException{run(s.getBytes(StandardCharsets.UTF_8));}byte[]bytes(){return o.toByteArray();}PhonException bad(String m){return new PhonException(PhonException.Kind.ENCODE,m);}PhonException limit(String m){return new PhonException(PhonException.Kind.LIMIT,m);}
    }
}
