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
}
