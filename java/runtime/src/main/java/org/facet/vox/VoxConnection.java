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
public final class VoxConnection implements AutoCloseable, ServiceLane.DriverCommands,
        ChannelRuntime.Transport {
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
    private static final class ChannelItemCommand implements DriverCommand {
        final long laneId;
        final long channelId;
        final byte[] payload;
        ChannelItemCommand(long laneId, long channelId, byte[] payload) {
            this.laneId = laneId;
            this.channelId = channelId;
            this.payload = payload.clone();
        }
    }
    private static final class ChannelCloseCommand implements DriverCommand {
        final long laneId;
        final long channelId;
        ChannelCloseCommand(long laneId, long channelId) {
            this.laneId = laneId;
            this.channelId = channelId;
        }
    }
    private static final class ChannelResetCommand implements DriverCommand {
        final long laneId;
        final long channelId;
        ChannelResetCommand(long laneId, long channelId) {
            this.laneId = laneId;
            this.channelId = channelId;
        }
    }
    private static final class ChannelGrantCommand implements DriverCommand {
        final long laneId;
        final long channelId;
        final int additional;
        ChannelGrantCommand(long laneId, long channelId, int additional) {
            this.laneId = laneId;
            this.channelId = channelId;
            this.additional = additional;
        }
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
    private record PeerSettings(
            int maxConcurrentRequests, int initialChannelCredit, boolean odd) {}
    private record InboundLane(ServiceDispatcher dispatcher, PeerSettings peerSettings) {}
    private static final class ActiveChannel {
        final String requestKey;
        final MethodDescriptor method;
        final ChannelDescriptor descriptor;
        final ChannelRuntime.Sender<?> sender;
        final ChannelRuntime.Receiver<?> receiver;
        final ServiceLane.OutboundCall outboundCall;

        ActiveChannel(
                String requestKey,
                MethodDescriptor method,
                ChannelDescriptor descriptor,
                ChannelRuntime.Sender<?> sender,
                ChannelRuntime.Receiver<?> receiver,
                ServiceLane.OutboundCall outboundCall) {
            this.requestKey = requestKey;
            this.method = method;
            this.descriptor = descriptor;
            this.sender = sender;
            this.receiver = receiver;
            this.outboundCall = outboundCall;
        }

        void progress() {
            if (outboundCall != null) outboundCall.progress();
        }

        void terminate(VoxException reason) {
            if (sender != null) sender.terminate(reason);
            if (receiver != null) receiver.terminate(reason);
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
    private final Map<Long, InboundLane> inboundLanes = new HashMap<>();
    private final Map<String, InboundRequest> inboundRequests = new HashMap<>();
    private final Map<String, SchemaClosure> receivedBindings = new HashMap<>();
    private final Set<String> sentBindings = new HashSet<>();
    private final Map<String, ActiveChannel> activeChannels = new HashMap<>();
    private final Set<Long> locallyClosedLanes = new HashSet<>();
    private final Map<Long, Set<Long>> laneRetirementBarriers = new HashMap<>();
    private final CompletableFuture<Void> closed = new CompletableFuture<>();
    private volatile int peerMaxConcurrentRequests = 1;
    private volatile int peerInitialChannelCredit = 16;

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
                    peerMaxConcurrentRequests,
                    peerInitialChannelCredit);
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

    @Override
    public boolean item(long laneId, long channelId, byte[] payload) {
        Objects.requireNonNull(payload, "payload");
        int bytes = payload.length;
        for (;;) {
            int current = queuedBytes.get();
            if (bytes > options.maxQueuedOutboundBytes() - current) return false;
            if (queuedBytes.compareAndSet(current, current + bytes)) break;
        }
        if (commands.offer(new ChannelItemCommand(laneId, channelId, payload))) return true;
        queuedBytes.addAndGet(-bytes);
        return false;
    }

    @Override public boolean close(long laneId, long channelId) {
        return offerControl(new ChannelCloseCommand(laneId, channelId));
    }

    @Override public boolean reset(long laneId, long channelId) {
        return offerControl(new ChannelResetCommand(laneId, channelId));
    }

    @Override public boolean grant(long laneId, long channelId, int additional) {
        return offerControl(new ChannelGrantCommand(laneId, channelId, additional));
    }

    private boolean offerControl(DriverCommand command) {
        if (commands.offer(command)) return true;
        fail(new VoxException("outbound channel control queue is full"));
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
            PeerSettings peer = validatePeerSettings(
                    WireCodec.required(helloYourself, "connection_settings"));
            peerMaxConcurrentRequests = peer.maxConcurrentRequests();
            peerInitialChannelCredit = peer.initialChannelCredit();
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
            PeerSettings peer = validatePeerSettings(
                    WireCodec.required(hello, "connection_settings"));
            peerMaxConcurrentRequests = peer.maxConcurrentRequests();
            peerInitialChannelCredit = peer.initialChannelCredit();
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
            laneRetirementBarriers.put(open.lane.id(), Set.copyOf(locallyClosedLanes));
        } else if (command instanceof CallCommand call) {
            queuedBytes.addAndGet(-call.call.arguments.length);
            if (!call.call.tryCommit()) return;
            String binding = bindingKey(
                    call.call.laneId, call.call.method.id(), WireCodec.Direction.ARGS);
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
            List<Long> channelIds;
            try {
                channelIds = bindOutboundChannels(call.call);
            } catch (VoxException failure) {
                call.call.fail(failure);
                return;
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
                            channelIds,
                            call.call.options.metadata())));
        } else if (command instanceof CloseLaneCommand close) {
            framing.writeFrame(codec.encodeMessage(close.laneId, codec.laneClose()));
            if (!locallyClosedLanes.contains(close.laneId)
                    && locallyClosedLanes.size() >= options.maxOpenLanes()) {
                throw new VoxException("retired service lane bound exceeded");
            }
            locallyClosedLanes.add(close.laneId);
            retireOutboundLane(close.laneId);
        } else if (command instanceof CancelCommand cancel) {
            framing.writeFrame(codec.encodeMessage(
                    cancel.laneId, codec.requestCancel(cancel.requestId)));
            terminateRequestChannels(
                    requestKey(cancel.laneId, cancel.requestId),
                    new VoxException("local request cancelled"));
        } else if (command instanceof ReplyCommand reply) {
            processReply(reply, framing, codec);
        } else if (command instanceof ChannelItemCommand item) {
            queuedBytes.addAndGet(-item.payload.length);
            ActiveChannel channel = activeChannels.get(channelKey(item.laneId, item.channelId));
            // Peer reset/lane close can retire a sender after it queued an
            // item but before the connection driver writes that item.
            if (channel == null) return;
            channel.progress();
            ensureArgsSchema(channel.method, item.laneId, framing, codec);
            framing.writeFrame(codec.encodeMessage(
                    item.laneId, codec.channelItem(item.channelId, item.payload)));
        } else if (command instanceof ChannelCloseCommand close) {
            ActiveChannel active = activeChannels.get(channelKey(close.laneId, close.channelId));
            if (active == null) return;
            active.progress();
            framing.writeFrame(codec.encodeMessage(
                    close.laneId, codec.channelClose(close.channelId)));
            activeChannels.remove(channelKey(close.laneId, close.channelId));
        } else if (command instanceof ChannelResetCommand reset) {
            ActiveChannel active = activeChannels.get(channelKey(reset.laneId, reset.channelId));
            // A peer Close or request Response can retire the channel while a
            // locally requested reset is still queued. That is a successful
            // terminal race, not an unknown-channel protocol violation.
            if (active == null) return;
            active.progress();
            framing.writeFrame(codec.encodeMessage(
                    reset.laneId, codec.channelReset(reset.channelId)));
            // Keep the reset receiver as a bounded tombstone until the peer
            // closes the channel or completes/cancels its owning request. Any
            // item already in flight is then discarded by Receiver.item while
            // genuinely unknown channel ids continue to fail closed.
        } else if (command instanceof ChannelGrantCommand grant) {
            ActiveChannel active = activeChannels.get(channelKey(grant.laneId, grant.channelId));
            // A receive can queue replenishment just before an already-in-flight
            // remote Close is observed. Credit after graceful close is useless,
            // so drop that local race instead of failing the whole connection.
            if (active == null) return;
            active.progress();
            framing.writeFrame(codec.encodeMessage(
                    grant.laneId, codec.channelGrant(grant.channelId, grant.additional)));
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
            case "LaneAccept" -> {
                PeerSettings peer = validatePeerSettings(
                        WireCodec.required(body, "connection_settings"), null);
                requireLane(laneId).markOpen(
                        peer.maxConcurrentRequests(), peer.initialChannelCredit());
                confirmLaneRetirements(laneId);
            }
            case "LaneReject" -> {
                requireLane(laneId).terminate(
                        new VoxException("peer rejected service lane"), LaneState.FAILED);
                confirmLaneRetirements(laneId);
            }
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
                        bindingKey(laneId, methodId, direction),
                        codec.parseBinding(WireCodec.byteList(
                                WireCodec.required(body, "schemas"))));
            }
            case "RequestMessage" -> processInboundRequest(laneId, body, codec);
            case "ChannelMessage" -> processInboundChannel(laneId, body);
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
            terminateRequestChannels(
                    requestKey(laneId, requestId), new VoxException("request cancelled"));
            return;
        }
        if (!"Response".equals(variant)) {
            throw new VoxException("unsupported inbound request " + variant);
        }
        ServiceLane.OutboundCall call = inFlight.remove(requestKey(laneId, requestId));
        if (call == null) return; // Late response after cancellation/timeout.
        terminateRequestChannels(
                requestKey(laneId, requestId),
                new VoxException("request completed before channel closed"));
        Value response = WireCodec.variantPayload(requestBody);
        byte[] encoded = WireCodec.required(response, "ret").asBytes();
        SchemaClosure writer = receivedBindings.get(
                bindingKey(laneId, call.method.id(), WireCodec.Direction.RESPONSE));
        if (writer == null) {
            call.fail(new VoxException("response arrived before its schema binding"));
            return;
        }
        byte[] local = codec.transcodeResponse(
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
        PeerSettings peerSettings = validatePeerSettings(
                WireCodec.required(open, "connection_settings"), null);
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
        inboundLanes.put(laneId, new InboundLane(dispatcher, peerSettings));
        framing.writeFrame(codec.encodeMessage(laneId, codec.laneAccept()));
    }

    private void processInboundCall(
            long laneId, long requestId, Value body, WireCodec codec)
            throws VoxException {
        InboundLane inboundLane = inboundLanes.get(laneId);
        if (inboundLane == null) {
            throw new VoxException(
                    "call for unopened inbound lane " + Long.toUnsignedString(laneId));
        }
        boolean expectedOdd = inboundLane.peerSettings().odd();
        if ((requestId & 1L) != (expectedOdd ? 1L : 0L)) {
            throw new VoxException(
                    "inbound request id violates opener's negotiated parity: "
                            + Long.toUnsignedString(requestId));
        }
        ServiceDispatcher dispatcher = inboundLane.dispatcher();
        List<Value> channelValues = WireCodec.required(body, "channels").asList();
        long methodId = WireCodec.unsignedLong(WireCodec.required(body, "method_id"));
        MethodDescriptor method = dispatcher.descriptor().method(methodId);
        if (method == null) {
            throw new VoxException(
                    "unknown method " + Long.toUnsignedString(methodId));
        }
        if (channelValues.size() != method.channels().size()) {
            throw new VoxException("request channel count does not match method descriptor");
        }
        List<Long> channelIds = new ArrayList<>(channelValues.size());
        Set<Long> uniqueChannelIds = new HashSet<>();
        for (Value value : channelValues) {
            long channelId = WireCodec.unsignedLong(value);
            if (channelId == 0
                    || (channelId & 1L) != (expectedOdd ? 1L : 0L)) {
                throw new VoxException("inbound channel id violates caller parity: "
                        + Long.toUnsignedString(channelId));
            }
            if (!uniqueChannelIds.add(channelId)) {
                throw new VoxException("duplicate channel id " + Long.toUnsignedString(channelId));
            }
            channelIds.add(channelId);
        }
        Value inlineSchemas = WireCodec.required(body, "schemas");
        if (!inlineSchemas.asList().isEmpty()) {
            receivedBindings.put(
                    bindingKey(laneId, methodId, WireCodec.Direction.ARGS),
                    codec.parseBinding(WireCodec.byteList(inlineSchemas)));
        }
        SchemaClosure writer =
                receivedBindings.get(bindingKey(laneId, methodId, WireCodec.Direction.ARGS));
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
        List<Object> channelEndpoints = bindInboundChannels(
                laneId, requestId, method, channelIds,
                inboundLane.peerSettings().initialChannelCredit());
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
                },
                channelEndpoints);
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
        finishInboundRequestChannels(
                requestKey(reply.laneId, reply.requestId), reply.laneId, framing, codec);
        byte[] response = reply.response;
        if (reply.failure != null) {
            response = encodeInfrastructureFailure(reply.method, reply.failure);
        }
        String binding = bindingKey(
                reply.laneId, reply.method.id(), WireCodec.Direction.RESPONSE);
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

    private List<Long> bindOutboundChannels(ServiceLane.OutboundCall call) throws VoxException {
        if (call.channels.isEmpty()) return List.of();
        ArrayList<Long> ids = new ArrayList<>(call.channels.size());
        String request = requestKey(call.laneId, call.requestId);
        for (VoxChannelArgument argument : call.channels) {
            long channelId = call.allocateChannelId();
            bindOutboundChannel(
                    call, request, call.laneId, channelId, call.method,
                    argument.descriptor(), argument.endpoint());
            ids.add(channelId);
        }
        return List.copyOf(ids);
    }

    @SuppressWarnings("unchecked")
    private <T> void bindOutboundChannel(
            ServiceLane.OutboundCall call,
            String request,
            long laneId,
            long channelId,
            MethodDescriptor method,
            ChannelDescriptor descriptor,
            Object endpoint) throws VoxException {
        org.facet.phon.PhonAdapter<T> adapter =
                (org.facet.phon.PhonAdapter<T>) descriptor.elementAdapter();
        ChannelRuntime.Sender<T> sender = null;
        ChannelRuntime.Receiver<T> receiver = null;
        if (descriptor.direction() == ChannelDescriptor.Direction.TX) {
            if (!(endpoint instanceof VoxTx<?> tx)) {
                throw new VoxException("Tx descriptor received non-Tx endpoint");
            }
            receiver = new ChannelRuntime.Receiver<>(
                    laneId, channelId, adapter, this, options.initialChannelCredit());
            ((VoxTx<T>) tx).core().bindReceiver(receiver);
        } else {
            if (!(endpoint instanceof VoxRx<?> rx)) {
                throw new VoxException("Rx descriptor received non-Rx endpoint");
            }
            sender = new ChannelRuntime.Sender<>(
                    laneId, channelId, adapter, this, call.peerInitialChannelCredit());
            ((VoxRx<T>) rx).core().bindSender(sender);
        }
        String key = channelKey(laneId, channelId);
        if (activeChannels.put(key,
                new ActiveChannel(request, method, descriptor, sender, receiver, call)) != null) {
            throw new VoxException("duplicate active channel " + key);
        }
    }

    private List<Object> bindInboundChannels(
            long laneId,
            long requestId,
            MethodDescriptor method,
            List<Long> ids,
            int peerInitialChannelCredit)
            throws VoxException {
        ArrayList<Object> endpoints = new ArrayList<>(ids.size());
        String request = requestKey(laneId, requestId);
        for (int index = 0; index < ids.size(); index++) {
            endpoints.add(bindInboundChannel(
                    request, laneId, ids.get(index), method, method.channels().get(index),
                    peerInitialChannelCredit));
        }
        return List.copyOf(endpoints);
    }

    @SuppressWarnings("unchecked")
    private <T> Object bindInboundChannel(
            String request,
            long laneId,
            long channelId,
            MethodDescriptor method,
            ChannelDescriptor descriptor,
            int peerInitialChannelCredit) throws VoxException {
        org.facet.phon.PhonAdapter<T> adapter =
                (org.facet.phon.PhonAdapter<T>) descriptor.elementAdapter();
        ChannelRuntime.Core<T> core = new ChannelRuntime.Core<>(adapter);
        ChannelRuntime.Sender<T> sender = null;
        ChannelRuntime.Receiver<T> receiver = null;
        Object endpoint;
        if (descriptor.direction() == ChannelDescriptor.Direction.TX) {
            sender = new ChannelRuntime.Sender<>(
                    laneId, channelId, adapter, this, peerInitialChannelCredit);
            core.bindSender(sender);
            endpoint = new VoxTx<>(core);
        } else {
            receiver = new ChannelRuntime.Receiver<>(
                    laneId, channelId, adapter, this, options.initialChannelCredit());
            core.bindReceiver(receiver);
            endpoint = new VoxRx<>(core);
        }
        String key = channelKey(laneId, channelId);
        if (activeChannels.put(key,
                new ActiveChannel(request, method, descriptor, sender, receiver, null)) != null) {
            throw new VoxException("duplicate active channel " + key);
        }
        return endpoint;
    }

    private void processInboundChannel(long laneId, Value channel) throws VoxException {
        long channelId = WireCodec.unsignedLong(WireCodec.required(channel, "id"));
        Value body = WireCodec.required(channel, "body");
        String variant = WireCodec.variant(body);
        ActiveChannel active = activeChannels.get(channelKey(laneId, channelId));
        if (active == null && locallyClosedLanes.contains(laneId)) return;
        if (active == null) {
            throw new VoxException("message for unknown channel "
                    + Long.toUnsignedString(laneId) + ":" + Long.toUnsignedString(channelId));
        }
        active.progress();
        switch (variant) {
            case "Item" -> {
                if (active.receiver == null) {
                    throw new VoxException("peer sent item on locally-sending channel");
                }
                SchemaClosure binding = receivedBindings.get(
                        bindingKey(laneId, active.method.id(), WireCodec.Direction.ARGS));
                if (binding == null) {
                    throw new VoxException("channel item arrived before argument schema binding");
                }
                SchemaClosure writer;
                try {
                    writer = binding.auxiliary(active.descriptor.role());
                } catch (org.facet.phon.PhonException failure) {
                    throw new VoxException("invalid channel auxiliary schema", failure);
                }
                if (writer == null) {
                    throw new VoxException("missing channel auxiliary schema "
                            + active.descriptor.role());
                }
                Value item = WireCodec.variantPayload(body);
                receiveItem(active.receiver, WireCodec.required(item, "item").asBytes(), writer);
            }
            case "Close" -> {
                if (active.receiver == null) {
                    throw new VoxException("peer closed locally-sending channel");
                }
                active.receiver.closeGracefully();
                activeChannels.remove(channelKey(laneId, channelId));
            }
            case "Reset" -> {
                if (active.sender == null) {
                    throw new VoxException("peer reset locally-receiving channel");
                }
                active.sender.terminate(new VoxException("peer reset channel"));
                activeChannels.remove(channelKey(laneId, channelId));
            }
            case "GrantCredit" -> {
                if (active.sender == null) {
                    throw new VoxException("peer granted credit to locally-receiving channel");
                }
                long additional = WireCodec.unsignedLong(
                        WireCodec.required(WireCodec.variantPayload(body), "additional"));
                if (additional == 0 || additional > Integer.MAX_VALUE) {
                    throw new VoxException("invalid channel credit " + additional);
                }
                active.sender.grant((int) additional);
            }
            default -> throw new VoxException("unsupported channel message " + variant);
        }
    }

    @SuppressWarnings("unchecked")
    private static <T> void receiveItem(
            ChannelRuntime.Receiver<?> receiver, byte[] payload, SchemaClosure writer)
            throws VoxException {
        ((ChannelRuntime.Receiver<T>) receiver).item(payload, writer);
    }

    private void ensureArgsSchema(
            MethodDescriptor method, long laneId, StreamFraming framing, WireCodec codec)
            throws IOException, VoxException {
        String binding = bindingKey(laneId, method.id(), WireCodec.Direction.ARGS);
        if (!sentBindings.add(binding)) return;
        try {
            framing.writeFrame(codec.encodeMessage(
                    laneId,
                    codec.schemaMessage(
                            method.id(), WireCodec.Direction.ARGS,
                            method.argumentAdapter().schema().bundleBytes())));
        } catch (org.facet.phon.PhonException failure) {
            throw new VoxException("cannot encode channel schema binding", failure);
        }
    }

    private ActiveChannel requireActiveChannel(long laneId, long channelId) throws VoxException {
        ActiveChannel channel = activeChannels.get(channelKey(laneId, channelId));
        if (channel == null) {
            throw new VoxException("message for unknown channel "
                    + Long.toUnsignedString(laneId) + ":" + Long.toUnsignedString(channelId));
        }
        return channel;
    }

    private void terminateRequestChannels(String request, VoxException reason) {
        for (Map.Entry<String, ActiveChannel> entry :
                new ArrayList<>(activeChannels.entrySet())) {
            if (entry.getValue().requestKey.equals(request)) {
                ActiveChannel removed = activeChannels.remove(entry.getKey());
                if (removed != null) removed.terminate(reason);
            }
        }
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
        if (locallyClosedLanes.remove(laneId)) {
            laneRetirementBarriers.remove(laneId);
            return;
        }
        InboundLane inbound = inboundLanes.remove(laneId);
        if (inbound != null) {
            cancelInboundLane(laneId);
            clearLaneBindings(laneId);
            return;
        }
        requireLane(laneId).terminate(
                new VoxException("peer closed service lane"), LaneState.CLOSED);
        terminateLaneChannels(laneId, new VoxException("peer closed service lane"));
        clearLaneBindings(laneId);
    }

    private void cancelInboundLane(long laneId) {
        for (Map.Entry<String, InboundRequest> entry :
                new ArrayList<>(inboundRequests.entrySet())) {
            if (entry.getValue().context.laneId() == laneId) {
                entry.getValue().context.cancel();
                inboundRequests.remove(entry.getKey());
            }
        }
        terminateLaneChannels(laneId, new VoxException("service lane closed"));
    }

    private void finishInboundRequestChannels(
            String request, long laneId, StreamFraming framing, WireCodec codec)
            throws IOException, VoxException {
        for (Map.Entry<String, ActiveChannel> entry :
                new ArrayList<>(activeChannels.entrySet())) {
            ActiveChannel active = entry.getValue();
            if (!active.requestKey.equals(request)) continue;
            if (active.sender != null) {
                ensureArgsSchema(active.method, laneId, framing, codec);
                framing.writeFrame(codec.encodeMessage(
                        laneId,
                        codec.channelClose(parseChannelId(entry.getKey()))));
            } else {
                framing.writeFrame(codec.encodeMessage(
                        laneId,
                        codec.channelReset(parseChannelId(entry.getKey()))));
            }
            ActiveChannel removed = activeChannels.remove(entry.getKey());
            if (removed != null) {
                removed.terminate(new VoxException("request scope completed"));
            }
        }
    }

    private void terminateLaneChannels(long laneId, VoxException reason) {
        String prefix = Long.toUnsignedString(laneId) + ":";
        for (Map.Entry<String, ActiveChannel> entry :
                new ArrayList<>(activeChannels.entrySet())) {
            if (!entry.getKey().startsWith(prefix)) continue;
            ActiveChannel removed = activeChannels.remove(entry.getKey());
            if (removed != null) removed.terminate(reason);
        }
    }

    private void retireOutboundLane(long laneId) {
        VoxException reason = new VoxException("local service lane closed");
        String prefix = Long.toUnsignedString(laneId) + ":";
        for (Map.Entry<String, ServiceLane.OutboundCall> entry :
                new ArrayList<>(inFlight.entrySet())) {
            if (!entry.getKey().startsWith(prefix)) continue;
            ServiceLane.OutboundCall removed = inFlight.remove(entry.getKey());
            if (removed != null) removed.fail(reason);
        }
        terminateLaneChannels(laneId, reason);
        synchronized (lanes) {
            lanes.removeIf(lane -> lane.id() == laneId);
        }
        clearLaneBindings(laneId);
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

    private PeerSettings validatePeerSettings(Value settings) throws VoxException {
        return validatePeerSettings(settings, !initiator);
    }

    private PeerSettings validatePeerSettings(Value settings, Boolean expectedOdd)
            throws VoxException {
        long credit = WireCodec.unsignedLong(
                WireCodec.required(settings, "initial_channel_credit"));
        if (credit == 0) throw new VoxException("initial_channel_credit must be nonzero");
        String parity = WireCodec.variant(WireCodec.required(settings, "parity"));
        boolean odd = switch (parity) {
            case "Odd" -> true;
            case "Even" -> false;
            default -> throw new VoxException("invalid peer parity " + parity);
        };
        if (expectedOdd != null && expectedOdd.booleanValue() != odd) {
            String expected = expectedOdd ? "Odd" : "Even";
            throw new VoxException(
                    "peer advertised " + parity + " parity; expected " + expected);
        }
        long maximum = WireCodec.unsignedLong(
                WireCodec.required(settings, "max_concurrent_requests"));
        if (maximum == 0 || maximum > Integer.MAX_VALUE) {
            throw new VoxException("invalid peer max_concurrent_requests " + maximum);
        }
        if (credit > Integer.MAX_VALUE) {
            throw new VoxException("invalid initial_channel_credit " + credit);
        }
        return new PeerSettings((int) maximum, (int) credit, odd);
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

    private void clearLaneBindings(long laneId) {
        String prefix = Long.toUnsignedString(laneId) + ":";
        sentBindings.removeIf(key -> key.startsWith(prefix));
        receivedBindings.keySet().removeIf(key -> key.startsWith(prefix));
    }

    private void confirmLaneRetirements(long acceptedLaneId) {
        Set<Long> confirmed = laneRetirementBarriers.remove(acceptedLaneId);
        if (confirmed != null) locallyClosedLanes.removeAll(confirmed);
    }

    private static String bindingKey(
            long laneId, long methodId, WireCodec.Direction direction) {
        return Long.toUnsignedString(laneId)
                + ":" + Long.toUnsignedString(methodId)
                + ":" + direction;
    }

    private static String requestKey(long laneId, long requestId) {
        return Long.toUnsignedString(laneId) + ":" + Long.toUnsignedString(requestId);
    }

    private static String channelKey(long laneId, long channelId) {
        return Long.toUnsignedString(laneId) + ":" + Long.toUnsignedString(channelId);
    }

    private static long parseChannelId(String key) {
        return Long.parseUnsignedLong(key.substring(key.indexOf(':') + 1));
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
            } else if (command instanceof ChannelItemCommand item) {
                queuedBytes.addAndGet(-item.payload.length);
            }
        }
    }

    private void terminateLanes(Throwable failure, LaneState terminal) {
        VoxException channelFailure = failure instanceof VoxException vox
                ? vox : new VoxException("connection terminated", failure);
        for (ActiveChannel channel : new ArrayList<>(activeChannels.values())) {
            channel.terminate(channelFailure);
        }
        activeChannels.clear();
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
