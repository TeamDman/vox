package org.facet.vox;

import java.net.InetSocketAddress;
import java.net.ServerSocket;
import java.net.Socket;
import java.time.Duration;
import java.util.List;
import java.util.Map;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.LinkedBlockingQueue;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.TimeoutException;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicInteger;
import org.facet.phon.PhonAdapter;
import org.facet.phon.PhonDecoder;
import org.facet.phon.PhonEncoder;
import org.facet.phon.PhonException;
import org.facet.phon.Schema;
import org.facet.phon.SchemaClosure;
import org.facet.vox.generated.DivideByZero;
import org.facet.vox.generated.DivideRequest;
import org.facet.vox.generated.DivideResponse;
import org.facet.vox.generated.JavaFixtureClient;
import org.facet.vox.generated.JavaFixtureDispatcher;
import org.facet.vox.generated.JavaFixtureHandler;
import org.facet.vox.generated.JavaFixtureServiceDescriptor;
import org.facet.vox.generated.NestedRequest;
import org.facet.vox.generated.NestedResponse;

public final class VoxRuntimeTest {
    private static final PhonAdapter<String> ADAPTER = new PhonAdapter<>() {
        private final SchemaClosure schema = stringSchema();
        public SchemaClosure schema() { return schema; }
        public void encode(PhonEncoder encoder, String value) throws PhonException {
            encoder.writeString(value);
        }
        public String decode(PhonDecoder decoder) throws PhonException {
            return decoder.readString();
        }
    };

    public static void main(String[] args) throws Exception {
        boundsAreFiniteAndPositive();
        registryRejectsDuplicates();
        pendingAndOutboundBoundsFailClosed();
        laneCorrelatesAndDiscardsLateResponses();
        requestIdsFollowNegotiatedParity();
        laneCancellationAndTimeoutAreTerminal();
        controlQueueFailureTerminatesLane();
        inboundCallIsExactlyOnce();
        connectionDriverOwnershipAndHandshake();
        generatedChannelRoundTripHonorsCreditAndCancellation();
        System.out.println("VoxRuntimeTest: PASS");
    }

