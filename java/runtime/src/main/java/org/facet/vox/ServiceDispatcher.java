package org.facet.vox;

import java.util.concurrent.CompletableFuture;

public interface ServiceDispatcher {
    ServiceDescriptor descriptor();
    CompletableFuture<Void> dispatch(InboundCall call);
}
