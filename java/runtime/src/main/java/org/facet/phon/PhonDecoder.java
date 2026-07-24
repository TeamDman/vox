package org.facet.phon;

import java.math.BigInteger;
import java.nio.ByteBuffer;
import java.nio.charset.CharacterCodingException;
import java.nio.charset.CodingErrorAction;
import java.nio.charset.StandardCharsets;
import java.util.Objects;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/** Bounded little-endian compact decoder used by generated adapters. */
public final class PhonDecoder {
    private final byte[] bytes;
    private final PhonLimits limits;
    private int position;
    private int depth;

    PhonDecoder(byte[] bytes, PhonLimits limits) throws PhonException {
        this.bytes = Objects.requireNonNull(bytes).clone();
        this.limits = Objects.requireNonNull(limits);
        if (bytes.length > limits.inputBytes()) throw limit("input exceeds inputBytes");
    }

    public int position() { return position; }
    public int remaining() { return bytes.length - position; }
    public void requireFinished() throws PhonException {
        if (remaining() != 0) throw malformed("trailing bytes: " + remaining());
    }
    private PhonException limit(String message) { return new PhonException(PhonException.Kind.LIMIT, message, position, null); }
    private PhonException malformed(String message) { return new PhonException(PhonException.Kind.MALFORMED, message, position, null); }
    private void need(int amount) throws PhonException {
        if (amount < 0 || remaining() < amount)
            throw new PhonException(PhonException.Kind.TRUNCATED, "need " + amount + " bytes, have " + remaining(), position, null);
    }
    private int raw() throws PhonException { need(1); return bytes[position++] & 0xff; }
    private long little(int width) throws PhonException {
        align(width); need(width); long value = 0;
        for (int i = 0; i < width; i++) value |= (long) raw() << (8 * i);
        return value;
    }
    private BigInteger bigInteger(int width, boolean signed) throws PhonException {
        align(width); need(width); byte[] bigEndian = new byte[width];
        for (int i = 0; i < width; i++) bigEndian[width - 1 - i] = (byte) raw();
        return new BigInteger(signed ? bigEndian : prependZero(bigEndian));
    }
    private static byte[] prependZero(byte[] bytes) {
        byte[] out = new byte[bytes.length + 1]; System.arraycopy(bytes, 0, out, 1, bytes.length); return out;
    }
    private void align(int alignment) throws PhonException {
        while (position % alignment != 0) {
            if (raw() != 0) throw malformed("non-zero alignment padding");
        }
    }

