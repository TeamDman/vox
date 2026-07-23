package org.facet.vox;

import java.util.Objects;

/** Declared application result; transport and protocol failures remain exceptional. */
public final class VoxResult<O, E> {
    public enum Kind {
        SUCCESS,
        APPLICATION_ERROR,
        UNKNOWN_METHOD,
        INVALID_PAYLOAD,
        CANCELLED,
        CONNECTION_CLOSED,
        CONNECTION_SHUTDOWN,
        SEND_FAILED,
        TIMED_OUT,
        INDETERMINATE
    }
    private final O success;
    private final E applicationError;
    private final Kind kind;
    private final String detail;

    private VoxResult(O success, E applicationError, Kind kind, String detail) {
        this.success = success;
        this.applicationError = applicationError;
        this.kind = kind;
        this.detail = detail;
    }

    public static <O, E> VoxResult<O, E> success(O value) {
        return new VoxResult<>(Objects.requireNonNull(value, "value"), null, Kind.SUCCESS, null);
    }

    public static <O, E> VoxResult<O, E> applicationError(E error) {
        return new VoxResult<>(null, Objects.requireNonNull(error, "error"),
                Kind.APPLICATION_ERROR, null);
    }

    public static <O, E> VoxResult<O, E> infrastructure(Kind kind) {
        return infrastructure(kind, null);
    }
    public static <O, E> VoxResult<O, E> infrastructure(Kind kind, String detail) {
        Objects.requireNonNull(kind, "kind");
        if (kind == Kind.SUCCESS || kind == Kind.APPLICATION_ERROR) {
            throw new IllegalArgumentException("not an infrastructure error kind");
        }
        return new VoxResult<>(null, null, kind, detail);
    }

    public Kind kind() { return kind; }
    public boolean isSuccess() { return kind == Kind.SUCCESS; }
    public boolean isApplicationError() { return kind == Kind.APPLICATION_ERROR; }
    public boolean isInfrastructureError() { return !isSuccess() && !isApplicationError(); }
    public String detail() { return detail; }
    public O success() {
        if (!isSuccess()) throw new IllegalStateException("result is not successful");
        return success;
    }
    public E applicationError() {
        if (!isApplicationError()) throw new IllegalStateException("result is not an application error");
        return applicationError;
    }
}
