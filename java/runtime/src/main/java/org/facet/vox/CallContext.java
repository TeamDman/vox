package org.facet.vox;

import java.util.Map;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.atomic.AtomicBoolean;

public final class CallContext {
    private final long requestId;
    private final long laneId;
    private final Map<String, String> metadata;
    private final AtomicBoolean cancelled = new AtomicBoolean();
    private final CompletableFuture<Void> cancellation = new CompletableFuture<>();

    CallContext(long requestId, long laneId, Map<String, String> metadata) {
        this.requestId = requestId;
        this.laneId = laneId;
        this.metadata = Map.copyOf(metadata);
    }

    public long requestId() { return requestId; }
    public long laneId() { return laneId; }
    public Map<String, String> metadata() { return metadata; }
    public boolean isCancelled() { return cancelled.get(); }
    public CompletableFuture<Void> cancellation() { return cancellation; }

    void cancel() {
        if (cancelled.compareAndSet(false, true)) {
            cancellation.complete(null);
        }
    }
}
