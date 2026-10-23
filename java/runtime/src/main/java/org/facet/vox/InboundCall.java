package org.facet.vox;

import java.util.Objects;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.List;
import org.facet.phon.PhonAdapter;
import org.facet.phon.PhonCodec;
import org.facet.phon.PhonException;
import org.facet.phon.PhonLimits;

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
    private final List<Object> channels;
    private final AtomicBoolean terminal = new AtomicBoolean();

    InboundCall(
            long requestId,
            MethodDescriptor method,
            byte[] encodedArguments,
            CallContext context,
            Reply reply) {
        this(requestId, method, encodedArguments, context, reply, List.of());
    }

    InboundCall(
            long requestId,
            MethodDescriptor method,
            byte[] encodedArguments,
            CallContext context,
            Reply reply,
            List<Object> channels) {
        this.requestId = requestId;
        this.method = Objects.requireNonNull(method, "method");
        this.encodedArguments = encodedArguments.clone();
        this.context = Objects.requireNonNull(context, "context");
        this.reply = Objects.requireNonNull(reply, "reply");
        this.channels = List.copyOf(Objects.requireNonNull(channels, "channels"));
    }

    public long requestId() { return requestId; }
    public MethodDescriptor method() { return method; }
    public byte[] encodedArguments() { return encodedArguments.clone(); }
    public CallContext context() { return context; }
    boolean isTerminal() { return terminal.get(); }

    public <T> T decodeArguments(PhonAdapter<T> adapter) throws PhonException {
        try {
            return VoxChannelDecoding.with(
                    this,
                    () -> PhonCodec.decode(adapter, encodedArguments, PhonLimits.defaults()));
        } catch (PhonException failure) {
            throw failure;
        } catch (Exception failure) {
            throw new PhonException(PhonException.Kind.DECODE,
                    "cannot decode channel arguments", failure);
        }
    }

    VoxTx<?> tx(long index) throws PhonException {
        Object endpoint = channel(index);
        if (endpoint instanceof VoxTx<?> tx) return tx;
        throw new PhonException(PhonException.Kind.DECODE,
                "channel table entry " + index + " is not Tx");
    }

    VoxRx<?> rx(long index) throws PhonException {
        Object endpoint = channel(index);
        if (endpoint instanceof VoxRx<?> rx) return rx;
        throw new PhonException(PhonException.Kind.DECODE,
                "channel table entry " + index + " is not Rx");
    }

    private Object channel(long index) throws PhonException {
        if (index < 0 || index >= channels.size()) {
            throw new PhonException(PhonException.Kind.DECODE,
                    "channel table index " + index + " out of range " + channels.size());
        }
        return channels.get((int) index);
    }

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
