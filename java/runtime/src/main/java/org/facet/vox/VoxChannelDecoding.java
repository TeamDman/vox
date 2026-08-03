package org.facet.vox;

/** Generated argument-adapter hook for resolving channel-table indices. */
public final class VoxChannelDecoding {
    private static final ThreadLocal<InboundCall> CURRENT = new ThreadLocal<>();

    static <T> T with(InboundCall call, CheckedSupplier<T> supplier) throws Exception {
        InboundCall previous = CURRENT.get();
        CURRENT.set(call);
        try {
            return supplier.get();
        } finally {
            if (previous == null) CURRENT.remove();
            else CURRENT.set(previous);
        }
    }

    @SuppressWarnings("unchecked")
    public static <T> VoxTx<T> tx(long index) throws org.facet.phon.PhonException {
        return (VoxTx<T>) current().tx(index);
    }

    public static <T> VoxTx<T> tx(byte[] encodedIndex) throws org.facet.phon.PhonException {
        return tx(decodeIndex(encodedIndex));
    }

    @SuppressWarnings("unchecked")
    public static <T> VoxRx<T> rx(long index) throws org.facet.phon.PhonException {
        return (VoxRx<T>) current().rx(index);
    }

    public static <T> VoxRx<T> rx(byte[] encodedIndex) throws org.facet.phon.PhonException {
        return rx(decodeIndex(encodedIndex));
    }

    public static byte[] encodeIndex(long index) {
        if (index < 0 || index > 0xffff_ffffL) {
            throw new IllegalArgumentException("channel-table index is outside u32");
        }
        return new byte[] {
            (byte) index,
            (byte) (index >>> 8),
            (byte) (index >>> 16),
            (byte) (index >>> 24)
        };
    }

    private static long decodeIndex(byte[] bytes) throws org.facet.phon.PhonException {
        if (bytes.length != Integer.BYTES) {
            throw new org.facet.phon.PhonException(
                    org.facet.phon.PhonException.Kind.MALFORMED,
                    "channel-table index payload must contain exactly four bytes");
        }
        return Byte.toUnsignedLong(bytes[0])
                | (Byte.toUnsignedLong(bytes[1]) << 8)
                | (Byte.toUnsignedLong(bytes[2]) << 16)
                | (Byte.toUnsignedLong(bytes[3]) << 24);
    }

    private static InboundCall current() throws org.facet.phon.PhonException {
        InboundCall call = CURRENT.get();
        if (call == null) {
            throw new org.facet.phon.PhonException(
                    org.facet.phon.PhonException.Kind.DECODE,
                    "channel argument decoded outside an inbound call");
        }
        return call;
    }

    @FunctionalInterface interface CheckedSupplier<T> { T get() throws Exception; }
    private VoxChannelDecoding() {}
}