    private static void generatedChannelRoundTripHonorsCreditAndCancellation()
            throws Exception {
        AtomicInteger sent = new AtomicInteger();
        AtomicBoolean cancelled = new AtomicBoolean();
        ExecutorService handlers = Executors.newFixedThreadPool(2, runnable -> {
            Thread thread = new Thread(runnable, "vox-java-test-handler");
            thread.setDaemon(true);
            return thread;
        });
        ConnectionOptions serverOptions = ConnectionOptions.builder()
                .initialChannelCredit(2)
                .handlerExecutor(handlers)
                .build();
        ConnectionOptions clientOptions = ConnectionOptions.builder()
                .initialChannelCredit(3)
                .build();
        JavaFixtureHandler handler = new JavaFixtureHandler() {
            @Override public CompletableFuture<String> echo(
                    CallContext context, String value) {
                return CompletableFuture.completedFuture(value);
            }

            @Override public CompletableFuture<NestedResponse> inspect(
                    CallContext context, NestedRequest request) {
                return CompletableFuture.failedFuture(
                        new UnsupportedOperationException("unused by channel test"));
            }

            @Override public CompletableFuture<VoxResult<DivideResponse, DivideByZero>> divide(
                    CallContext context, DivideRequest request) {
                return CompletableFuture.failedFuture(
                        new UnsupportedOperationException("unused by channel test"));
            }

            @Override public CompletableFuture<String> generate(
                    CallContext context, long count, VoxTx<String> output) {
                try {
                    for (long index = 0; index < count; index++) {
                        output.send("item-" + index);
                        sent.incrementAndGet();
                    }
                    output.close();
                    return CompletableFuture.completedFuture("sent-" + count);
                } catch (InterruptedException failure) {
                    Thread.currentThread().interrupt();
                    return CompletableFuture.failedFuture(failure);
                } catch (VoxException failure) {
                    cancelled.set(context.isCancelled());
                    return CompletableFuture.completedFuture("stopped");
                }
            }
        };

        try (ServerSocket listener = new ServerSocket(0)) {
            CompletableFuture<VoxConnection> accepted = new CompletableFuture<>();
            Thread acceptThread = new Thread(() -> {
                try {
                    Socket socket = listener.accept();
                    accepted.complete(VoxConnection.accept(
                            socket,
                            new ServiceRegistry().register(
                                    new JavaFixtureDispatcher(handler)),
                            serverOptions));
                } catch (Exception failure) {
                    accepted.completeExceptionally(failure);
                }
            }, "vox-java-channel-accept");
            acceptThread.start();
            VoxConnection client = VoxConnection.connect(
                    new InetSocketAddress("127.0.0.1", listener.getLocalPort()),
                    clientOptions);
            VoxConnection server = accepted.get(2, TimeUnit.SECONDS);
            CompletableFuture<Void> serverDone = server.start(VoxRuntimeTest::startDaemon);
            CompletableFuture<Void> clientDone = client.start(VoxRuntimeTest::startDaemon);
            awaitConnectionOpen(client, server, clientDone, serverDone);

            ServiceLane lane = client.openLane(
                    JavaFixtureServiceDescriptor.INSTANCE,
                    LaneOptions.defaults());
            lane.opened().get(2, TimeUnit.SECONDS);
            JavaFixtureClient fixture = new JavaFixtureClient(lane);

            VoxChannels.Pair<String> stream = VoxChannels.channel(ADAPTER);
            CompletableFuture<String> result = fixture.generate(
                    40,
                    stream.tx(),
                    CallOptions.withIdleTimeout(Duration.ofMillis(150)));
            Thread.sleep(75);
            check(sent.get() == 3,
                    "sender stops at receiver-advertised initial credit; sent=" + sent.get());
            for (int index = 0; index < 40; index++) {
                String item = stream.rx().receive(Duration.ofSeconds(2));
                check(("item-" + index).equals(item), "ordered channel item " + index);
                Thread.sleep(10);
            }
            check(stream.rx().receive(Duration.ofSeconds(2)) == null,
                    "graceful channel close follows queued items");
            check("sent-40".equals(result.get(2, TimeUnit.SECONDS)),
                    "generated channel response");

            sent.set(0);
            cancelled.set(false);
            VoxChannels.Pair<String> cancelledStream = VoxChannels.channel(ADAPTER);
            CompletableFuture<String> cancelledCall = fixture.generate(
                    10_000,
                    cancelledStream.tx(),
                    CallOptions.withIdleTimeout(Duration.ofSeconds(2)));
            long deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(2);
            while (sent.get() < 3 && System.nanoTime() < deadline) Thread.sleep(5);
            check(cancelledCall.cancel(false), "request cancellation accepted");
            deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(2);
            while (!cancelled.get() && System.nanoTime() < deadline) Thread.sleep(5);
            check(cancelled.get(), "remote handler observes request cancellation");
            boolean receiverTerminated = false;
            for (int attempt = 0; attempt < 5 && !receiverTerminated; attempt++) {
                try {
                    cancelledStream.rx().receive(Duration.ofSeconds(1));
                } catch (VoxException failure) {
                    receiverTerminated = true;
                }
            }
            check(receiverTerminated, "cancelled request terminates local receiver");

            lane.close();
            client.close();
            server.close();
            try { clientDone.get(2, TimeUnit.SECONDS); } catch (ExecutionException ignored) {}
            try { serverDone.get(2, TimeUnit.SECONDS); } catch (ExecutionException ignored) {}
            acceptThread.join();
        } finally {
            handlers.shutdownNow();
        }
    }

    private static void startDaemon(Runnable command) {
        Thread thread = new Thread(command, "vox-java-test-driver");
        thread.setDaemon(true);
        thread.start();
    }

    private static void awaitConnectionOpen(
            VoxConnection client,
            VoxConnection server,
            CompletableFuture<Void> clientDone,
            CompletableFuture<Void> serverDone) throws Exception {
        long deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(2);
        while ((client.state() != ConnectionState.OPEN || server.state() != ConnectionState.OPEN)
                && System.nanoTime() < deadline) {
            Thread.sleep(5);
        }
        check(client.state() == ConnectionState.OPEN,
                "channel client opens; failure=" + completionFailure(clientDone));
        check(server.state() == ConnectionState.OPEN,
                "channel server opens; failure=" + completionFailure(serverDone));
    }

