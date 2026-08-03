package org.facet.vox;

import java.util.Objects;

/** The sending half of a typed request-scoped Vox channel. */
public final class VoxTx<T> implements AutoCloseable {
    private final ChannelRuntime.Core<T> core;

    VoxTx(ChannelRuntime.Core<T> core) {
        this.core = Objects.requireNonNull(core, "core");
    }

    /** Reliably enqueue one item, blocking for channel credit and bounded runtime capacity. */
    public void send(T value) throws InterruptedException, VoxException {
        core.sender().send(value);
    }

    /** Nonblocking send that preserves ownership of a value that could not be accepted. */
    public TrySendResult<T> trySend(T value) throws VoxException {
        ChannelRuntime.Sender<T> sender = core.trySender();
        return sender == null ? new TrySendResult.Full<>(value) : sender.trySend(value);
    }

    public boolean isBound() { return core.hasSender(); }

    /** Gracefully closes this sender after previously accepted items. */
    @Override public void close() { core.closeSender(); }

    ChannelRuntime.Core<T> core() { return core; }

    public sealed interface TrySendResult<T>
            permits TrySendResult.Sent, TrySendResult.Full, TrySendResult.Closed {
        record Sent<T>() implements TrySendResult<T> {}
        record Full<T>(T value) implements TrySendResult<T> {}
        record Closed<T>(T value) implements TrySendResult<T> {}
    }
}
