package org.facet.phon;

/** Entry points for typed and language-neutral compact Phon encoding. */
public final class PhonCodec {
    private PhonCodec() {}

    public static <T> byte[] encode(PhonAdapter<T> adapter, T value, PhonLimits limits)
            throws PhonException {
        PhonEncoder encoder = new PhonEncoder(limits);
        adapter.encode(encoder, value);
        return encoder.finish();
    }

    public static <T> T decode(PhonAdapter<T> adapter, byte[] bytes, PhonLimits limits)
            throws PhonException {
        PhonDecoder decoder = new PhonDecoder(bytes, limits);
        T value = adapter.decode(decoder);
        decoder.requireFinished();
        return value;
    }

    public static Value decodeValue(SchemaClosure writer, byte[] bytes, PhonLimits limits)
            throws PhonException {
        PhonDecoder decoder = new PhonDecoder(bytes, limits);
        Value value = CompactValues.decode(writer, writer.root().id(), decoder, 0);
        decoder.requireFinished();
        return value;
    }

    public static byte[] encodeValue(
            SchemaClosure schema, Value value, PhonLimits limits) throws PhonException {
        return CompactValues.encode(schema, value, limits);
    }

    public static Value decodeCompatibleValue(
            SchemaClosure writer, SchemaClosure reader, byte[] bytes, PhonLimits limits)
            throws PhonException {
        Value source = decodeValue(writer, bytes, limits);
        return CompatibilityPlan.plan(writer, reader, limits).translate(source);
    }

    public static byte[] transcode(
            SchemaClosure writer, SchemaClosure reader, byte[] bytes, PhonLimits limits)
            throws PhonException {
        return encodeValue(reader, decodeCompatibleValue(writer, reader, bytes, limits), limits);
    }
}
