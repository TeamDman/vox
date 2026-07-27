package org.facet.vox.tcp;

import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.EOFException;
import java.util.Arrays;
import org.facet.vox.VoxException;

public final class StreamFramingTest {
    public static void main(String[] args) throws Exception {
        exactLinkAndFrameBytes();
        oversizedFrameRejectedBeforeAllocation();
        truncatedHeaderAndBodyAreDistinct();
        transportVectors();
        System.out.println("StreamFramingTest: PASS");
    }

    private static void exactLinkAndFrameBytes() throws Exception {
        byte[] input = concat(
                new byte[] {'V', 'O', 'X', 'L', 1, 0},
                new byte[] {3, 0, 0, 0, 7, 8, 9});
        ByteArrayOutputStream output = new ByteArrayOutputStream();
        StreamFraming framing =
                new StreamFraming(new ByteArrayInputStream(input), output, 32);
        framing.exchangeLinkPrologue();
        check(Arrays.equals(framing.readFrame(), new byte[] {7, 8, 9}), "frame body");
        check(Arrays.equals(output.toByteArray(),
                new byte[] {'V', 'O', 'X', 'L', 1, 0}), "link prologue bytes");
    }

    private static void oversizedFrameRejectedBeforeAllocation() throws Exception {
        byte[] input = concat(
                new byte[] {'V', 'O', 'X', 'L', 1, 0},
                new byte[] {33, 0, 0, 0});
        StreamFraming framing = new StreamFraming(
                new ByteArrayInputStream(input), new ByteArrayOutputStream(), 32);
        framing.exchangeLinkPrologue();
        VoxException failure = expectThrows(VoxException.class, framing::readFrame);
        check(failure.getMessage().contains("exceeds bound"), "bounded before body allocation");
    }

    private static void truncatedHeaderAndBodyAreDistinct() throws Exception {
        StreamFraming header = framingWith(new byte[] {2, 0});
        EOFException headerFailure = expectThrows(EOFException.class, header::readFrame);
        check(headerFailure.getMessage().contains("frame header"), "header truncation");

        StreamFraming body = framingWith(new byte[] {4, 0, 0, 0, 1});
        EOFException bodyFailure = expectThrows(EOFException.class, body::readFrame);
        check(bodyFailure.getMessage().contains("frame body"), "body truncation");
    }

    private static void transportVectors() throws Exception {
        byte[] acceptFrame = framed(new byte[] {'V', 'O', 'T', 'A', 9, 0, 0, 0});
        ByteArrayOutputStream initiatingOutput = new ByteArrayOutputStream();
        StreamFraming initiator = new StreamFraming(
                new ByteArrayInputStream(concat(link(), acceptFrame)), initiatingOutput, 64);
        initiator.exchangeLinkPrologue();
        TransportPrologue.initiate(initiator);
        check(Arrays.equals(initiatingOutput.toByteArray(), concat(
                link(), framed(new byte[] {'V', 'O', 'T', 'H', 9, 0, 0, 0}))),
                "TransportHello vector");

        ByteArrayOutputStream acceptingOutput = new ByteArrayOutputStream();
        StreamFraming acceptor = new StreamFraming(
                new ByteArrayInputStream(concat(
                        link(), framed(new byte[] {'V', 'O', 'T', 'H', 9, 0, 0, 0}))),
                acceptingOutput, 64);
        acceptor.exchangeLinkPrologue();
        TransportPrologue.accept(acceptor);
        check(Arrays.equals(acceptingOutput.toByteArray(), concat(link(), acceptFrame)),
                "TransportAccept vector");
    }

    private static StreamFraming framingWith(byte[] afterPrologue) throws Exception {
        StreamFraming framing = new StreamFraming(
                new ByteArrayInputStream(concat(link(), afterPrologue)),
                new ByteArrayOutputStream(), 32);
        framing.exchangeLinkPrologue();
        return framing;
    }

    private static byte[] link() {
        return new byte[] {'V', 'O', 'X', 'L', 1, 0};
    }

    private static byte[] framed(byte[] payload) {
        return concat(new byte[] {(byte) payload.length, 0, 0, 0}, payload);
    }

    private static byte[] concat(byte[]... arrays) {
        int length = 0;
        for (byte[] array : arrays) length += array.length;
        byte[] result = new byte[length];
        int offset = 0;
        for (byte[] array : arrays) {
            System.arraycopy(array, 0, result, offset, array.length);
            offset += array.length;
        }
        return result;
    }

    private static <T extends Throwable> T expectThrows(
            Class<T> type, ThrowingRunnable runnable) {
        try {
            runnable.run();
        } catch (Throwable failure) {
            if (type.isInstance(failure)) return type.cast(failure);
            throw new AssertionError("expected " + type.getName() + ", got " + failure, failure);
        }
        throw new AssertionError("expected " + type.getName());
    }

    private static void check(boolean condition, String description) {
        if (!condition) throw new AssertionError(description);
    }

    private interface ThrowingRunnable {
        void run() throws Exception;
    }
}
