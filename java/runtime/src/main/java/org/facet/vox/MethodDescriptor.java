package org.facet.vox;

import java.util.Objects;
import org.facet.phon.PhonAdapter;

public final class MethodDescriptor {
    private final long id;
    private final String name;
    private final PhonAdapter<?> argumentAdapter;
    private final PhonAdapter<?> returnAdapter;
    private final PhonAdapter<?> applicationErrorAdapter;

    public MethodDescriptor(
            long id,
            String name,
            PhonAdapter<?> argumentAdapter,
            PhonAdapter<?> returnAdapter) {
        this(id, name, argumentAdapter, returnAdapter, null);
    }

    public MethodDescriptor(
            long id,
            String name,
            PhonAdapter<?> argumentAdapter,
            PhonAdapter<?> returnAdapter,
            PhonAdapter<?> applicationErrorAdapter) {
        this.id = id;
        this.name = Objects.requireNonNull(name, "name");
        this.argumentAdapter = Objects.requireNonNull(argumentAdapter, "argumentAdapter");
        this.returnAdapter = Objects.requireNonNull(returnAdapter, "returnAdapter");
        this.applicationErrorAdapter = applicationErrorAdapter;
    }

    public long id() { return id; }
    public String name() { return name; }
    public PhonAdapter<?> argumentAdapter() { return argumentAdapter; }
    public PhonAdapter<?> returnAdapter() { return returnAdapter; }
    public PhonAdapter<?> applicationErrorAdapter() { return applicationErrorAdapter; }
}
