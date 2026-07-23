package org.facet.vox;

import java.io.IOException;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.util.ArrayList;
import java.util.List;
import java.util.Objects;
import java.util.concurrent.ArrayBlockingQueue;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.Executor;
import java.util.concurrent.RejectedExecutionException;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicLong;
import java.util.concurrent.atomic.AtomicReference;
import org.facet.vox.tcp.StreamFraming;
import org.facet.vox.tcp.TransportPrologue;

/**
 * One explicitly-driven Vox TCP connection.
 *
 * <p>The stream and transport prologues are interoperable now. The subsequent self-describing
 * Phon handshake is an intentional integration seam until the Phon Java track lands.
 */
public final class VoxConnection implements AutoCloseable, ServiceLane.DriverCommands {
    private interface DriverCommand {}
    private static final class CallCommand implements DriverCommand {
        final ServiceLane.OutboundCall call;
        CallCommand(ServiceLane.OutboundCall call) { this.call = call; }
    }
    private static final class CancelCommand implements DriverCommand {
        final long laneId;
        final long requestId;
        CancelCommand(long laneId, long requestId) {
            this.laneId = laneId;
            this.requestId = requestId;
        }
    }
    private static final class CloseLaneCommand implements DriverCommand {
        final long laneId;
        CloseLaneCommand(long laneId) { this.laneId = laneId; }
    }

    private final Socket socket;
    private final boolean initiator;
    private final ServiceRegistry services;
    private final ConnectionOptions options;
    private final ArrayBlockingQueue<DriverCommand> commands;
    private final AtomicInteger queuedBytes = new AtomicInteger();
    private final AtomicBoolean driverOwned = new AtomicBoolean();
    private final AtomicReference<ConnectionState> state =
            new AtomicReference<>(ConnectionState.NEW);
    private final AtomicLong nextLaneId;
    private final List<ServiceLane> lanes = new ArrayList<>();
    private final CompletableFuture<Void> closed = new CompletableFuture<>();

    private VoxConnection(
            Socket socket,
            boolean initiator,
            ServiceRegistry services,
            ConnectionOptions options) {
        this.socket = Objects.requireNonNull(socket, "socket");
        this.initiator = initiator;
        this.services = Objects.requireNonNull(services, "services");
        this.options = Objects.requireNonNull(options, "options");
        commands = new ArrayBlockingQueue<>(options.maxQueuedOutboundMessages());
        // Match the Rust parity convention: the initiator starts with an odd lane id.
        nextLaneId = new AtomicLong(initiator ? 1 : 2);
    }

    public static VoxConnection connect(
            InetSocketAddress address, ConnectionOptions options)
            throws IOException, VoxException {
        Objects.requireNonNull(address, "address");
        Objects.requireNonNull(options, "options");
        Socket socket = new Socket();
        socket.connect(address, durationMillis(options.handshakeTimeout()));
        socket.setTcpNoDelay(true);
        return new VoxConnection(socket, true, new ServiceRegistry(), options);
    }

    public static VoxConnection accept(
            Socket socket, ServiceRegistry services, ConnectionOptions options)
            throws IOException, VoxException {
        Objects.requireNonNull(socket, "socket");
        socket.setTcpNoDelay(true);
        return new VoxConnection(socket, false, services, options);
    }

    public void drive() throws IOException, VoxException {
        if (!driverOwned.compareAndSet(false, true)) {
            throw new IllegalStateException("VoxConnection already has a driver owner");
        }
        driveOwned();
    }

    public CompletableFuture<Void> start(Executor executor) {
        Objects.requireNonNull(executor, "executor");
        if (!driverOwned.compareAndSet(false, true)) {
            return CompletableFuture.failedFuture(
                    new IllegalStateException("VoxConnection already has a driver owner"));
        }
        try {
            executor.execute(() -> {
                try {
                    driveOwned();
                } catch (IOException | VoxException ignored) {
                    // The exact failure is already carried by closed().
                }
            });
        } catch (RejectedExecutionException failure) {
            fail(failure);
        }
        return closed;
    }

    public ServiceLane openLane(ServiceDescriptor service, LaneOptions laneOptions) {
        Objects.requireNonNull(service, "service");
        Objects.requireNonNull(laneOptions, "laneOptions");
        if (state.get() != ConnectionState.OPEN) {
            throw new IllegalStateException("connection is not open: " + state.get());
        }
        synchronized (lanes) {
            if (lanes.size() >= options.maxOpenLanes()) {
                throw new IllegalStateException("open lane bound exceeded");
            }
            long id = nextLaneId.getAndAdd(2);
            ServiceLane lane = new ServiceLane(
                    id, service, this, options, LaneState.OPENING);
            lanes.add(lane);
            // LaneOpen encoding is owned by the post-handshake message codec integration.
            return lane;
        }
    }