    private static void requestIdsFollowNegotiatedParity() throws Exception {
        FakeDriver driver = new FakeDriver();
        ConnectionOptions options = ConnectionOptions.defaults();
        MethodDescriptor method = method();
        ServiceDescriptor service = new ServiceDescriptor("echo", List.of(method));
        ServiceLane lane =
                new ServiceLane(
                        2,
                        service,
                        driver,
                        options,
                        LaneOptions.defaults(),
                        LaneState.OPEN,
                        2,
                        64,
                        16);
        CompletableFuture<byte[]> first =
                lane.call(method, new byte[0], CallOptions.defaults());
        ServiceLane.OutboundCall firstCall = driver.take();
        CompletableFuture<byte[]> second =
                lane.call(method, new byte[0], CallOptions.defaults());
        ServiceLane.OutboundCall secondCall = driver.take();
        check(firstCall.requestId == 2 && secondCall.requestId == 4,
                "request ids retain even negotiated parity");
        first.cancel(false);
        second.cancel(false);
        lane.close();
        options.closeOwnedResources();
    }

    private static void controlQueueFailureTerminatesLane() throws Exception {
        ConnectionOptions options = ConnectionOptions.defaults();
        MethodDescriptor method = method();
        ControlRejectingDriver driver = new ControlRejectingDriver();
        ServiceLane lane = lane(driver, options, method);
        CompletableFuture<byte[]> pending =
                lane.call(method, new byte[0], CallOptions.defaults());
        ServiceLane.OutboundCall call = driver.take();
        call.tryCommit();
        pending.cancel(false);
        check(lane.state() == LaneState.FAILED,
                "lost cancel control command fails lane closed");
        options.closeOwnedResources();
    }

    private static void boundsAreFiniteAndPositive() {
        expectThrows(IllegalArgumentException.class,
                () -> ConnectionOptions.builder().maxFrameBytes(0).build());
        expectThrows(IllegalArgumentException.class,
                () -> new CallOptions(Duration.ZERO, Map.of()));
    }

    private static void registryRejectsDuplicates() {
        MethodDescriptor method = method();
        ServiceDescriptor descriptor = new ServiceDescriptor("echo", List.of(method));
        ServiceDispatcher dispatcher = new ServiceDispatcher() {
            public ServiceDescriptor descriptor() { return descriptor; }
            public CompletableFuture<Void> dispatch(InboundCall call) {
                return CompletableFuture.completedFuture(null);
            }
        };
        ServiceRegistry registry = new ServiceRegistry().register(dispatcher);
        expectThrows(IllegalArgumentException.class, () -> registry.register(dispatcher));
    }

    private static void laneCorrelatesAndDiscardsLateResponses() throws Exception {
        FakeDriver driver = new FakeDriver();
        ConnectionOptions options = ConnectionOptions.defaults();
        MethodDescriptor method = method();
        ServiceLane lane = lane(driver, options, method);
        CompletableFuture<byte[]> first =
                lane.call(method, new byte[] {1}, CallOptions.withIdleTimeout(Duration.ofSeconds(1)));
        ServiceLane.OutboundCall firstCall = driver.take();
        firstCall.tryCommit();
        firstCall.succeed(new byte[] {9});
        check(first.get()[0] == 9, "response correlation");

        CompletableFuture<byte[]> cancelled =
                lane.call(method, new byte[] {2}, CallOptions.withIdleTimeout(Duration.ofSeconds(1)));
        ServiceLane.OutboundCall cancelledCall = driver.take();
        cancelledCall.tryCommit();
        check(cancelled.cancel(false), "future cancellation");
        cancelledCall.succeed(new byte[] {7}); // Must not complete any future.

        CompletableFuture<byte[]> next =
                lane.call(method, new byte[] {3}, CallOptions.withIdleTimeout(Duration.ofSeconds(1)));
        ServiceLane.OutboundCall nextCall = driver.take();
        nextCall.tryCommit();
        nextCall.succeed(new byte[] {8});
        check(next.get()[0] == 8, "late response did not corrupt later correlation");
        lane.close();
        options.closeOwnedResources();
    }

