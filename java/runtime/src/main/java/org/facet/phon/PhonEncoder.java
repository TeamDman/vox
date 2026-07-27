package org.facet.phon;

import java.io.ByteArrayOutputStream;
import java.math.BigInteger;
import java.nio.charset.StandardCharsets;
import java.util.Objects;
import java.util.Map;

/** Bounded little-endian compact encoder used by generated adapters. */
public final class PhonEncoder {
    private final ByteArrayOutputStream out = new ByteArrayOutputStream();
    private final PhonLimits limits;
    private int depth;

    PhonEncoder(PhonLimits limits) { this.limits = Objects.requireNonNull(limits); }

    byte[] finish() throws PhonException {
        byte[] bytes = out.toByteArray();
        if (bytes.length > limits.inputBytes()) throw limit("encoded input exceeds inputBytes");
        return bytes;
    }

    private PhonException limit(String message) { return new PhonException(PhonException.Kind.LIMIT, message); }
    private void capacity(int additional) throws PhonException {
        if (additional < 0 || out.size() > limits.inputBytes() - additional) throw limit("encoded input exceeds inputBytes");
    }
    private void raw(int value) throws PhonException { capacity(1); out.write(value); }
    private void little(long value, int width) throws PhonException {
        align(width);
        capacity(width);
        for (int i = 0; i < width; i++) out.write((int) (value >>> (8 * i)) & 0xff);
    }
    private void bigInteger(BigInteger value, int width, boolean signed) throws PhonException {
        Objects.requireNonNull(value);
        int bits = width * 8;
        if (signed) {
            if (value.bitLength() > bits - 1) throw new PhonException(PhonException.Kind.ENCODE, "signed integer out of range");
            if (value.signum() < 0) value = value.add(BigInteger.ONE.shiftLeft(bits));
        } else if (value.signum() < 0 || value.bitLength() > bits) {
            throw new PhonException(PhonException.Kind.ENCODE, "unsigned integer out of range");
        }
        align(width);
        capacity(width);
        for (int i = 0; i < width; i++) out.write(value.shiftRight(8 * i).byteValue() & 0xff);
    }
    private void align(int alignment) throws PhonException {
        while (out.size() % alignment != 0) raw(0);
    }

