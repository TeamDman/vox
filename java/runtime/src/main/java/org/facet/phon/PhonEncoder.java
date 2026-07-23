package org.facet.phon;

import java.io.ByteArrayOutputStream;
import java.math.BigInteger;
import java.nio.charset.StandardCharsets;
import java.util.Objects;

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
        writeU32(count);
    }
    public void writePresence(boolean present) throws PhonException { writeBool(present); }
    public <T> void writeAdapted(PhonAdapter<T> adapter, T value) throws PhonException {
        if (++depth > limits.nestingDepth()) throw limit("nesting exceeds nestingDepth");
        try { adapter.encode(this, value); } finally { depth--; }
    }
}
