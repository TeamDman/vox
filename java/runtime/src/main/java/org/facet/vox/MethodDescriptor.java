package org.facet.vox;

import java.util.Objects;
import java.util.List;
import org.facet.phon.PhonAdapter;

public final class MethodDescriptor {
    private final long id;
    private final String name;
    private final PhonAdapter<?> argumentAdapter;
    private final PhonAdapter<?> returnAdapter;
    private final PhonAdapter<?> applicationErrorAdapter;
    private final PhonAdapter<?> responseWireAdapter;
    private final List<ChannelDescriptor> channels;

    public MethodDescriptor(
            long id,
            String name,
            PhonAdapter<?> argumentAdapter,
            PhonAdapter<?> returnAdapter,
            PhonAdapter<?> applicationErrorAdapter,
            PhonAdapter<?> responseWireAdapter) {
        this(id, name, argumentAdapter, returnAdapter, applicationErrorAdapter,
                responseWireAdapter, List.of());
    }

    public MethodDescriptor(
            long id,
            String name,
            PhonAdapter<?> argumentAdapter,
            PhonAdapter<?> returnAdapter,
            PhonAdapter<?> applicationErrorAdapter,
            PhonAdapter<?> responseWireAdapter,
            List<ChannelDescriptor> channels) {
        this.id = id;
        this.name = Objects.requireNonNull(name, "name");
        this.argumentAdapter = Objects.requireNonNull(argumentAdapter, "argumentAdapter");
        this.returnAdapter = Objects.requireNonNull(returnAdapter, "returnAdapter");
        this.applicationErrorAdapter = applicationErrorAdapter;
        this.responseWireAdapter = Objects.requireNonNull(responseWireAdapter, "responseWireAdapter");
        this.channels = List.copyOf(Objects.requireNonNull(channels, "channels"));
    }

    public long id() { return id; }
    public String name() { return name; }
    public PhonAdapter<?> argumentAdapter() { return argumentAdapter; }
    public PhonAdapter<?> returnAdapter() { return returnAdapter; }
    public PhonAdapter<?> applicationErrorAdapter() { return applicationErrorAdapter; }
    public PhonAdapter<?> responseWireAdapter() { return responseWireAdapter; }
    public List<ChannelDescriptor> channels() { return channels; }
}
