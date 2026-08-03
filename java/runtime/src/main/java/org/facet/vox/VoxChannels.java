package org.facet.vox;

import java.util.Objects;
import org.facet.phon.PhonAdapter;

/** Factory for a linked, initially unbound Vox channel pair. */
public final class VoxChannels {
    public record Pair<T>(VoxTx<T> tx, VoxRx<T> rx) {
        public Pair {
            Objects.requireNonNull(tx, "tx");
            Objects.requireNonNull(rx, "rx");
        }
    }

    public static <T> Pair<T> channel(PhonAdapter<T> adapter) {
        ChannelRuntime.Core<T> core = new ChannelRuntime.Core<>(adapter);
        return new Pair<>(new VoxTx<>(core), new VoxRx<>(core));
    }

    private VoxChannels() {}
}