    public ConnectionState state() { return state.get(); }
    public CompletableFuture<Void> closed() { return closed; }

    @Override
    public void close() {
        ConnectionState current = state.getAndSet(ConnectionState.CLOSING);
        if (current == ConnectionState.CLOSED || current == ConnectionState.CLOSING) return;
        try {
            socket.close();
        } catch (IOException ignored) {
            // close remains idempotent
        }
        terminateLanes(new VoxException("connection closed"), LaneState.CLOSED);
        state.set(ConnectionState.CLOSED);
        closed.complete(null);
        options.closeOwnedResources();
    }

    @Override
    public boolean submit(ServiceLane.OutboundCall call) {
        int bytes = call.arguments.length;
        for (;;) {
            int current = queuedBytes.get();
            if (bytes > options.maxQueuedOutboundBytes() - current) return false;
            if (queuedBytes.compareAndSet(current, current + bytes)) break;
        }
        if (!commands.offer(new CallCommand(call))) {
            queuedBytes.addAndGet(-bytes);
            return false;
        }
        return true;
    }

    @Override
    public void cancel(long laneId, long requestId) {
        commands.offer(new CancelCommand(laneId, requestId));
    }

    @Override
    public void closeLane(long laneId) {
        commands.offer(new CloseLaneCommand(laneId));
    }

    private void driveOwned() throws IOException, VoxException {
        try {
            if (!state.compareAndSet(ConnectionState.NEW, ConnectionState.TRANSPORT_NEGOTIATING)) {
                throw new IllegalStateException("connection cannot be driven from " + state.get());
            }
            socket.setSoTimeout(durationMillis(options.handshakeTimeout()));
            StreamFraming framing =
                    new StreamFraming(socket.getInputStream(), socket.getOutputStream(),
                            options.maxFrameBytes());
            framing.exchangeLinkPrologue();
            if (initiator) {
                TransportPrologue.initiate(framing);
            } else {
                TransportPrologue.accept(framing);
            }
            state.set(ConnectionState.HANDSHAKING);
            establishSelfDescribingHandshake(framing);
            state.set(ConnectionState.OPEN);
            socket.setSoTimeout(0);
            runOpenDriver(framing);
            close();
        } catch (IOException | VoxException | RuntimeException failure) {
            fail(failure);
            if (failure instanceof IOException io) throw io;
            if (failure instanceof VoxException vox) throw vox;
            throw failure;
        }
    }

    private void establishSelfDescribingHandshake(StreamFraming framing) throws VoxException {
        throw new VoxException(
                "Phon self-describing connection handshake is not integrated: "
                        + "wire schema/adapters from the Phon Java track are required");
    }

    private void runOpenDriver(StreamFraming framing) throws IOException, VoxException {
        // This loop is deliberately unreachable until establishSelfDescribingHandshake is
        // implemented. Keeping the queue/state ownership here makes the integration boundary
        // explicit: only this driver will encode messages or mutate protocol state.
        while (state.get() == ConnectionState.OPEN) {
            DriverCommand command;
            try {
                command = commands.take();
            } catch (InterruptedException interrupted) {
                Thread.currentThread().interrupt();
                throw new VoxException("connection driver interrupted", interrupted);
            }
            if (command instanceof CallCommand call) {
                queuedBytes.addAndGet(-call.call.arguments.length);
                if (!call.call.tryCommit()) {
                    continue;
                }
                throw new VoxException("post-handshake RequestCall codec is not integrated");
            } else if (command instanceof CloseLaneCommand) {
                // LaneClose encoding joins this state machine with the generated message codec.
            } else if (command instanceof CancelCommand) {
                // RequestCancel encoding joins this state machine with the generated message codec.
            }
        }
    }

    private void fail(Throwable failure) {
        state.set(ConnectionState.FAILED);
        try {
            socket.close();
        } catch (IOException ignored) {
            // Preserve the primary failure.
        }
        terminateQueuedCalls(failure);
        terminateLanes(failure, LaneState.FAILED);
        closed.completeExceptionally(failure);
        options.closeOwnedResources();
    }

    private void terminateQueuedCalls(Throwable failure) {
        DriverCommand command;
        while ((command = commands.poll()) != null) {
            if (command instanceof CallCommand call) {
                queuedBytes.addAndGet(-call.call.arguments.length);
                call.call.fail(failure);
            }
        }
    }

    private void terminateLanes(Throwable failure, LaneState terminal) {
        synchronized (lanes) {
            for (ServiceLane lane : lanes) {
                lane.terminate(failure, terminal);
            }
            lanes.clear();
        }
    }

    private static int durationMillis(java.time.Duration duration) {
        long millis = Math.max(1, duration.toMillis());
        return (int) Math.min(Integer.MAX_VALUE, millis);
    }
}
