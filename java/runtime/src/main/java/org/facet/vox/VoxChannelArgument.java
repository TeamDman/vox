package org.facet.vox;

import java.util.Objects;

/** One generated direct channel argument supplied alongside encoded request arguments. */
public final class VoxChannelArgument {
    private final ChannelDescriptor descriptor;
    private final Object endpoint;

    private VoxChannelArgument(ChannelDescriptor descriptor, Object endpoint) {
        this.descriptor = Objects.requireNonNull(descriptor, "descriptor");
        this.endpoint = Objects.requireNonNull(endpoint, "endpoint");
    }

    public static VoxChannelArgument tx(ChannelDescriptor descriptor, VoxTx<?> endpoint) {
        if (descriptor.direction() != ChannelDescriptor.Direction.TX) {
            throw new IllegalArgumentException("descriptor is not Tx");
        }
        return new VoxChannelArgument(descriptor, endpoint);
    }

    public static VoxChannelArgument rx(ChannelDescriptor descriptor, VoxRx<?> endpoint) {
        if (descriptor.direction() != ChannelDescriptor.Direction.RX) {
            throw new IllegalArgumentException("descriptor is not Rx");
        }
        return new VoxChannelArgument(descriptor, endpoint);
    }

    ChannelDescriptor descriptor() { return descriptor; }
    Object endpoint() { return endpoint; }
}