    public boolean readBool() throws PhonException {
        int value = raw(); if (value > 1) throw malformed("invalid boolean " + value); return value == 1;
    }
    public int readU8() throws PhonException { return raw(); }
    public int readU16() throws PhonException { return (int) little(2); }
    public long readU32() throws PhonException { return little(4) & 0xffff_ffffL; }
    public BigInteger readU64() throws PhonException { return bigInteger(8, false); }
    public BigInteger readU128() throws PhonException { return bigInteger(16, false); }
    public byte readI8() throws PhonException { return (byte) raw(); }
    public short readI16() throws PhonException { return (short) little(2); }
    public int readI32() throws PhonException { return (int) little(4); }
    public long readI64() throws PhonException { return little(8); }
    public BigInteger readI128() throws PhonException { return bigInteger(16, true); }
    public float readF32() throws PhonException { return Float.intBitsToFloat((int) little(4)); }
    public double readF64() throws PhonException { return Double.longBitsToDouble(little(8)); }
    public int readChar() throws PhonException {
        long value = readU32();
        if (!Character.isValidCodePoint((int) value) || value >= 0xd800 && value <= 0xdfff) throw malformed("invalid Unicode scalar");
        return (int) value;
    }
    public int readCount() throws PhonException {
        need(4);
        long count = 0;
        for (int index = 0; index < 4; index++) {
            count |= (long) raw() << (8 * index);
        }
        count &= 0xffff_ffffL;
        if (count > limits.collectionEntries()) throw limit("count exceeds collectionEntries");
        return (int) count;
    }
    public boolean readPresence() throws PhonException { return readBool(); }
    /** Decode the protocol's constrained self-describing dynamic metadata value. */
    public Value readDynamic() throws PhonException { return readDynamic(0); }
    private Value readDynamic(int nesting) throws PhonException {
        if (nesting > limits.nestingDepth()) throw limit("dynamic nesting exceeds nestingDepth");
        int tag = raw();
        return switch (tag) {
            case 0x00, 0x18 -> Value.nullValue();
            case 0x01 -> Value.bool(readDynamicBool());
            case 0x02 -> Value.wire(Value.Type.UNSIGNED, dynamicInteger(1, false), tag);
            case 0x03 -> Value.wire(Value.Type.UNSIGNED, dynamicInteger(2, false), tag);
            case 0x04 -> Value.wire(Value.Type.UNSIGNED, dynamicInteger(4, false), tag);
            case 0x05 -> Value.wire(Value.Type.UNSIGNED, dynamicInteger(8, false), tag);
            case 0x06 -> Value.wire(Value.Type.UNSIGNED, dynamicInteger(16, false), tag);
            case 0x07 -> Value.wire(Value.Type.SIGNED, dynamicInteger(1, true), tag);
            case 0x08 -> Value.wire(Value.Type.SIGNED, dynamicInteger(2, true), tag);
            case 0x09 -> Value.wire(Value.Type.SIGNED, dynamicInteger(4, true), tag);
            case 0x0a -> Value.wire(Value.Type.SIGNED, dynamicInteger(8, true), tag);
            case 0x0b -> Value.wire(Value.Type.SIGNED, dynamicInteger(16, true), tag);
            case 0x0c -> Value.wire(
                    Value.Type.FLOAT,
                    (double) Float.intBitsToFloat(dynamicInteger(4, false).intValue()),
                    tag);
            case 0x0d -> Value.wire(
                    Value.Type.FLOAT,
                    Double.longBitsToDouble(dynamicInteger(8, false).longValue()),
                    tag);
            case 0x0e -> {
                int codePoint = dynamicInteger(4, false).intValue();
                if (!Character.isValidCodePoint(codePoint)
                        || codePoint >= 0xd800 && codePoint <= 0xdfff) {
                    throw malformed("invalid dynamic Unicode scalar");
                }
                yield Value.wire(Value.Type.CHAR, codePoint, tag);
            }
            case 0x0f -> Value.string(dynamicString());
            case 0x10 -> Value.bytes(dynamicBytes());
            case 0x11, 0x15 -> {
                int count = dynamicCount();
                java.util.ArrayList<Value> result = new java.util.ArrayList<>(count);
                for (int index = 0; index < count; index++) {
                    result.add(readDynamic(nesting + 1));
                }
                yield Value.wire(Value.Type.LIST, List.copyOf(result), tag);
            }
            case 0x13 -> {
                int count = dynamicCount();
                Map<String, Value> result = new LinkedHashMap<>();
                for (int index = 0; index < count; index++) {
                    if (raw() != 0x0f) throw malformed("dynamic metadata key is not a string");
                    String key = dynamicString();
                    if (result.put(key, readDynamic(nesting + 1)) != null) {
                        throw malformed("duplicate dynamic metadata key " + key);
                    }
                }
                yield Value.map(result);
            }
            case 0x19 -> readDynamic(nesting + 1);
            default -> throw malformed("unsupported dynamic metadata tag " + tag);
        };
    }
    private boolean readDynamicBool() throws PhonException {
        int value = raw();
        if (value > 1) throw malformed("invalid dynamic boolean " + value);
        return value == 1;
    }
    private int dynamicCount() throws PhonException {
        BigInteger count = dynamicInteger(4, false);
        if (count.compareTo(BigInteger.valueOf(limits.collectionEntries())) > 0) {
            throw limit("dynamic count exceeds collectionEntries");
        }
        return count.intValue();
    }
    private byte[] dynamicBytes() throws PhonException {
        int length = dynamicCount();
        if (length > limits.byteRunLength()) throw limit("dynamic byte run exceeds byteRunLength");
        need(length);
        byte[] result = new byte[length];
        System.arraycopy(bytes, position, result, 0, length);
        position += length;
        return result;
    }
    private String dynamicString() throws PhonException {
        byte[] value = dynamicBytes();
        try {
            return StandardCharsets.UTF_8.newDecoder()
                    .onMalformedInput(CodingErrorAction.REPORT)
                    .onUnmappableCharacter(CodingErrorAction.REPORT)
                    .decode(ByteBuffer.wrap(value)).toString();
        } catch (CharacterCodingException error) {
            throw malformed("invalid dynamic UTF-8");
        }
    }
    private BigInteger dynamicInteger(int width, boolean signed) throws PhonException {
        need(width);
        byte[] bigEndian = new byte[width];
        for (int index = 0; index < width; index++) {
            bigEndian[width - 1 - index] = (byte) raw();
        }
        return new BigInteger(signed ? bigEndian : prependZero(bigEndian));
    }
    public String readString() throws PhonException {
        byte[] value = readBytes();
        try {
            return StandardCharsets.UTF_8.newDecoder()
                    .onMalformedInput(CodingErrorAction.REPORT).onUnmappableCharacter(CodingErrorAction.REPORT)
                    .decode(ByteBuffer.wrap(value)).toString();
        } catch (CharacterCodingException e) {
            throw new PhonException(PhonException.Kind.MALFORMED, "invalid UTF-8", position, null);
        }
    }
    public byte[] readBytes() throws PhonException {
        int length = readCount();
        if (length > limits.byteRunLength()) throw limit("byte run exceeds byteRunLength");
        need(length); byte[] out = new byte[length]; System.arraycopy(bytes, position, out, 0, length); position += length; return out;
    }
    public <T> T readAdapted(PhonAdapter<T> adapter) throws PhonException {
        if (++depth > limits.nestingDepth()) throw limit("nesting exceeds nestingDepth");
        try { return adapter.decode(this); } finally { depth--; }
    }
}
