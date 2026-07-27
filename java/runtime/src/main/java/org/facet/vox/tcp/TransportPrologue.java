package org.facet.vox.tcp;

import java.io.IOException;
import java.util.Arrays;
import org.facet.vox.VoxException;

public final class TransportPrologue {
    private TransportPrologue() {}

    public static void initiate(StreamFraming framing) throws IOException, VoxException {
        framing.writeFrame(message(WireConstants.TRANSPORT_HELLO_MAGIC, 0));
        byte[] response = requireEightBytes(framing.readFrame());
        if (magicIs(response, WireConstants.TRANSPORT_ACCEPT_MAGIC)) {
            requireVersionAndReserved(response, false);
            return;
        }
        if (magicIs(response, WireConstants.TRANSPORT_REJECT_MAGIC)) {
            requireVersionAndReserved(response, true);
            int reason = Byte.toUnsignedInt(response[5]);
            if (reason == WireConstants.TRANSPORT_REJECT_UNSUPPORTED_PROLOGUE) {
                throw new VoxException("transport rejected: unsupported transport prologue");
            }
            throw new VoxException("unknown transport reject reason " + reason);
        }
        throw new VoxException("expected TransportAccept or TransportReject");
    }

    public static void accept(StreamFraming framing) throws IOException, VoxException {
        byte[] hello = requireEightBytes(framing.readFrame());
        if (!magicIs(hello, WireConstants.TRANSPORT_HELLO_MAGIC)) {
            throw new VoxException("transport hello magic mismatch");
        }
        if (Byte.toUnsignedInt(hello[4]) != WireConstants.TRANSPORT_VERSION) {
            throw new VoxException(
                    "unsupported transport version " + Byte.toUnsignedInt(hello[4]));
        }
        if (hello[5] != 0 || hello[6] != 0 || hello[7] != 0) {
            framing.writeFrame(message(
                    WireConstants.TRANSPORT_REJECT_MAGIC,
                    WireConstants.TRANSPORT_REJECT_UNSUPPORTED_PROLOGUE));
            throw new VoxException("transport hello reserved bytes must be zero");
        }
        framing.writeFrame(message(WireConstants.TRANSPORT_ACCEPT_MAGIC, 0));
    }

    private static byte[] message(byte[] magic, int reason) {
        return new byte[] {
            magic[0], magic[1], magic[2], magic[3],
            (byte) WireConstants.TRANSPORT_VERSION, (byte) reason, 0, 0
        };
    }

    private static byte[] requireEightBytes(byte[] bytes) throws VoxException {
        if (bytes == null) throw new VoxException("link closed during transport prologue");
        if (bytes.length != 8) {
            throw new VoxException("transport prologue message size mismatch");
        }
        return bytes;
    }

    private static boolean magicIs(byte[] message, byte[] magic) {
        return Arrays.equals(Arrays.copyOf(message, 4), magic);
    }

    private static void requireVersionAndReserved(byte[] message, boolean reject)
            throws VoxException {
        if (Byte.toUnsignedInt(message[4]) != WireConstants.TRANSPORT_VERSION) {
            throw new VoxException(
                    "unsupported transport version " + Byte.toUnsignedInt(message[4]));
        }
        int firstReserved = reject ? 6 : 5;
        for (int index = firstReserved; index < 8; index++) {
            if (message[index] != 0) {
                throw new VoxException(
                        (reject ? "transport reject" : "transport accept")
                                + " reserved bytes must be zero");
            }
        }
    }
}
