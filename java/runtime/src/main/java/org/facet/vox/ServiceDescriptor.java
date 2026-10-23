package org.facet.vox;

import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Objects;

public final class ServiceDescriptor {
    private final String name;
    private final List<MethodDescriptor> methods;
    private final Map<Long, MethodDescriptor> methodsById;

    public ServiceDescriptor(String name, List<MethodDescriptor> methods) {
        this.name = Objects.requireNonNull(name, "name");
        this.methods = List.copyOf(Objects.requireNonNull(methods, "methods"));
        Map<Long, MethodDescriptor> indexed = new LinkedHashMap<>();
        for (MethodDescriptor method : this.methods) {
            if (indexed.put(method.id(), method) != null) {
                throw new IllegalArgumentException(
                        "duplicate method id " + Long.toUnsignedString(method.id())
                                + " in service " + name);
            }
        }
        methodsById = Map.copyOf(indexed);
    }

    public String name() { return name; }
    public List<MethodDescriptor> methods() { return methods; }
    public MethodDescriptor method(long id) { return methodsById.get(id); }
}
