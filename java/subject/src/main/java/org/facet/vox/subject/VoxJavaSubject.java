package org.facet.vox.subject;

import java.net.InetSocketAddress;
import java.net.ServerSocket;
import java.net.Socket;
import java.time.Duration;
import java.util.Map;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CompletionException;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.TimeoutException;
import org.facet.phon.PhonCodec;
import org.facet.phon.PhonLimits;
import org.facet.vox.CallOptions;
import org.facet.vox.ConnectionOptions;
import org.facet.vox.ConnectionState;
import org.facet.vox.LaneOptions;
import org.facet.vox.ServiceLane;
import org.facet.vox.ServiceRegistry;
import org.facet.vox.VoxConnection;
import org.facet.vox.VoxResult;
import org.facet.vox.generated.MathError;
import org.facet.vox.generated.TestbedClient;
import org.facet.vox.generated.TestbedDispatcher;
import org.facet.vox.generated.TestbedEchoResponse;
import org.facet.vox.generated.TestbedHandler;
import org.facet.vox.generated.TestbedServiceDescriptor;

/**
 * Hosted-subject lifecycle shell.
 *
 * <p>The first interoperable client scenario is Testbed.echo over TCP. Server-listen still
 * announces only its bound address, as the existing harness requires before it creates the peer.
 */
public final class VoxJavaSubject {
    private VoxJavaSubject() {}

    public static void main(String[] args) throws Exception {
        String mode = System.getenv().getOrDefault("SUBJECT_MODE", "server");
        long inactivitySeconds = parseLong(
                System.getenv("SUBJECT_INACTIVITY_TIMEOUT_SECS"), 60);
        ConnectionOptions options = ConnectionOptions.builder()
                .idleTimeout(Duration.ofSeconds(Math.max(1, inactivitySeconds)))
                .build();
        ExecutorService driver = Executors.newSingleThreadExecutor(runnable -> {
            Thread thread = new Thread(runnable, "vox-java-subject-driver");
            thread.setDaemon(true);
            return thread;
        });
        try {
            if ("server-listen".equals(mode)) {
                listenAndServe(options, driver);
            } else if ("server".equals(mode) || "client".equals(mode)) {
                connectAndDrive(mode, options, driver);
            } else {
                throw new IllegalArgumentException("unknown SUBJECT_MODE: " + mode);
            }
        } finally {
            driver.shutdownNow();
        }
    }

    private static void listenAndServe(ConnectionOptions options, ExecutorService driver)
            throws Exception {
        int port = (int) parseLong(System.getenv("LISTEN_PORT"), 0);
        try (ServerSocket listener = new ServerSocket()) {
            listener.bind(new InetSocketAddress("127.0.0.1", port));
            System.out.println("LISTEN_ADDR=127.0.0.1:" + listener.getLocalPort());
            System.out.flush();
            try (Socket socket = listener.accept();
                    VoxConnection connection =
                            VoxConnection.accept(
                                    socket,
                                    serviceRegistry(CompletableFuture.completedFuture(null)),
                                    options)) {
                await(connection, driver);
            }
        }
    }

    private static void connectAndDrive(
            String mode, ConnectionOptions options, ExecutorService driver)
            throws Exception {
        String peer = System.getenv("PEER_ADDR");
        if (peer == null || peer.isBlank()) {
            throw new IllegalArgumentException("PEER_ADDR env var not set");
        }
        if (peer.startsWith("tcp://")) peer = peer.substring("tcp://".length());
        int colon = peer.lastIndexOf(':');
        if (colon <= 0 || colon == peer.length() - 1) {
            throw new IllegalArgumentException("invalid PEER_ADDR: " + peer);
        }
        InetSocketAddress address =
                new InetSocketAddress(peer.substring(0, colon), Integer.parseInt(peer.substring(colon + 1)));
        boolean bidirectional =
                "1".equals(System.getenv("SUBJECT_BIDIRECTIONAL"));
        CompletableFuture<Void> serveReady = bidirectional
                ? new CompletableFuture<>()
                : CompletableFuture.completedFuture(null);
        try (VoxConnection connection =
                VoxConnection.connect(address, serviceRegistry(serveReady), options)) {
            CompletableFuture<Void> driven = connection.start(driver);
            awaitOpen(connection, driven);
            if ("client".equals(mode) || bidirectional) {
                String scenario = System.getenv().getOrDefault("CLIENT_SCENARIO", "echo");
                runClientScenario(connection, scenario);
                serveReady.complete(null);
            }
            if ("server".equals(mode)) driven.join();
        }
    }

