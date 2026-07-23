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
import java.util.concurrent.TimeUnit;
import java.util.concurrent.TimeoutException;
import java.util.concurrent.atomic.AtomicInteger;
import org.facet.phon.PhonAdapter;
import org.facet.phon.PhonDecoder;
import org.facet.phon.PhonEncoder;
import org.facet.phon.PhonException;
import org.facet.phon.Schema;
import org.facet.phon.SchemaClosure;

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
        laneCancellationAndTimeoutAreTerminal();
        inboundCallIsExactlyOnce();
        connectionDriverOwnershipAndHandshakeSeam();
        System.out.println("VoxRuntimeTest: PASS");
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

    private static void connectionDriverOwnershipAndHandshakeSeam() throws Exception {
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
            expectThrows(ExecutionException.class, () -> serverDone.get(2, TimeUnit.SECONDS));
            ExecutionException clientFailure = expectThrows(
                    ExecutionException.class, () -> clientDone.get(2, TimeUnit.SECONDS));
            check(clientFailure.getCause().getMessage().contains("Phon self-describing"),
                    "explicit handshake integration seam");
            check(client.state() == ConnectionState.FAILED, "terminal failed state");
            CompletableFuture<Void> duplicate = client.start(Runnable::run);
            expectThrows(ExecutionException.class, duplicate::get);
            client.close();
            server.close();
            acceptThread.join();
        }
    }

    private static ServiceLane lane(
            ServiceLane.DriverCommands driver,
            ConnectionOptions options,
            MethodDescriptor method) {
        ServiceDescriptor service = new ServiceDescriptor("echo", List.of(method));
        return new ServiceLane(1, service, driver, options, LaneState.OPEN);
    }

    private static MethodDescriptor method() {
        return new MethodDescriptor(0x8000_0000_0000_0001L, "echo", ADAPTER, ADAPTER);
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
        public void cancel(long laneId, long requestId) { cancels.incrementAndGet(); }
        public void closeLane(long laneId) {}
        ServiceLane.OutboundCall take() throws Exception {
            ServiceLane.OutboundCall value = submitted.poll(1, TimeUnit.SECONDS);
            if (value == null) throw new TimeoutException("no submitted call");
            return value;
        }
    }

    private static final class RejectingDriver implements ServiceLane.DriverCommands {
        public boolean submit(ServiceLane.OutboundCall call) { return false; }
        public void cancel(long laneId, long requestId) {}
        public void closeLane(long laneId) {}
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

    private static void check(boolean condition, String description) {
        if (!condition) throw new AssertionError(description);
    }

    private interface ThrowingRunnable {
        void run() throws Exception;
    }
}
