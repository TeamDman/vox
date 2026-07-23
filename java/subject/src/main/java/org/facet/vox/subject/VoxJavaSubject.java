package org.facet.vox.subject;

import java.net.InetSocketAddress;
import java.net.ServerSocket;
import java.net.Socket;
import java.time.Duration;
import java.util.concurrent.CompletionException;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import org.facet.vox.ConnectionOptions;
import org.facet.vox.ServiceRegistry;
import org.facet.vox.VoxConnection;

/**
 * Hosted-subject lifecycle shell.
 *
 * <p>It intentionally cannot announce a usable Vox connection until the Phon handshake and
 * generated Testbed dispatcher integrate. Server-listen announces only its bound address, as the
 * existing harness requires before it can create the peer.
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
                connectAndDrive(options, driver);
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
                            VoxConnection.accept(socket, new ServiceRegistry(), options)) {
                await(connection, driver);
            }
        }
    }

    private static void connectAndDrive(ConnectionOptions options, ExecutorService driver)
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
        try (VoxConnection connection = VoxConnection.connect(address, options)) {
            await(connection, driver);
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
