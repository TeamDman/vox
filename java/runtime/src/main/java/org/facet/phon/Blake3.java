package org.facet.phon;

import java.util.ArrayDeque;
import java.util.Arrays;

/** Minimal unkeyed BLAKE3 implementation used for dependency-free schema identity. */
final class Blake3 {
    private static final int[] IV = {
            0x6A09E667, 0xBB67AE85, 0x3C6EF372, 0xA54FF53A,
            0x510E527F, 0x9B05688C, 0x1F83D9AB, 0x5BE0CD19
    };
    private static final int[] PERMUTATION = {2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8};
    private static final int CHUNK_START = 1, CHUNK_END = 2, PARENT = 4, ROOT = 8;
    private Blake3() {}

    static byte[] hash(byte[] input) {
        int chunks = Math.max(1, (input.length + 1023) / 1024);
        ArrayDeque<int[]> stack = new ArrayDeque<>();
        for (int chunk = 0; chunk < chunks - 1; chunk++) {
            Output output = chunkOutput(input, chunk * 1024, 1024, chunk);
            int[] cv = output.chainingValue();
            long totalChunks = chunk + 1L;
            while ((totalChunks & 1) == 0) {
                cv = parentOutput(stack.removeLast(), cv).chainingValue();
                totalChunks >>>= 1;
            }
            stack.addLast(cv);
        }
        int lastOffset = (chunks - 1) * 1024;
        Output output = chunkOutput(input, lastOffset, input.length - lastOffset, chunks - 1L);
        while (!stack.isEmpty()) output = parentOutput(stack.removeLast(), output.chainingValue());
        return output.rootBytes();
    }

    private static Output chunkOutput(byte[] input, int offset, int length, long chunkCounter) {
        int[] cv = IV.clone();
        int consumed = 0, blockIndex = 0;
        while (length - consumed > 64) {
            int flags = blockIndex == 0 ? CHUNK_START : 0;
            cv = first8(compress(cv, words(input, offset + consumed, 64), chunkCounter, 64, flags));
            consumed += 64; blockIndex++;
        }
        int remaining = length - consumed;
        int flags = CHUNK_END | (blockIndex == 0 ? CHUNK_START : 0);
        return new Output(cv, words(input, offset + consumed, remaining), chunkCounter, remaining, flags);
    }

    private static Output parentOutput(int[] left, int[] right) {
        int[] block = new int[16];
        System.arraycopy(left, 0, block, 0, 8);
        System.arraycopy(right, 0, block, 8, 8);
        return new Output(IV, block, 0, 64, PARENT);
    }

    private static int[] words(byte[] input, int offset, int length) {
        int[] words = new int[16];
        for (int i = 0; i < length; i++) words[i >>> 2] |= (input[offset + i] & 0xff) << (8 * (i & 3));
        return words;
    }

    private static int[] first8(int[] words) { return Arrays.copyOf(words, 8); }
    private static int[] compress(int[] cv, int[] block, long counter, int blockLen, int flags) {
        int[] state = new int[16];
        System.arraycopy(cv, 0, state, 0, 8);
        System.arraycopy(IV, 0, state, 8, 4);
        state[12] = (int) counter; state[13] = (int) (counter >>> 32);
        state[14] = blockLen; state[15] = flags;
        int[] message = block.clone();
        for (int round = 0; round < 7; round++) {
            round(state, message);
            int[] permuted = new int[16];
            for (int i = 0; i < 16; i++) permuted[i] = message[PERMUTATION[i]];
            message = permuted;
        }
        for (int i = 0; i < 8; i++) {
            state[i] ^= state[i + 8];
            state[i + 8] ^= cv[i];
        }
        return state;
    }

    private static void round(int[] s, int[] m) {
        g(s, 0, 4, 8, 12, m[0], m[1]); g(s, 1, 5, 9, 13, m[2], m[3]);
        g(s, 2, 6, 10, 14, m[4], m[5]); g(s, 3, 7, 11, 15, m[6], m[7]);
        g(s, 0, 5, 10, 15, m[8], m[9]); g(s, 1, 6, 11, 12, m[10], m[11]);
        g(s, 2, 7, 8, 13, m[12], m[13]); g(s, 3, 4, 9, 14, m[14], m[15]);
    }
    private static void g(int[] s, int a, int b, int c, int d, int mx, int my) {
        s[a] = s[a] + s[b] + mx; s[d] = Integer.rotateRight(s[d] ^ s[a], 16);
        s[c] += s[d]; s[b] = Integer.rotateRight(s[b] ^ s[c], 12);
        s[a] = s[a] + s[b] + my; s[d] = Integer.rotateRight(s[d] ^ s[a], 8);
        s[c] += s[d]; s[b] = Integer.rotateRight(s[b] ^ s[c], 7);
    }
    private static final class Output {
        private final int[] cv, block; private final long counter; private final int blockLen, flags;
        private Output(int[] cv, int[] block, long counter, int blockLen, int flags) {
            this.cv = cv; this.block = block; this.counter = counter; this.blockLen = blockLen; this.flags = flags;
        }
        int[] chainingValue() { return first8(compress(cv, block, counter, blockLen, flags)); }
        byte[] rootBytes() {
            int[] words = compress(cv, block, 0, blockLen, flags | ROOT);
            byte[] bytes = new byte[32];
            for (int i = 0; i < 8; i++) for (int j = 0; j < 4; j++) bytes[i * 4 + j] = (byte) (words[i] >>> (8 * j));
            return bytes;
        }
    }
}