    private static void runEcho(VoxConnection connection) throws Exception {
        ServiceLane lane =
                connection.openLane(TestbedServiceDescriptor.INSTANCE, LaneOptions.defaults());
        lane.opened().get(5, TimeUnit.SECONDS);
        TestbedClient client = new TestbedClient(lane);
        for (String message : new String[] {"hello from client", "hello again"}) {
            String result = client.echo(message).get(5, TimeUnit.SECONDS);
            if (!message.equals(result)) {
                throw new IllegalStateException("echo returned " + result);
            }
            System.err.println("echo result: " + result);
        }
        lane.close();
    }

    private static void runClientScenario(
            VoxConnection connection, String scenario) throws Exception {
        switch (scenario) {
            case "echo" -> runEcho(connection);
            case "cancel_timeout" -> runCancelTimeout(connection);
            case "invalid_payload" -> runInvalidPayload(connection);
            case "divide_error" -> runDivideError(connection);
            default -> throw new IllegalArgumentException(
                    "unsupported Java CLIENT_SCENARIO: " + scenario);
        }
    }

    private static ServiceLane openTestbed(VoxConnection connection) throws Exception {
        ServiceLane lane =
                connection.openLane(TestbedServiceDescriptor.INSTANCE, LaneOptions.defaults());
        lane.opened().get(5, TimeUnit.SECONDS);
        return lane;
    }

    private static void runCancelTimeout(VoxConnection connection) throws Exception {
        try (ServiceLane lane = openTestbed(connection)) {
            try {
                new TestbedClient(lane)
                        .echo(
                                "cancel-me",
                                new CallOptions(Duration.ofMillis(100), Map.of()))
                        .get(3, TimeUnit.SECONDS);
                throw new AssertionError("cancel-me unexpectedly completed");
            } catch (ExecutionException failure) {
                if (!hasCause(failure, TimeoutException.class)) throw failure;
            }
            Thread.sleep(100);
        }
    }

    private static void runInvalidPayload(VoxConnection connection) throws Exception {
        try (ServiceLane lane = openTestbed(connection)) {
            byte[] response = lane.call(
                            TestbedServiceDescriptor.ECHO,
                            new byte[] {0},
                            CallOptions.defaults())
                    .get(3, TimeUnit.SECONDS);
            VoxResult<String, Void> result = PhonCodec.decode(
                    TestbedEchoResponse.ADAPTER, response, PhonLimits.defaults());
            if (result.kind() != VoxResult.Kind.INVALID_PAYLOAD) {
                throw new AssertionError("expected INVALID_PAYLOAD, got " + result);
            }
        }
    }

    private static void runDivideError(VoxConnection connection) throws Exception {
        try (ServiceLane lane = openTestbed(connection)) {
            VoxResult<Long, MathError> result =
                    new TestbedClient(lane).divide(10, 0).get(3, TimeUnit.SECONDS);
            if (result.kind() != VoxResult.Kind.APPLICATION_ERROR
                    || result.applicationError() != MathError.DIVISION_BY_ZERO) {
                throw new AssertionError("unexpected divide result " + result);
            }
        }
    }

    private static boolean hasCause(Throwable failure, Class<? extends Throwable> type) {
        for (Throwable current = failure; current != null; current = current.getCause()) {
            if (type.isInstance(current)) return true;
        }
        return false;
    }

    private static ServiceRegistry serviceRegistry(CompletableFuture<Void> serveReady) {
        return new ServiceRegistry().register(new TestbedDispatcher(new TestbedHandler() {
            @Override
            public CompletableFuture<String> echo(
                    org.facet.vox.CallContext context, String message) {
                if ("disconnect-pending".equals(message)) {
                    return new CompletableFuture<>();
                }
                return serveReady.thenApply(ignored -> {
                    System.err.println("echo request: " + message);
                    return message;
                });
            }

            @Override
            public CompletableFuture<VoxResult<Long, MathError>> divide(
                    org.facet.vox.CallContext context, long dividend, long divisor) {
                if (divisor == 0) {
                    return CompletableFuture.completedFuture(
                            VoxResult.applicationError(MathError.DIVISION_BY_ZERO));
                }
                return CompletableFuture.completedFuture(
                        VoxResult.success(dividend / divisor));
            }
        }));
    }

    private static void awaitOpen(
            VoxConnection connection, CompletableFuture<Void> driven) throws Exception {
        long deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(10);
        while (connection.state() != ConnectionState.OPEN && System.nanoTime() < deadline) {
            if (driven.isDone()) driven.get();
            Thread.sleep(5);
        }
        if (connection.state() != ConnectionState.OPEN) {
            throw new IllegalStateException(
                    "connection did not open: " + connection.state());
        }
    }

    private static void await(VoxConnection connection, ExecutorService driver) throws Exception {
        try {
            connection.start(driver).join();
        } catch (CompletionException failure) {
            Throwable cause = failure.getCause();
            if (cause instanceof Exception exception) throw exception;
            throw failure;
        }
    }

    private static long parseLong(String value, long fallback) {
        return value == null ? fallback : Long.parseLong(value);
    }
}
