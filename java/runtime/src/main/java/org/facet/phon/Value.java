package org.facet.phon;

import java.math.BigInteger;
import java.util.Arrays;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Objects;

/** Immutable language-neutral value tree used for conformance and diagnostics. */
public final class Value {
    public enum Type { NULL, BOOL, SIGNED, UNSIGNED, FLOAT, CHAR, STRING, BYTES, LIST, MAP, ENUM }
    private final Type type;
    private final Object value;
    private final int wireTag;
    private final String wireName;
    private final List<Long> wireDimensions;
    private Value(Type type, Object value) { this(type, value, -1, null, List.of()); }
    private Value(Type type, Object value, int wireTag, String wireName, List<Long> wireDimensions) {
        this.type = type; this.value = value; this.wireTag = wireTag; this.wireName = wireName;
        this.wireDimensions = List.copyOf(wireDimensions);
    }

    public static Value nullValue() { return new Value(Type.NULL, null); }
    public static Value bool(boolean value) { return new Value(Type.BOOL, value); }
    public static Value signed(BigInteger value) { return new Value(Type.SIGNED, Objects.requireNonNull(value)); }
    public static Value signed(long value) { return signed(BigInteger.valueOf(value)); }
    public static Value unsigned(BigInteger value) {
        if (value.signum() < 0) throw new IllegalArgumentException("unsigned value must be nonnegative");
        return new Value(Type.UNSIGNED, value);
    }
    public static Value unsigned(long value) {
        if (value < 0) throw new IllegalArgumentException("use unsigned(BigInteger) above Long.MAX_VALUE");
        return unsigned(BigInteger.valueOf(value));
    }
    public static Value floating(double value) { return new Value(Type.FLOAT, value); }
    public static Value character(int codePoint) {
        if (!Character.isValidCodePoint(codePoint) || codePoint >= 0xd800 && codePoint <= 0xdfff)
            throw new IllegalArgumentException("invalid Unicode scalar");
        return new Value(Type.CHAR, codePoint);
    }
    public static Value string(String value) { return new Value(Type.STRING, Objects.requireNonNull(value)); }
    public static Value bytes(byte[] value) { return new Value(Type.BYTES, value.clone()); }
    public static Value list(List<Value> value) { return new Value(Type.LIST, List.copyOf(value)); }
    public static Value map(Map<String, Value> value) {
        return new Value(Type.MAP, java.util.Collections.unmodifiableMap(new LinkedHashMap<>(value)));
    }
    public static Value enumValue(String variant, Value payload) {
        return new Value(Type.ENUM, new EnumValue(variant, payload));
    }
    static Value wire(Type type, Object value, int tag) { return new Value(type, value, tag, null, List.of()); }
    static Value wire(Type type, Object value, int tag, String name, List<Long> dimensions) {
        return new Value(type, value, tag, name, dimensions);
    }

    public Type type() { return type; }
    public boolean asBool() { return (Boolean) value; }
    public BigInteger asInteger() { return (BigInteger) value; }
    public double asDouble() { return (Double) value; }
    public int asCodePoint() { return (Integer) value; }
    public String asString() { return (String) value; }
    public byte[] asBytes() { return ((byte[]) value).clone(); }
    @SuppressWarnings("unchecked") public List<Value> asList() { return (List<Value>) value; }
    @SuppressWarnings("unchecked") public Map<String, Value> asMap() { return (Map<String, Value>) value; }
    public EnumValue asEnum() { return (EnumValue) value; }
    int wireTag() { return wireTag; }
    String wireName() { return wireName; }
    List<Long> wireDimensions() { return wireDimensions; }

    public static final class EnumValue {
        private final String variant; private final Value payload;
        EnumValue(String variant, Value payload) {
            this.variant = Objects.requireNonNull(variant); this.payload = Objects.requireNonNull(payload);
        }
        public String variant() { return variant; } public Value payload() { return payload; }
        @Override public boolean equals(Object o) {
            return o instanceof EnumValue && variant.equals(((EnumValue)o).variant) && payload.equals(((EnumValue)o).payload);
        }
        @Override public int hashCode() { return Objects.hash(variant, payload); }
    }

    @Override public boolean equals(Object other) {
        if (!(other instanceof Value) || type != ((Value) other).type) return false;
        Value that = (Value) other;
        return type == Type.BYTES ? Arrays.equals((byte[]) value, (byte[]) that.value) : Objects.equals(value, that.value);
    }
    @Override public int hashCode() { return type == Type.BYTES ? Arrays.hashCode((byte[]) value) : Objects.hashCode(value); }
    @Override public String toString() { return type == Type.BYTES ? Arrays.toString((byte[]) value) : String.valueOf(value); }
}