    private static void pendingAndOutboundBoundsFailClosed() throws Exception {
        MethodDescriptor method = method();
        ConnectionOptions onePending =
                ConnectionOptions.builder().maxPendingRequests(1).build();
        FakeDriver holding = new FakeDriver();
        ServiceLane lane = lane(holding, onePending, method);
        CompletableFuture<byte[]> first =
                lane.call(method, new byte[0], CallOptions.withIdleTimeout(Duration.ofSeconds(1)));
        holding.take();
        CompletableFuture<byte[]> second =
                lane.call(method, new byte[0], CallOptions.withIdleTimeout(Duration.ofSeconds(1)));
        ExecutionException bound =
                expectThrows(ExecutionException.class, second::get);
        check(bound.getCause().getMessage().contains("pending request bound"),
                "pending request bound");
        first.cancel(false);
        lane.close();
        onePending.closeOwnedResources();

        ConnectionOptions rejectedOptions = ConnectionOptions.defaults();
        ServiceLane rejectedLane = lane(new RejectingDriver(), rejectedOptions, method);
        CompletableFuture<byte[]> rejected = rejectedLane.call(
                method, new byte[0], CallOptions.withIdleTimeout(Duration.ofSeconds(1)));
        ExecutionException rejection =
                expectThrows(ExecutionException.class, rejected::get);
        check(rejection.getCause().getMessage().contains("outbound queue"),
                "outbound queue rejection");
        rejectedLane.close();
        rejectedOptions.closeOwnedResources();
    }

    private static void laneCancellationAndTimeoutAreTerminal() throws Exception {
        FakeDriver driver = new FakeDriver();
        ConnectionOptions options = ConnectionOptions.defaults();
        MethodDescriptor method = method();
        ServiceLane lane = lane(driver, options, method);

        CompletableFuture<byte[]> beforeCommit =
                lane.call(method, new byte[0], CallOptions.withIdleTimeout(Duration.ofSeconds(1)));
        ServiceLane.OutboundCall uncommitted = driver.take();
        beforeCommit.cancel(false);
        check(driver.cancels.get() == 0, "cancel before commitment stays local");
        check(!uncommitted.tryCommit(), "cancelled call cannot later commit");

        CompletableFuture<byte[]> timedOut =
                lane.call(method, new byte[0], CallOptions.withIdleTimeout(Duration.ofMillis(20)));
        ServiceLane.OutboundCall timedCall = driver.take();
        timedCall.tryCommit();
        ExecutionException failure = expectThrows(
                ExecutionException.class, () -> timedOut.get(2, TimeUnit.SECONDS));
        check(failure.getCause() instanceof TimeoutException, "idle timeout kind");
        check(driver.cancels.get() == 1, "timeout sends cancel after commitment");
        lane.close();
        options.closeOwnedResources();
    }

    private static void inboundCallIsExactlyOnce() {
        AtomicInteger replies = new AtomicInteger();
        InboundCall call = new InboundCall(
                4, method(), new byte[] {1},
                new CallContext(4, 2, Map.of()),
                new InboundCall.Reply() {
                    public void success(byte[] bytes) { replies.incrementAndGet(); }
                    public void failure(VoxException failure) { replies.incrementAndGet(); }
                });
        call.respond(new byte[] {2});
        check(replies.get() == 1, "one terminal response");
        expectThrows(IllegalStateException.class, () -> call.fail(new VoxException("late")));
    }

    private static void connectionDriverOwnershipAndHandshake() throws Exception {
        try (ServerSocket listener = new ServerSocket(0)) {
            CompletableFuture<VoxConnection> accepted = new CompletableFuture<>();
            Thread acceptThread = new Thread(() -> {
                try {
                    Socket socket = listener.accept();
                    accepted.complete(VoxConnection.accept(
                            socket, new ServiceRegistry(), ConnectionOptions.defaults()));
                } catch (Exception failure) {
                    accepted.completeExceptionally(failure);
                }
            });
            acceptThread.start();
            VoxConnection client = VoxConnection.connect(
                    new InetSocketAddress("127.0.0.1", listener.getLocalPort()),
                    ConnectionOptions.defaults());
            VoxConnection server = accepted.get(2, TimeUnit.SECONDS);
            CompletableFuture<Void> serverDone =
                    server.start(command -> new Thread(command, "server-driver").start());
            CompletableFuture<Void> clientDone =
                    client.start(command -> new Thread(command, "client-driver").start());
            long deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(2);
            while ((client.state() != ConnectionState.OPEN
                            || server.state() != ConnectionState.OPEN)
                    && System.nanoTime() < deadline) {
                Thread.sleep(5);
            }
            check(client.state() == ConnectionState.OPEN,
                    "client handshake opens; state=" + client.state()
                            + ", failure=" + completionFailure(clientDone));
            check(server.state() == ConnectionState.OPEN,
                    "server handshake opens; state=" + server.state()
                            + ", failure=" + completionFailure(serverDone));
            CompletableFuture<Void> duplicate = client.start(Runnable::run);
            expectThrows(ExecutionException.class, duplicate::get);
            client.close();
            server.close();
            try { clientDone.get(2, TimeUnit.SECONDS); } catch (ExecutionException ignored) {}
            try { serverDone.get(2, TimeUnit.SECONDS); } catch (ExecutionException ignored) {}
            acceptThread.join();
        }
    }


