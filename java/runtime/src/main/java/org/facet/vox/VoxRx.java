package org.facet.vox;

import java.time.Duration;
import java.util.Objects;
import java.util.concurrent.TimeoutException;

/** The receiving half of a typed request-scoped Vox channel. */
public final class VoxRx<T> implements AutoCloseable {
    private final ChannelRuntime.Core<T> core;

    VoxRx(ChannelRuntime.Core<T> core) {
        this.core = Objects.requireNonNull(core, "core");
    }

    /** Returns the next item, or {@code null} after a graceful remote close. */
    public T receive() throws InterruptedException, VoxException {
        try {
            return core.receiver().receive(null);
        } catch (ChannelRuntime.ReceiveTimeout impossible) {
            throw new AssertionError(impossible);
        }
    }

    /** Returns the next item, or {@code null} after a graceful remote close. */
    public T receive(Duration timeout) throws InterruptedException, VoxException, TimeoutException {
        Objects.requireNonNull(timeout, "timeout");
        if (timeout.isNegative() || timeout.isZero()) {
            throw new IllegalArgumentException("timeout must be positive");
        }
        try {
            return core.receiver().receive(timeout);
        } catch (ChannelRuntime.ReceiveTimeout timeoutFailure) {
            throw new TimeoutException(timeoutFailure.getMessage());
        }
    }

    public boolean isBound() { return core.hasReceiver(); }

    /** Resets the receive side and asks the peer to stop sending. */
    @Override public void close() { core.resetReceiver(); }

    ChannelRuntime.Core<T> core() { return core; }
}
