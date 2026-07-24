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
import java.util.concurrent.CompletionException;
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
    private static final class ReplyCommand implements DriverCommand {
        final long laneId;
        final long requestId;
        final MethodDescriptor method;
        final byte[] response;
        final VoxException failure;
        ReplyCommand(
                long laneId,
                long requestId,
                MethodDescriptor method,
                byte[] response,
                VoxException failure) {
            this.laneId = laneId;
            this.requestId = requestId;
            this.method = method;
            this.response = response == null ? null : response.clone();
            this.failure = failure;
        }
    }
    private static final class InboundRequest {
        final CallContext context;
        final InboundCall call;
        InboundRequest(CallContext context, InboundCall call) {
            this.context = context;
            this.call = call;
        }
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
    private final AtomicBoolean laneIdsExhausted = new AtomicBoolean();
    private final List<ServiceLane> lanes = new ArrayList<>();
    private final Map<String, ServiceLane.OutboundCall> inFlight = new HashMap<>();
    private final Map<Long, ServiceDispatcher> inboundLanes = new HashMap<>();
    private final Map<String, InboundRequest> inboundRequests = new HashMap<>();
    private final Map<String, SchemaClosure> receivedBindings = new HashMap<>();
    private final Set<String> sentBindings = new HashSet<>();
    private final CompletableFuture<Void> closed = new CompletableFuture<>();
    private volatile int peerMaxConcurrentRequests = 1;

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
        return connect(address, new ServiceRegistry(), options);
    }

    public static VoxConnection connect(
            InetSocketAddress address,
            ServiceRegistry services,
            ConnectionOptions options)
            throws IOException, VoxException {
        Objects.requireNonNull(address, "address");
        Objects.requireNonNull(services, "services");
        Objects.requireNonNull(options, "options");
        Socket socket = new Socket();
        socket.connect(address, durationMillis(options.handshakeTimeout()));
        socket.setTcpNoDelay(true);
        return new VoxConnection(socket, true, services, options);
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
            long id = allocateLaneId();
            ServiceLane lane = new ServiceLane(
                    id,
                    service,
                    this,
                    options,
                    laneOptions,
                    LaneState.OPENING,
                    1,
                    peerMaxConcurrentRequests);
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
        terminateInbound();
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
    public boolean cancel(long laneId, long requestId) {
        if (commands.offer(new CancelCommand(laneId, requestId))) return true;
        fail(new VoxException("outbound control queue is full"));
        return false;
    }

    @Override
    public boolean closeLane(long laneId) {
        if (commands.offer(new CloseLaneCommand(laneId))) return true;
        fail(new VoxException("outbound control queue is full"));
        return false;
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
            peerMaxConcurrentRequests =
                    validatePeerSettings(
                            WireCodec.required(helloYourself, "connection_settings"));
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
            peerMaxConcurrentRequests =
                    validatePeerSettings(WireCodec.required(hello, "connection_settings"));
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
                    codec.laneOpen(
                            open.lane.service().name(),
                            open.lane.options().metadata())));
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
                            call.call.requestId,
                            call.call.method.id(),
                            call.call.arguments,
                            call.call.options.metadata())));
        } else if (command instanceof CloseLaneCommand close) {
            framing.writeFrame(codec.encodeMessage(close.laneId, codec.laneClose()));
        } else if (command instanceof CancelCommand cancel) {
            framing.writeFrame(codec.encodeMessage(
                    cancel.laneId, codec.requestCancel(cancel.requestId)));
        } else if (command instanceof ReplyCommand reply) {
            processReply(reply, framing, codec);
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
            case "LaneOpen" -> processInboundLaneOpen(laneId, body, framing, codec);
            case "LaneAccept" -> requireLane(laneId).markOpen(
                    validatePeerSettings(
                            WireCodec.required(body, "connection_settings"), false));
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
            case "LaneClose" -> processLaneClose(laneId);
            default -> throw new VoxException(
                    "unsupported Java wire message " + variant);
        }
    }

    private void processInboundRequest(long laneId, Value request, WireCodec codec)
            throws VoxException {
        long requestId = WireCodec.unsignedLong(WireCodec.required(request, "id"));
        Value requestBody = WireCodec.required(request, "body");
        String variant = WireCodec.variant(requestBody);
        if ("Call".equals(variant)) {
            processInboundCall(
                    laneId, requestId, WireCodec.variantPayload(requestBody), codec);
            return;
        }
        if ("Cancel".equals(variant)) {
            InboundRequest inbound =
                    inboundRequests.remove(requestKey(laneId, requestId));
            if (inbound != null) inbound.context.cancel();
            return;
        }
        if (!"Response".equals(variant)) {
            throw new VoxException("unsupported inbound request " + variant);
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

    private void processInboundLaneOpen(
            long laneId, Value open, StreamFraming framing, WireCodec codec)
            throws IOException, VoxException {
        boolean peerOdd = !initiator;
        if (laneId == 0 || ((laneId & 1L) != (peerOdd ? 1L : 0L))) {
            throw new VoxException(
                    "peer opened lane with invalid parity " + Long.toUnsignedString(laneId));
        }
        validatePeerSettings(WireCodec.required(open, "connection_settings"), true);
        if (inboundLanes.containsKey(laneId) || hasOutboundLane(laneId)) {
            throw new VoxException("duplicate lane " + Long.toUnsignedString(laneId));
        }
        if (inboundLanes.size() + outboundLaneCount() >= options.maxOpenLanes()) {
            framing.writeFrame(codec.encodeMessage(
                    laneId, codec.laneReject("resource-limit")));
            return;
        }
        String serviceName = metadataString(
                WireCodec.required(open, "metadata"), "vox-service");
        if (serviceName == null) {
            framing.writeFrame(codec.encodeMessage(
                    laneId, codec.laneReject("unknown-service")));
            return;
        }
        ServiceDispatcher dispatcher = services.find(serviceName);
        if (dispatcher == null) {
            framing.writeFrame(codec.encodeMessage(
                    laneId, codec.laneReject("unknown-service")));
            return;
        }
        inboundLanes.put(laneId, dispatcher);
        framing.writeFrame(codec.encodeMessage(laneId, codec.laneAccept()));
    }

    private void processInboundCall(
            long laneId, long requestId, Value body, WireCodec codec)
            throws VoxException {
        ServiceDispatcher dispatcher = inboundLanes.get(laneId);
        if (dispatcher == null) {
            throw new VoxException(
                    "call for unopened inbound lane " + Long.toUnsignedString(laneId));
        }
        if ((requestId & 1L) == 0) {
            throw new VoxException(
                    "inbound request id violates opener's odd parity: "
                            + Long.toUnsignedString(requestId));
        }
        if (!WireCodec.required(body, "channels").asList().isEmpty()) {
            throw new VoxException("channels are outside the Java unary slice");
        }
        long methodId = WireCodec.unsignedLong(WireCodec.required(body, "method_id"));
        MethodDescriptor method = dispatcher.descriptor().method(methodId);
        if (method == null) {
            throw new VoxException(
                    "unknown method " + Long.toUnsignedString(methodId));
        }
        Value inlineSchemas = WireCodec.required(body, "schemas");
        if (!inlineSchemas.asList().isEmpty()) {
            receivedBindings.put(
                    bindingKey(methodId, WireCodec.Direction.ARGS),
                    codec.parseBinding(WireCodec.byteList(inlineSchemas)));
        }
        SchemaClosure writer =
                receivedBindings.get(bindingKey(methodId, WireCodec.Direction.ARGS));
        if (writer == null) {
            throw new VoxException("call arrived before its argument schema binding");
        }
        if (inboundRequests.size() >= options.maxPendingRequests()) {
            throw new VoxException("inbound pending request bound exceeded");
        }
        String key = requestKey(laneId, requestId);
        if (inboundRequests.containsKey(key)) {
            throw new VoxException("duplicate request " + Long.toUnsignedString(requestId));
        }
        byte[] localArguments = codec.transcode(
                writer,
                method.argumentAdapter().schema(),
                WireCodec.required(body, "args").asBytes());
        CallContext context = new CallContext(
                requestId,
                laneId,
                metadataStrings(WireCodec.required(body, "metadata")));
        InboundCall call = new InboundCall(
                requestId,
                method,
                localArguments,
                context,
                new InboundCall.Reply() {
                    @Override
                    public void success(byte[] response) {
                        enqueueReply(new ReplyCommand(
                                laneId, requestId, method, response, null));
                    }

                    @Override
                    public void failure(VoxException failure) {
                        enqueueReply(new ReplyCommand(
                                laneId, requestId, method, null, failure));
                    }
                });
        inboundRequests.put(key, new InboundRequest(context, call));
        try {
            options.handlerExecutor().execute(() -> {
                CompletableFuture<Void> dispatched;
                try {
                    dispatched = dispatcher.dispatch(call);
                } catch (RuntimeException failure) {
                    dispatched = CompletableFuture.failedFuture(failure);
                }
                dispatched.whenComplete((ignored, failure) -> {
                    if (failure != null && !call.isTerminal()) {
                        Throwable cause = failure instanceof CompletionException
                                && failure.getCause() != null
                                ? failure.getCause()
                                : failure;
                        call.fail(new VoxException("handler failed", cause));
                    } else if (failure == null && !call.isTerminal()) {
                        call.fail(new VoxException("handler completed without replying"));
                    }
                });
            });
        } catch (RejectedExecutionException failure) {
            if (!call.isTerminal()) call.fail(new VoxException("handler executor rejected", failure));
        }
    }

    private void processReply(
            ReplyCommand reply, StreamFraming framing, WireCodec codec)
            throws IOException, VoxException {
        InboundRequest inbound =
                inboundRequests.remove(requestKey(reply.laneId, reply.requestId));
        if (inbound == null) return;
        byte[] response = reply.response;
        if (reply.failure != null) {
            response = encodeInfrastructureFailure(reply.method, reply.failure);
        }
        String binding = bindingKey(reply.method.id(), WireCodec.Direction.RESPONSE);
        if (sentBindings.add(binding)) {
            byte[] schemas;
            try {
                schemas = reply.method.responseWireAdapter().schema().bundleBytes();
            } catch (org.facet.phon.PhonException failure) {
                throw new VoxException("cannot encode response schema binding", failure);
            }
            framing.writeFrame(codec.encodeMessage(
                    reply.laneId,
                    codec.schemaMessage(
                            reply.method.id(), WireCodec.Direction.RESPONSE, schemas)));
        }
        framing.writeFrame(codec.encodeMessage(
                reply.laneId,
                codec.requestResponse(reply.requestId, response)));
    }

    @SuppressWarnings({"rawtypes", "unchecked"})
    private static byte[] encodeInfrastructureFailure(
            MethodDescriptor method, VoxException failure) throws VoxException {
        try {
            return org.facet.phon.PhonCodec.encode(
                    (org.facet.phon.PhonAdapter) method.responseWireAdapter(),
                    VoxResult.infrastructure(
                            VoxResult.Kind.INVALID_PAYLOAD, failure.getMessage()),
                    org.facet.phon.PhonLimits.defaults());
        } catch (org.facet.phon.PhonException encodingFailure) {
            throw new VoxException("cannot encode handler failure response", encodingFailure);
        }
    }

    private void enqueueReply(ReplyCommand reply) {
        if (!commands.offer(reply)) {
            fail(new VoxException("outbound queue is full while replying"));
        }
    }

    private void processLaneClose(long laneId) throws VoxException {
        ServiceDispatcher inbound = inboundLanes.remove(laneId);
        if (inbound != null) {
            cancelInboundLane(laneId);
            return;
        }
        requireLane(laneId).terminate(
                new VoxException("peer closed service lane"), LaneState.CLOSED);
    }

    private void cancelInboundLane(long laneId) {
        for (Map.Entry<String, InboundRequest> entry :
                new ArrayList<>(inboundRequests.entrySet())) {
            if (entry.getValue().context.laneId() == laneId) {
                entry.getValue().context.cancel();
                inboundRequests.remove(entry.getKey());
            }
        }
    }

    private boolean hasOutboundLane(long laneId) {
        synchronized (lanes) {
            for (ServiceLane lane : lanes) if (lane.id() == laneId) return true;
            return false;
        }
    }

    private int outboundLaneCount() {
        synchronized (lanes) {
            return lanes.size();
        }
    }

    private static String metadataString(Value metadata, String name)
            throws VoxException {
        if (metadata.type() == Value.Type.NULL) return null;
        if (metadata.type() != Value.Type.MAP) {
            throw new VoxException("expected metadata map");
        }
        Value value = metadata.asMap().get(name);
        if (value == null) return null;
        if (value.type() != Value.Type.STRING) {
            throw new VoxException("metadata " + name + " is not a string");
        }
        return value.asString();
    }

    private static Map<String, String> metadataStrings(Value metadata)
            throws VoxException {
        if (metadata.type() == Value.Type.NULL) return Map.of();
        if (metadata.type() != Value.Type.MAP) {
            throw new VoxException("expected metadata map");
        }
        Map<String, String> result = new HashMap<>();
        for (Map.Entry<String, Value> entry : metadata.asMap().entrySet()) {
            if (entry.getValue().type() == Value.Type.STRING) {
                result.put(entry.getKey(), entry.getValue().asString());
            }
        }
        return result;
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

    private int validatePeerSettings(Value settings) throws VoxException {
        return validatePeerSettings(settings, !initiator);
    }

    private int validatePeerSettings(Value settings, boolean expectedOdd)
            throws VoxException {
        long credit = WireCodec.unsignedLong(
                WireCodec.required(settings, "initial_channel_credit"));
        if (credit == 0) throw new VoxException("initial_channel_credit must be nonzero");
        String parity = WireCodec.variant(WireCodec.required(settings, "parity"));
        String expected = expectedOdd ? "Odd" : "Even";
        if (!expected.equals(parity)) {
            throw new VoxException(
                    "peer advertised " + parity + " parity; expected " + expected);
        }
        long maximum = WireCodec.unsignedLong(
                WireCodec.required(settings, "max_concurrent_requests"));
        if (maximum == 0 || maximum > Integer.MAX_VALUE) {
            throw new VoxException("invalid peer max_concurrent_requests " + maximum);
        }
        return (int) maximum;
    }

    private synchronized long allocateLaneId() {
        if (laneIdsExhausted.get()) {
            throw new IllegalStateException("lane id space exhausted");
        }
        long candidate = nextLaneId.getAndAdd(2);
        if (candidate == -1L || candidate == -2L) {
            laneIdsExhausted.set(true);
        }
        if (candidate == 0) {
            throw new IllegalStateException("lane id space exhausted");
        }
        return candidate;
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
        terminateInbound();
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

    private void terminateInbound() {
        for (InboundRequest request : inboundRequests.values()) {
            request.context.cancel();
        }
        inboundRequests.clear();
        inboundLanes.clear();
    }

    private static int durationMillis(java.time.Duration duration) {
        long millis = Math.max(1, duration.toMillis());
        return (int) Math.min(Integer.MAX_VALUE, millis);
    }
}
