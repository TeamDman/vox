package org.facet.vox;

import java.util.LinkedHashMap;
import java.util.Map;
import java.util.Objects;

public final class LaneOptions {
    private final Map<String, String> metadata;

    public LaneOptions(Map<String, String> metadata) {
        this.metadata = Map.copyOf(new LinkedHashMap<>(
                Objects.requireNonNull(metadata, "metadata")));
    }

    public static LaneOptions defaults() { return new LaneOptions(Map.of()); }
    public Map<String, String> metadata() { return metadata; }
}
