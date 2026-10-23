package org.facet.vox;

import java.util.Objects;
import java.util.concurrent.CompletableFuture;
import java.util.function.Function;

/** Future composition helpers that preserve Vox request cancellation. */
public final class VoxFutures {
    public static <T, U> CompletableFuture<U> mapCancellable(
            CompletableFuture<T> source, Function<? super T, ? extends U> mapper) {
        Objects.requireNonNull(source, "source");
        Objects.requireNonNull(mapper, "mapper");
        CancellableFuture<U> target = new CancellableFuture<>(source);
        source.whenComplete((value, failure) -> {
            if (failure != null) {
                target.completeExceptionally(failure);
                return;
            }
            try {
                target.complete(mapper.apply(value));
            } catch (Throwable mappingFailure) {
                target.completeExceptionally(mappingFailure);
            }
        });
        return target;
    }

    private static final class CancellableFuture<T> extends CompletableFuture<T> {
        private final CompletableFuture<?> source;

        CancellableFuture(CompletableFuture<?> source) { this.source = source; }

        @Override public boolean cancel(boolean mayInterruptIfRunning) {
            boolean cancelled = super.cancel(false);
            if (cancelled) source.cancel(false);
            return cancelled;
        }
    }

    private VoxFutures() {}
}
