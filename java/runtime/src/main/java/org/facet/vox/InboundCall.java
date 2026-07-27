package org.facet.vox;

import java.util.Objects;
import java.util.concurrent.atomic.AtomicBoolean;

public final class InboundCall {
    interface Reply {
        void success(byte[] bytes);
        void failure(VoxException failure);
    }

    private final long requestId;
    private final MethodDescriptor method;
    private final byte[] encodedArguments;
    private final CallContext context;
    private final Reply reply;
    private final AtomicBoolean terminal = new AtomicBoolean();

    InboundCall(
            long requestId,
            MethodDescriptor method,
            byte[] encodedArguments,
            CallContext context,
            Reply reply) {
        this.requestId = requestId;
        this.method = Objects.requireNonNull(method, "method");
        this.encodedArguments = encodedArguments.clone();
        this.context = Objects.requireNonNull(context, "context");
        this.reply = Objects.requireNonNull(reply, "reply");
    }

    public long requestId() { return requestId; }
    public MethodDescriptor method() { return method; }
    public byte[] encodedArguments() { return encodedArguments.clone(); }
    public CallContext context() { return context; }
    boolean isTerminal() { return terminal.get(); }

    public void respond(byte[] encodedResponse) {
        Objects.requireNonNull(encodedResponse, "encodedResponse");
        if (!terminal.compareAndSet(false, true)) {
            throw new IllegalStateException("request already terminal");
        }
        reply.success(encodedResponse.clone());
    }

    public void fail(VoxException failure) {
        Objects.requireNonNull(failure, "failure");
        if (!terminal.compareAndSet(false, true)) {
            throw new IllegalStateException("request already terminal");
        }
        reply.failure(failure);
    }
}
