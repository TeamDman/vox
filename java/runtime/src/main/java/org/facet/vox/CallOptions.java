package org.facet.vox;

import java.time.Duration;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.Objects;

public final class CallOptions {
    private final Duration idleTimeout;
    private final Map<String, String> metadata;

    public CallOptions(Duration idleTimeout, Map<String, String> metadata) {
        this.idleTimeout = Objects.requireNonNull(idleTimeout, "idleTimeout");
        if (idleTimeout.isZero() || idleTimeout.isNegative()) {
            throw new IllegalArgumentException("idleTimeout must be positive");
        }
        this.metadata = Map.copyOf(new LinkedHashMap<>(
                Objects.requireNonNull(metadata, "metadata")));
    }

    public static CallOptions withIdleTimeout(Duration timeout) {
        return new CallOptions(timeout, Map.of());
    }

    public Duration idleTimeout() { return idleTimeout; }
    public Map<String, String> metadata() { return metadata; }
}
