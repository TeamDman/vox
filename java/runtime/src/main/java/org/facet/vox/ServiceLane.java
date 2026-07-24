package org.facet.vox;

import java.util.Map;
import java.util.Objects;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.ScheduledFuture;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.TimeoutException;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicLong;
import java.util.concurrent.atomic.AtomicReference;

public final class ServiceLane implements AutoCloseable {
    interface DriverCommands {
        boolean submit(OutboundCall call);
        void cancel(long laneId, long requestId);
        void closeLane(long laneId);
    }

    static final class OutboundCall {
        final long laneId;
        final long requestId;
        final MethodDescriptor method;
        final byte[] arguments;
        final CallOptions options;
        private final ServiceLane owner;
        // 0 = queued, 1 = committed to the wire owner, 2 = cancelled before commitment.
        private final java.util.concurrent.atomic.AtomicInteger commitment =
                new java.util.concurrent.atomic.AtomicInteger();

        OutboundCall(
                ServiceLane owner,
                long laneId,
                long requestId,
                MethodDescriptor method,
                byte[] arguments,
                CallOptions options) {
            this.owner = owner;
            this.laneId = laneId;
            this.requestId = requestId;
            this.method = method;
            this.arguments = arguments;
            this.options = options;
        }

        boolean tryCommit() {
            if (commitment.compareAndSet(0, 1)) {
                owner.committed(this);
                return true;
            }
            return false;
        }

        boolean cancelBeforeCommit() { return commitment.compareAndSet(0, 2); }
        boolean isCommitted() { return commitment.get() == 1; }
        void succeed(byte[] response) { owner.succeed(requestId, response); }
        void fail(Throwable failure) { owner.fail(requestId, failure); }
    }

    private static final class PendingFuture extends CompletableFuture<byte[]> {
        private final ServiceLane lane;
        private final long requestId;

        PendingFuture(ServiceLane lane, long requestId) {
            this.lane = lane;
            this.requestId = requestId;
        }

        @Override
        public boolean cancel(boolean mayInterruptIfRunning) {
            boolean cancelled = super.cancel(false);
            if (cancelled) lane.cancel(requestId);
            return cancelled;
        }
    }

    private static final class Pending {
        final PendingFuture future;
        final OutboundCall call;
        volatile ScheduledFuture<?> timeout;

        Pending(PendingFuture future, OutboundCall call) {
            this.future = future;
            this.call = call;
        }
    }

    private final long id;
    private final ServiceDescriptor service;
    private final DriverCommands driver;
    private final ConnectionOptions connectionOptions;
    private final AtomicReference<LaneState> state;
    private final AtomicLong nextRequestId = new AtomicLong(1);
    private final Map<Long, Pending> pending = new ConcurrentHashMap<>();
    private final CompletableFuture<Void> opened = new CompletableFuture<>();

    ServiceLane(
            long id,
            ServiceDescriptor service,
            DriverCommands driver,
            ConnectionOptions connectionOptions,
            LaneState initialState) {
        this.id = id;
        this.service = service;
        this.driver = driver;
        this.connectionOptions = connectionOptions;
        this.state = new AtomicReference<>(initialState);
    }

    public CompletableFuture<byte[]> call(
            MethodDescriptor method, byte[] encodedArguments, CallOptions options) {
        Objects.requireNonNull(method, "method");
        Objects.requireNonNull(encodedArguments, "encodedArguments");
        Objects.requireNonNull(options, "options");
        if (service.method(method.id()) != method && service.method(method.id()) == null) {
            return CompletableFuture.failedFuture(
                    new VoxException("method does not belong to service " + service.name()));
        }
        if (state.get() != LaneState.OPEN) {
            return CompletableFuture.failedFuture(
                    new VoxException("lane is not open: " + state.get()));
        }
        if (pending.size() >= connectionOptions.maxPendingRequests()) {
            return CompletableFuture.failedFuture(
                    new VoxException("pending request bound exceeded"));
        }
        long requestId = allocateRequestId();
        PendingFuture future = new PendingFuture(this, requestId);
        OutboundCall outbound = new OutboundCall(
                this, id, requestId, method, encodedArguments.clone(), options);
        Pending entry = new Pending(future, outbound);
        pending.put(requestId, entry);
        if (!driver.submit(outbound)) {
            pending.remove(requestId);
            future.completeExceptionally(new VoxException("outbound queue is full"));
        }
        return future;
    }

    public LaneState state() { return state.get(); }
    public CompletableFuture<Void> opened() { return opened; }

    @Override
    public void close() {
        LaneState previous = state.getAndSet(LaneState.CLOSING);
        if (previous == LaneState.CLOSED || previous == LaneState.CLOSING) return;
        driver.closeLane(id);
        terminate(new VoxException("lane closed"), LaneState.CLOSED);
    }

    long id() { return id; }
    ServiceDescriptor service() { return service; }

    void markOpen() {
        if (state.compareAndSet(LaneState.OPENING, LaneState.OPEN)) {
            opened.complete(null);
        }
    }

    void terminate(Throwable failure, LaneState terminalState) {
        state.set(terminalState);
        opened.completeExceptionally(failure);
        for (Map.Entry<Long, Pending> item : pending.entrySet()) {
            Pending removed = pending.remove(item.getKey());
            if (removed != null) {
                cancelTimeout(removed);
                removed.future.completeExceptionally(failure);
            }
        }
    }

    private long allocateRequestId() {
        for (;;) {
            long candidate = nextRequestId.getAndIncrement();
            if (candidate != 0 && !pending.containsKey(candidate)) return candidate;
        }
    }

    private void committed(OutboundCall call) {
        Pending entry = pending.get(call.requestId);
        if (entry == null || entry.future.isDone()) return;
        entry.timeout = connectionOptions.scheduler().schedule(
                () -> timeout(call.requestId),
                call.options.idleTimeout().toNanos(),
                TimeUnit.NANOSECONDS);
    }

    private void timeout(long requestId) {
        Pending removed = pending.remove(requestId);
        if (removed == null) return;
        removed.future.completeExceptionally(
                new TimeoutException("vox request " + Long.toUnsignedString(requestId)
                        + " exceeded idle timeout"));
        if (removed.call.isCommitted()) {
            driver.cancel(id, requestId);
        }
    }

    private void cancel(long requestId) {
        Pending removed = pending.remove(requestId);
        if (removed == null) return;
        cancelTimeout(removed);
        if (!removed.call.cancelBeforeCommit()) {
            driver.cancel(id, requestId);
        }
    }

    private void succeed(long requestId, byte[] response) {
        Pending removed = pending.remove(requestId);
        if (removed == null) return; // Late response after cancellation/timeout.
        cancelTimeout(removed);
        removed.future.complete(response.clone());
    }

    private void fail(long requestId, Throwable failure) {
        Pending removed = pending.remove(requestId);
        if (removed == null) return;
        cancelTimeout(removed);
        removed.future.completeExceptionally(failure);
    }

    private static void cancelTimeout(Pending pending) {
        ScheduledFuture<?> timeout = pending.timeout;
        if (timeout != null) timeout.cancel(false);
    }
}
