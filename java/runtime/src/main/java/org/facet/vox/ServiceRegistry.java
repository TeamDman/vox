package org.facet.vox;

import java.util.LinkedHashMap;
import java.util.Map;
import java.util.Objects;

public final class ServiceRegistry {
    private final Map<String, ServiceDispatcher> dispatchers = new LinkedHashMap<>();

    public synchronized ServiceRegistry register(ServiceDispatcher dispatcher) {
        Objects.requireNonNull(dispatcher, "dispatcher");
        String name = dispatcher.descriptor().name();
        if (dispatchers.putIfAbsent(name, dispatcher) != null) {
            throw new IllegalArgumentException("duplicate service identity: " + name);
        }
        return this;
    }

    synchronized ServiceDispatcher find(String serviceName) {
        return dispatchers.get(serviceName);
    }
}