    private static ServiceLane lane(
            ServiceLane.DriverCommands driver,
            ConnectionOptions options,
            MethodDescriptor method) {
        ServiceDescriptor service = new ServiceDescriptor("echo", List.of(method));
        return new ServiceLane(
                1,
                service,
                driver,
                options,
                LaneOptions.defaults(),
                LaneState.OPEN,
                1,
                64,
                16);
    }

    private static MethodDescriptor method() {
        return new MethodDescriptor(
                0x8000_0000_0000_0001L,
                "echo",
                ADAPTER,
                ADAPTER,
                null,
                ADAPTER);
    }

    private static SchemaClosure stringSchema() {
        try {
            return SchemaClosure.of(Schema.primitive(Schema.Primitive.STRING));
        } catch (PhonException failure) {
            throw new ExceptionInInitializerError(failure);
        }
    }

    private static final class FakeDriver implements ServiceLane.DriverCommands {
        private final LinkedBlockingQueue<ServiceLane.OutboundCall> submitted =
                new LinkedBlockingQueue<>();
        final AtomicInteger cancels = new AtomicInteger();
        public boolean submit(ServiceLane.OutboundCall call) {
            submitted.add(call);
            return true;
        }
        public boolean cancel(long laneId, long requestId) {
            cancels.incrementAndGet();
            return true;
        }
        public boolean closeLane(long laneId) { return true; }
        ServiceLane.OutboundCall take() throws Exception {
            ServiceLane.OutboundCall value = submitted.poll(1, TimeUnit.SECONDS);
            if (value == null) throw new TimeoutException("no submitted call");
            return value;
        }
    }

    private static final class RejectingDriver implements ServiceLane.DriverCommands {
        public boolean submit(ServiceLane.OutboundCall call) { return false; }
        public boolean cancel(long laneId, long requestId) { return true; }
        public boolean closeLane(long laneId) { return true; }
    }

    private static final class ControlRejectingDriver implements ServiceLane.DriverCommands {
        private final LinkedBlockingQueue<ServiceLane.OutboundCall> submitted =
                new LinkedBlockingQueue<>();
        public boolean submit(ServiceLane.OutboundCall call) {
            submitted.add(call);
            return true;
        }
        public boolean cancel(long laneId, long requestId) { return false; }
        public boolean closeLane(long laneId) { return false; }
        ServiceLane.OutboundCall take() throws Exception {
            ServiceLane.OutboundCall value = submitted.poll(1, TimeUnit.SECONDS);
            if (value == null) throw new TimeoutException("no submitted call");
            return value;
        }
    }

    private static <T extends Throwable> T expectThrows(
            Class<T> type, ThrowingRunnable runnable) {
        try {
            runnable.run();
        } catch (Throwable failure) {
            if (type.isInstance(failure)) return type.cast(failure);
            throw new AssertionError("expected " + type.getName() + ", got " + failure, failure);
        }
        throw new AssertionError("expected " + type.getName());
    }

    private static String completionFailure(CompletableFuture<?> future) {
        if (!future.isDone()) return "<pending>";
        try {
            future.join();
            return "<none>";
        } catch (java.util.concurrent.CompletionException failure) {
            StringBuilder result = new StringBuilder();
            Throwable cause = failure.getCause();
            while (cause != null) {
                if (result.length() != 0) result.append(" <- ");
                result.append(cause);
                cause = cause.getCause();
            }
            return result.toString();
        }
    }

    private static void check(boolean condition, String description) {
        if (!condition) throw new AssertionError(description);
    }

    private interface ThrowingRunnable {
        void run() throws Exception;
    }
}
