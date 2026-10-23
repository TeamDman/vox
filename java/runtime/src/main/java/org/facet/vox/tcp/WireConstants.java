package org.facet.vox.tcp;

/**
 * Temporary Java projection of the normative Rust constants.
 *
 * <p>The Java code generator will replace this hand-written integration fixture. Keeping the
 * values in one class prevents the runtime and tests from growing independent copies.
 */
public final class WireConstants {
    public static final byte[] LINK_MAGIC = {'V', 'O', 'X', 'L'};
    public static final int LINK_VERSION = 1;
    public static final int LINK_FLAG_FD_CAPABLE = 0x01;
    public static final int LINK_PROLOGUE_BYTES = 6;

    public static final byte[] TRANSPORT_HELLO_MAGIC = {'V', 'O', 'T', 'H'};
    public static final byte[] TRANSPORT_ACCEPT_MAGIC = {'V', 'O', 'T', 'A'};
    public static final byte[] TRANSPORT_REJECT_MAGIC = {'V', 'O', 'T', 'R'};
    public static final int TRANSPORT_VERSION = 9;
    public static final int TRANSPORT_REJECT_UNSUPPORTED_PROLOGUE = 1;

    private WireConstants() {}
}