    public void writeBool(boolean value) throws PhonException { raw(value ? 1 : 0); }
    public void writeU8(int value) throws PhonException {
        if (value < 0 || value > 255) throw new PhonException(PhonException.Kind.ENCODE, "u8 out of range");
        raw(value);
    }
    public void writeU16(int value) throws PhonException {
        if (value < 0 || value > 65535) throw new PhonException(PhonException.Kind.ENCODE, "u16 out of range");
        little(value, 2);
    }
    public void writeU32(long value) throws PhonException {
        if (value < 0 || value > 0xffff_ffffL) throw new PhonException(PhonException.Kind.ENCODE, "u32 out of range");
        little(value, 4);
    }
    public void writeU64(BigInteger value) throws PhonException { bigInteger(value, 8, false); }
    public void writeU128(BigInteger value) throws PhonException { bigInteger(value, 16, false); }
    public void writeI8(byte value) throws PhonException { raw(value); }
    public void writeI16(short value) throws PhonException { little(value, 2); }
    public void writeI32(int value) throws PhonException { little(value, 4); }
    public void writeI64(long value) throws PhonException { little(value, 8); }
    public void writeI128(BigInteger value) throws PhonException { bigInteger(value, 16, true); }
    public void writeF32(float value) throws PhonException { little(Float.floatToRawIntBits(value), 4); }
    public void writeF64(double value) throws PhonException { little(Double.doubleToRawLongBits(value), 8); }
    public void writeChar(int codePoint) throws PhonException {
        if (!Character.isValidCodePoint(codePoint) || codePoint >= 0xd800 && codePoint <= 0xdfff)
            throw new PhonException(PhonException.Kind.ENCODE, "invalid Unicode scalar");
        writeU32(codePoint);
    }
    public void writeString(String value) throws PhonException {
        byte[] bytes = Objects.requireNonNull(value).getBytes(StandardCharsets.UTF_8);
        writeByteRun(bytes);
    }
    public void writeBytes(byte[] value) throws PhonException { writeByteRun(value.clone()); }
    private void writeByteRun(byte[] bytes) throws PhonException {
        if (bytes.length > limits.byteRunLength()) throw limit("byte run exceeds byteRunLength");
        writeCount(bytes.length); capacity(bytes.length); out.write(bytes, 0, bytes.length);
    }
    public void writeCount(int count) throws PhonException {
        if (count < 0 || count > limits.collectionEntries()) throw limit("count exceeds collectionEntries");
        capacity(4);
        for (int index = 0; index < 4; index++) {
            out.write((count >>> (8 * index)) & 0xff);
        }
    }
    public void writePresence(boolean present) throws PhonException { writeBool(present); }
    /** Encode the protocol's constrained self-describing dynamic metadata value. */
    public void writeDynamic(Value value) throws PhonException {
        writeDynamic(value, 0);
    }
    private void writeDynamic(Value value, int nesting) throws PhonException {
        if (nesting > limits.nestingDepth()) throw limit("dynamic nesting exceeds nestingDepth");
        if (value.wireTag() >= 0 && writePreservedDynamicScalar(value)) return;
        switch (value.type()) {
            case NULL -> raw(0x18);
            case BOOL -> { raw(0x01); raw(value.asBool() ? 1 : 0); }
            case UNSIGNED -> {
                raw(0x05);
                dynamicLittle(value.asInteger(), 8, false);
            }
            case STRING -> {
                raw(0x0f);
                dynamicByteRun(value.asString().getBytes(StandardCharsets.UTF_8));
            }
            case BYTES -> {
                raw(0x10);
                dynamicByteRun(value.asBytes());
            }
            case LIST -> {
                if (value.asList().size() > limits.collectionEntries()) {
                    throw limit("dynamic list exceeds collectionEntries");
                }
                raw(value.wireTag() == 0x15 ? 0x15 : 0x11);
                dynamicU32(value.asList().size());
                for (Value item : value.asList()) writeDynamic(item, nesting + 1);
            }
            case MAP -> {
                if (value.asMap().size() > limits.collectionEntries()) {
                    throw limit("dynamic map exceeds collectionEntries");
                }
                raw(0x13);
                dynamicU32(value.asMap().size());
                for (Map.Entry<String, Value> entry : value.asMap().entrySet()) {
                    raw(0x0f);
                    dynamicByteRun(entry.getKey().getBytes(StandardCharsets.UTF_8));
                    writeDynamic(entry.getValue(), nesting + 1);
                }
            }
            default -> throw new PhonException(PhonException.Kind.ENCODE,
                    "dynamic metadata supports only null, bool, u64, string, bytes, and object");
        }
    }
    private boolean writePreservedDynamicScalar(Value value) throws PhonException {
        int tag = value.wireTag();
        switch (tag) {
            case 0x02 -> { raw(tag); dynamicLittle(value.asInteger(), 1, false); }
            case 0x03 -> { raw(tag); dynamicLittle(value.asInteger(), 2, false); }
            case 0x04 -> { raw(tag); dynamicLittle(value.asInteger(), 4, false); }
            case 0x05 -> { raw(tag); dynamicLittle(value.asInteger(), 8, false); }
            case 0x06 -> { raw(tag); dynamicLittle(value.asInteger(), 16, false); }
            case 0x07 -> { raw(tag); dynamicLittle(value.asInteger(), 1, true); }
            case 0x08 -> { raw(tag); dynamicLittle(value.asInteger(), 2, true); }
            case 0x09 -> { raw(tag); dynamicLittle(value.asInteger(), 4, true); }
            case 0x0a -> { raw(tag); dynamicLittle(value.asInteger(), 8, true); }
            case 0x0b -> { raw(tag); dynamicLittle(value.asInteger(), 16, true); }
            case 0x0c -> {
                raw(tag);
                dynamicUnsignedBits(Float.floatToRawIntBits((float) value.asDouble()), 4);
            }
            case 0x0d -> {
                raw(tag);
                dynamicUnsignedBits(Double.doubleToRawLongBits(value.asDouble()), 8);
            }
            case 0x0e -> { raw(tag); dynamicUnsignedBits(value.asCodePoint(), 4); }
            default -> { return false; }
        }
        return true;
    }
    private void dynamicUnsignedBits(long value, int width) throws PhonException {
        capacity(width);
        for (int index = 0; index < width; index++) {
            out.write((int) (value >>> (8 * index)) & 0xff);
        }
    }
    private void dynamicByteRun(byte[] bytes) throws PhonException {
        if (bytes.length > limits.byteRunLength()) throw limit("dynamic byte run exceeds byteRunLength");
        dynamicU32(bytes.length);
        capacity(bytes.length);
        out.write(bytes, 0, bytes.length);
    }
    private void dynamicU32(long value) throws PhonException {
        capacity(4);
        for (int index = 0; index < 4; index++) out.write((int) (value >>> (8 * index)) & 0xff);
    }
    private void dynamicLittle(BigInteger value, int width, boolean signed) throws PhonException {
        int bits = width * 8;
        if (signed) {
            if (value.bitLength() > bits - 1) throw new PhonException(PhonException.Kind.ENCODE, "dynamic signed integer out of range");
            if (value.signum() < 0) value = value.add(BigInteger.ONE.shiftLeft(bits));
        } else if (value.signum() < 0 || value.bitLength() > bits) {
            throw new PhonException(PhonException.Kind.ENCODE, "dynamic unsigned integer out of range");
        }
        capacity(width);
        for (int index = 0; index < width; index++) out.write(value.shiftRight(8 * index).byteValue() & 0xff);
    }
    public <T> void writeAdapted(PhonAdapter<T> adapter, T value) throws PhonException {
        if (++depth > limits.nestingDepth()) throw limit("nesting exceeds nestingDepth");
        try { adapter.encode(this, value); } finally { depth--; }
    }
}
