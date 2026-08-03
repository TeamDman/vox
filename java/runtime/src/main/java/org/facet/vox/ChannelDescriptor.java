package org.facet.vox;

import java.util.Objects;
import org.facet.phon.PhonAdapter;

/** Generated metadata for one direct {@code Tx<T>} or {@code Rx<T>} method argument. */
public final class ChannelDescriptor {
    public enum Direction { TX, RX }

    private final int argumentIndex;
    private final Direction direction;
    private final String role;
    private final PhonAdapter<?> elementAdapter;

    public ChannelDescriptor(
            int argumentIndex,
            Direction direction,
            String role,
            PhonAdapter<?> elementAdapter) {
        if (argumentIndex < 0) throw new IllegalArgumentException("argumentIndex must be nonnegative");
        this.argumentIndex = argumentIndex;
        this.direction = Objects.requireNonNull(direction, "direction");
        this.role = Objects.requireNonNull(role, "role");
        this.elementAdapter = Objects.requireNonNull(elementAdapter, "elementAdapter");
    }

    public int argumentIndex() { return argumentIndex; }
    public Direction direction() { return direction; }
    public String role() { return role; }
    public PhonAdapter<?> elementAdapter() { return elementAdapter; }
}
