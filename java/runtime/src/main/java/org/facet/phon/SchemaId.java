package org.facet.phon;

import java.util.Arrays;

/** Immutable eight-byte Phon schema identifier in little-endian wire order. */
public final class SchemaId implements Comparable<SchemaId> {
    public static final int WIDTH = 8;
    private final byte[] bytes;

    public SchemaId(byte[] bytes) {
        if (bytes.length != WIDTH) throw new IllegalArgumentException("SchemaId must contain 8 bytes");
        this.bytes = bytes.clone();
    }

    public static SchemaId fromLong(long value) {
        byte[] bytes = new byte[WIDTH];
        for (int i = 0; i < WIDTH; i++) bytes[i] = (byte) (value >>> (8 * i));
        return new SchemaId(bytes);
    }

    public long asLong() {
        long value = 0;
        for (int i = 0; i < WIDTH; i++) value |= (long) (bytes[i] & 0xff) << (8 * i);
        return value;
    }

    public byte[] bytes() { return bytes.clone(); }

    @Override public String toString() { return String.format("%016x", asLong()); }
    @Override public boolean equals(Object other) {
        return other instanceof SchemaId && Arrays.equals(bytes, ((SchemaId) other).bytes);
    }
    @Override public int hashCode() { return Arrays.hashCode(bytes); }
    @Override public int compareTo(SchemaId other) {
        return Long.compareUnsigned(asLong(), other.asLong());
    }
}
