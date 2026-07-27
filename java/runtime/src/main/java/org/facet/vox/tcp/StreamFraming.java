package org.facet.vox.tcp;

import java.io.EOFException;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.Arrays;
import org.facet.vox.VoxException;

/** Bounded Vox framing over a byte stream. Exactly one thread may own each instance. */
public final class StreamFraming {
    private final InputStream input;
    private final OutputStream output;
    private final int maxFrameBytes;
    private boolean linkReady;
    private boolean eof;

    public StreamFraming(InputStream input, OutputStream output, int maxFrameBytes) {
        if (maxFrameBytes <= 0) {
            throw new IllegalArgumentException("maxFrameBytes must be positive");
        }
        this.input = input;
        this.output = output;
        this.maxFrameBytes = maxFrameBytes;
    }

    /** Send our link prologue and validate the peer prologue. TCP never carries descriptors. */
    public void exchangeLinkPrologue() throws IOException, VoxException {
        if (linkReady) {
            throw new IllegalStateException("link prologue already exchanged");
        }
        output.write(new byte[] {'V', 'O', 'X', 'L', (byte) WireConstants.LINK_VERSION, 0});
        output.flush();
        byte[] peer = readExactly(WireConstants.LINK_PROLOGUE_BYTES, "link prologue", false);
        if (!Arrays.equals(Arrays.copyOf(peer, 4), WireConstants.LINK_MAGIC)) {
            throw new VoxException("bad vox link magic");
        }
        if (Byte.toUnsignedInt(peer[4]) != WireConstants.LINK_VERSION) {
            throw new VoxException("unsupported vox link version " + Byte.toUnsignedInt(peer[4]));
        }
        if ((Byte.toUnsignedInt(peer[5]) & WireConstants.LINK_FLAG_FD_CAPABLE) != 0) {
            throw new VoxException("vox link fd-capability mismatch: peer=true, local=false");
        }
        if ((Byte.toUnsignedInt(peer[5]) & ~WireConstants.LINK_FLAG_FD_CAPABLE) != 0) {
            throw new VoxException("unsupported vox link flags " + Byte.toUnsignedInt(peer[5]));
        }
        linkReady = true;
    }

    public void writeFrame(byte[] payload) throws IOException {
        if (!linkReady) throw new IllegalStateException("link prologue not exchanged");
        if (payload.length > maxFrameBytes) {
            throw new IOException(
                    "frame length " + payload.length + " exceeds bound " + maxFrameBytes);
        }
        int length = payload.length;
        output.write(length & 0xff);
        output.write((length >>> 8) & 0xff);
        output.write((length >>> 16) & 0xff);
        output.write((length >>> 24) & 0xff);
        output.write(payload);
        output.flush();
    }

    /** Returns null only for clean EOF before any frame-prefix byte. */
    public byte[] readFrame() throws IOException, VoxException {
        if (!linkReady) throw new IllegalStateException("link prologue not exchanged");
        if (eof) return null;
        byte[] prefix = readExactly(4, "frame header", true);
        if (prefix == null) {
            eof = true;
            return null;
        }
        long length = Integer.toUnsignedLong(
                Byte.toUnsignedInt(prefix[0])
                        | Byte.toUnsignedInt(prefix[1]) << 8
                        | Byte.toUnsignedInt(prefix[2]) << 16
                        | Byte.toUnsignedInt(prefix[3]) << 24);
        if (length > maxFrameBytes) {
            throw new VoxException(
                    "frame length " + length + " exceeds bound " + maxFrameBytes);
        }
        // The bound is checked before this allocation.
        return readExactly((int) length, "frame body", false);
    }

    private byte[] readExactly(int length, String part, boolean cleanEofAllowed)
            throws IOException {
        byte[] bytes = new byte[length];
        int read = 0;
        while (read < length) {
            int count = input.read(bytes, read, length - read);
            if (count < 0) {
                if (read == 0 && cleanEofAllowed) return null;
                throw new EOFException(
                        "stream ended after " + read + " of " + length + " " + part + " bytes");
            }
            read += count;
        }
        return bytes;
    }
}
