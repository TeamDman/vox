package org.facet.vox;

import java.io.IOException;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.net.SocketTimeoutException;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Objects;
import java.util.Set;
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
import org.facet.phon.SchemaClosure;
import org.facet.phon.Value;

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
    private static final class OpenLaneCommand implements DriverCommand {
        final ServiceLane lane;
        OpenLaneCommand(ServiceLane lane) { this.lane = lane; }
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
    private final Map<String, ServiceLane.OutboundCall> inFlight = new HashMap<>();
    private final Map<String, SchemaClosure> receivedBindings = new HashMap<>();
    private final Set<String> sentBindings = new HashSet<>();
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
            if (!commands.offer(new OpenLaneCommand(lane))) {
                lanes.remove(lane);
                lane.terminate(new VoxException("outbound queue is full"), LaneState.FAILED);
            }
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
            WireCodec codec = new WireCodec(options);
            establishSelfDescribingHandshake(framing, codec);
            state.set(ConnectionState.OPEN);
            dlog("connection open");
            socket.setSoTimeout(25);
            runOpenDriver(framing, codec);
            close();
        } catch (IOException | VoxException | RuntimeException failure) {
            fail(failure);
            if (failure instanceof IOException io) throw io;
            if (failure instanceof VoxException vox) throw vox;
            throw failure;
        }
    }

    private void establishSelfDescribingHandshake(
            StreamFraming framing, WireCodec codec) throws IOException, VoxException {
        if (initiator) {
            framing.writeFrame(codec.encodeHello(true));
            Value response = codec.decodeHandshake(requireFrame(framing, "HelloYourself"));
            String variant = WireCodec.variant(response);
            if ("Decline".equals(variant) || "Sorry".equals(variant)) {
                throw new VoxException("peer rejected Vox handshake with " + variant);
            }
            if (!"HelloYourself".equals(variant)) {
                throw new VoxException("expected HelloYourself, got " + variant);
            }
            Value helloYourself = WireCodec.variantPayload(response);
            validateSettings(WireCodec.required(helloYourself, "connection_settings"));
            codec.bindPeerMessageSchema(
                    WireCodec.byteList(
                            WireCodec.required(helloYourself, "message_payload_schema")));
            framing.writeFrame(codec.encodeLetsGo());
        } else {
            Value request = codec.decodeHandshake(requireFrame(framing, "Hello"));
            if (!"Hello".equals(WireCodec.variant(request))) {
                throw new VoxException("expected Hello, got " + WireCodec.variant(request));
            }
            Value hello = WireCodec.variantPayload(request);
            validateSettings(WireCodec.required(hello, "connection_settings"));
            codec.bindPeerMessageSchema(
                    WireCodec.byteList(
                            WireCodec.required(hello, "message_payload_schema")));
            framing.writeFrame(codec.encodeHelloYourself(false));
            Value confirmation = codec.decodeHandshake(requireFrame(framing, "LetsGo"));
            if (!"LetsGo".equals(WireCodec.variant(confirmation))) {
                throw new VoxException("expected LetsGo, got " + WireCodec.variant(confirmation));
            }
        }
    }

    private void runOpenDriver(StreamFraming framing, WireCodec codec)
            throws IOException, VoxException {
        while (state.get() == ConnectionState.OPEN) {
            DriverCommand command = commands.poll();
            while (command != null) {
                processCommand(command, framing, codec);
                command = commands.poll();
            }
            try {
                byte[] frame = framing.readFrame();
                if (frame == null) return;
                processInbound(codec.decodeMessage(frame), framing, codec);
            } catch (SocketTimeoutException timeout) {
                // The short read timeout lets this single owner service outbound commands.
            }
        }
    }

    private void processCommand(
            DriverCommand command, StreamFraming framing, WireCodec codec)
            throws IOException, VoxException {
        if (command instanceof OpenLaneCommand open) {
            dlog("send LaneOpen lane=" + Long.toUnsignedString(open.lane.id()));
            framing.writeFrame(codec.encodeMessage(
                    open.lane.id(),
                    codec.laneOpen(open.lane.service().name(), initiator)));
        } else if (command instanceof CallCommand call) {
            queuedBytes.addAndGet(-call.call.arguments.length);
            if (!call.call.tryCommit()) return;
            String binding = bindingKey(call.call.method.id(), WireCodec.Direction.ARGS);
            if (sentBindings.add(binding)) {
                dlog("send Args SchemaMessage method="
                        + Long.toUnsignedString(call.call.method.id()));
                byte[] schemas;
                try {
                    schemas = call.call.method.argumentAdapter().schema().bundleBytes();
                } catch (org.facet.phon.PhonException failure) {
                    call.call.fail(failure);
                    return;
                }
                framing.writeFrame(codec.encodeMessage(
                        call.call.laneId,
                        codec.schemaMessage(
                                call.call.method.id(), WireCodec.Direction.ARGS, schemas)));
            }
            inFlight.put(requestKey(call.call.laneId, call.call.requestId), call.call);
            dlog("send RequestCall lane=" + Long.toUnsignedString(call.call.laneId)
                    + " request=" + Long.toUnsignedString(call.call.requestId));
            framing.writeFrame(codec.encodeMessage(
                    call.call.laneId,
                    codec.requestCall(
                            call.call.requestId, call.call.method.id(), call.call.arguments)));
        } else if (command instanceof CloseLaneCommand close) {
            framing.writeFrame(codec.encodeMessage(close.laneId, codec.laneClose()));
        } else if (command instanceof CancelCommand cancel) {
            framing.writeFrame(codec.encodeMessage(
                    cancel.laneId, codec.requestCancel(cancel.requestId)));
        }
    }

    private void processInbound(
            Value message, StreamFraming framing, WireCodec codec)
            throws IOException, VoxException {
        long laneId = WireCodec.laneId(message);
        Value payload = WireCodec.payload(message);
        String variant = WireCodec.variant(payload);
        dlog("recv " + variant + " lane=" + Long.toUnsignedString(laneId));
        Value body = WireCodec.variantPayload(payload);
        switch (variant) {
            case "LaneAccept" -> requireLane(laneId).markOpen();
            case "LaneReject" -> requireLane(laneId).terminate(
                    new VoxException("peer rejected service lane"), LaneState.FAILED);
            case "SchemaMessage" -> {
                long methodId = WireCodec.unsignedLong(
                        WireCodec.required(body, "method_id"));
                String directionVariant = WireCodec.variant(
                        WireCodec.required(body, "direction"));
                WireCodec.Direction direction = switch (directionVariant) {
                    case "Args" -> WireCodec.Direction.ARGS;
                    case "Response" -> WireCodec.Direction.RESPONSE;
                    default -> throw new VoxException(
                            "unsupported binding direction " + directionVariant);
                };
                receivedBindings.put(
                        bindingKey(methodId, direction),
                        codec.parseBinding(WireCodec.byteList(
                                WireCodec.required(body, "schemas"))));
            }
            case "RequestMessage" -> processInboundRequest(laneId, body, codec);
            case "LaneClose" -> requireLane(laneId).terminate(
                    new VoxException("peer closed service lane"), LaneState.CLOSED);
            default -> throw new VoxException(
                    "unsupported Java wire message " + variant);
        }
    }

    private void processInboundRequest(long laneId, Value request, WireCodec codec)
            throws VoxException {
        long requestId = WireCodec.unsignedLong(WireCodec.required(request, "id"));
        Value requestBody = WireCodec.required(request, "body");
        String variant = WireCodec.variant(requestBody);
        if (!"Response".equals(variant)) {
            throw new VoxException("Java caller slice received unsupported request " + variant);
        }
        ServiceLane.OutboundCall call = inFlight.remove(requestKey(laneId, requestId));
        if (call == null) return; // Late response after cancellation/timeout.
        Value response = WireCodec.variantPayload(requestBody);
        byte[] encoded = WireCodec.required(response, "ret").asBytes();
        SchemaClosure writer = receivedBindings.get(
                bindingKey(call.method.id(), WireCodec.Direction.RESPONSE));
        if (writer == null) {
            call.fail(new VoxException("response arrived before its schema binding"));
            return;
        }
        byte[] local = codec.transcode(
                writer, call.method.responseWireAdapter().schema(), encoded);
        call.succeed(local);
    }

    private ServiceLane requireLane(long laneId) throws VoxException {
        synchronized (lanes) {
            for (ServiceLane lane : lanes) {
                if (lane.id() == laneId) return lane;
            }
        }
        throw new VoxException("message for unknown lane " + Long.toUnsignedString(laneId));
    }

    private static byte[] requireFrame(StreamFraming framing, String part)
            throws IOException, VoxException {
        byte[] frame = framing.readFrame();
        if (frame == null) throw new VoxException("peer closed before " + part);
        return frame;
    }

    private static void validateSettings(Value settings) throws VoxException {
        long credit = WireCodec.unsignedLong(
                WireCodec.required(settings, "initial_channel_credit"));
        if (credit == 0) throw new VoxException("initial_channel_credit must be nonzero");
    }

    private static String bindingKey(long methodId, WireCodec.Direction direction) {
        return Long.toUnsignedString(methodId) + ":" + direction;
    }

    private static String requestKey(long laneId, long requestId) {
        return Long.toUnsignedString(laneId) + ":" + Long.toUnsignedString(requestId);
    }

    private static void dlog(String message) {
        if ("1".equals(System.getenv("VOX_DLOG"))) {
            System.err.println("[vox-java] " + message);
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
