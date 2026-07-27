package org.facet.vox;

import java.time.Duration;
import java.util.Objects;
import java.util.concurrent.Executor;
import java.util.concurrent.Executors;
import java.util.concurrent.ScheduledExecutorService;

/** Finite connection bounds and embedding-owned execution policy. */
public final class ConnectionOptions {
    private final int maxFrameBytes;
    private final int maxQueuedOutboundBytes;
    private final int maxQueuedOutboundMessages;
    private final int maxPendingRequests;
    private final int maxOpenLanes;
    private final int maxSchemaBytes;
    private final int maxSchemas;
    private final Duration handshakeTimeout;
    private final Duration idleTimeout;
    private final Duration closeTimeout;
    private final Executor handlerExecutor;
    private final ScheduledExecutorService scheduler;
    private final boolean ownsScheduler;

    private ConnectionOptions(Builder builder) {
        maxFrameBytes = positive(builder.maxFrameBytes, "maxFrameBytes");
        maxQueuedOutboundBytes = positive(builder.maxQueuedOutboundBytes, "maxQueuedOutboundBytes");
        maxQueuedOutboundMessages =
                positive(builder.maxQueuedOutboundMessages, "maxQueuedOutboundMessages");
        maxPendingRequests = positive(builder.maxPendingRequests, "maxPendingRequests");
        maxOpenLanes = positive(builder.maxOpenLanes, "maxOpenLanes");
        maxSchemaBytes = positive(builder.maxSchemaBytes, "maxSchemaBytes");
        maxSchemas = positive(builder.maxSchemas, "maxSchemas");
        handshakeTimeout = positive(builder.handshakeTimeout, "handshakeTimeout");
        idleTimeout = positive(builder.idleTimeout, "idleTimeout");
        closeTimeout = positive(builder.closeTimeout, "closeTimeout");
        handlerExecutor = Objects.requireNonNull(builder.handlerExecutor, "handlerExecutor");
        if (builder.scheduler == null) {
            scheduler = Executors.newSingleThreadScheduledExecutor(runnable -> {
                Thread thread = new Thread(runnable, "vox-java-timeouts");
                thread.setDaemon(true);
                return thread;
            });
            ownsScheduler = true;
        } else {
            scheduler = builder.scheduler;
            ownsScheduler = false;
        }
    }

    public static Builder builder() {
        return new Builder();
    }

    public static ConnectionOptions defaults() {
        return builder().build();
    }

    public int maxFrameBytes() { return maxFrameBytes; }
    public int maxQueuedOutboundBytes() { return maxQueuedOutboundBytes; }
    public int maxQueuedOutboundMessages() { return maxQueuedOutboundMessages; }
    public int maxPendingRequests() { return maxPendingRequests; }
    public int maxOpenLanes() { return maxOpenLanes; }
    public int maxSchemaBytes() { return maxSchemaBytes; }
    public int maxSchemas() { return maxSchemas; }
    public Duration handshakeTimeout() { return handshakeTimeout; }
    public Duration idleTimeout() { return idleTimeout; }
    public Duration closeTimeout() { return closeTimeout; }
    public Executor handlerExecutor() { return handlerExecutor; }
    public ScheduledExecutorService scheduler() { return scheduler; }

    void closeOwnedResources() {
        if (ownsScheduler) {
            scheduler.shutdownNow();
        }
    }

    private static int positive(int value, String name) {
        if (value <= 0) throw new IllegalArgumentException(name + " must be positive");
        return value;
    }

    private static Duration positive(Duration value, String name) {
        Objects.requireNonNull(value, name);
        if (value.isZero() || value.isNegative()) {
            throw new IllegalArgumentException(name + " must be positive");
        }
        return value;
    }

    public static final class Builder {
        private int maxFrameBytes = 16 * 1024 * 1024;
        private int maxQueuedOutboundBytes = 16 * 1024 * 1024;
        private int maxQueuedOutboundMessages = 128;
        private int maxPendingRequests = 1_024;
        private int maxOpenLanes = 128;
        private int maxSchemaBytes = 4 * 1024 * 1024;
        private int maxSchemas = 4_096;
        private Duration handshakeTimeout = Duration.ofSeconds(10);
        private Duration idleTimeout = Duration.ofSeconds(30);
        private Duration closeTimeout = Duration.ofSeconds(5);
        private Executor handlerExecutor = Runnable::run;
        private ScheduledExecutorService scheduler;

        public Builder maxFrameBytes(int value) { maxFrameBytes = value; return this; }
        public Builder maxQueuedOutboundBytes(int value) { maxQueuedOutboundBytes = value; return this; }
        public Builder maxQueuedOutboundMessages(int value) { maxQueuedOutboundMessages = value; return this; }
        public Builder maxPendingRequests(int value) { maxPendingRequests = value; return this; }
        public Builder maxOpenLanes(int value) { maxOpenLanes = value; return this; }
        public Builder maxSchemaBytes(int value) { maxSchemaBytes = value; return this; }
        public Builder maxSchemas(int value) { maxSchemas = value; return this; }
        public Builder handshakeTimeout(Duration value) { handshakeTimeout = value; return this; }
        public Builder idleTimeout(Duration value) { idleTimeout = value; return this; }
        public Builder closeTimeout(Duration value) { closeTimeout = value; return this; }
        public Builder handlerExecutor(Executor value) { handlerExecutor = value; return this; }
        public Builder scheduler(ScheduledExecutorService value) { scheduler = value; return this; }
        public ConnectionOptions build() { return new ConnectionOptions(this); }
    }
}
