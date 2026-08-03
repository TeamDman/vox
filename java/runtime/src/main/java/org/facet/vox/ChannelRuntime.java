package org.facet.vox;

import java.time.Duration;
import java.util.ArrayDeque;
import java.util.Objects;
import org.facet.phon.PhonAdapter;
import org.facet.phon.PhonCodec;
import org.facet.phon.PhonException;
import org.facet.phon.PhonLimits;
import org.facet.phon.SchemaClosure;

/** Package-private bounded channel state shared by generated bindings and the connection driver. */
final class ChannelRuntime {
    interface Transport {
        boolean item(long laneId, long channelId, byte[] payload);
        boolean close(long laneId, long channelId);
        boolean reset(long laneId, long channelId);
        boolean grant(long laneId, long channelId, int additional);
    }

    static final class Core<T> {
        final PhonAdapter<T> adapter;
        private Sender<T> sender;
        private Receiver<T> receiver;
        private boolean senderClosedBeforeBinding;
        private boolean receiverResetBeforeBinding;

        Core(PhonAdapter<T> adapter) { this.adapter = Objects.requireNonNull(adapter, "adapter"); }

        synchronized void bindSender(Sender<T> value) throws VoxException {
            if (sender != null) throw new VoxException("Tx channel handle already consumed");
            sender = value;
            if (senderClosedBeforeBinding) sender.close();
            notifyAll();
        }

        synchronized void bindReceiver(Receiver<T> value) throws VoxException {
            if (receiver != null) throw new VoxException("Rx channel handle already consumed");
            receiver = value;
            if (receiverResetBeforeBinding) receiver.reset();
            notifyAll();
        }

        synchronized boolean hasSender() { return sender != null; }
        synchronized boolean hasReceiver() { return receiver != null; }

        synchronized Sender<T> sender() throws InterruptedException, VoxException {
            while (sender == null && !senderClosedBeforeBinding) wait();
            if (sender == null) throw new VoxException("Tx channel closed before binding");
            return sender;
        }

        synchronized Sender<T> trySender() { return sender; }

        synchronized Receiver<T> receiver() throws InterruptedException, VoxException {
            while (receiver == null && !receiverResetBeforeBinding) wait();
            if (receiver == null) throw new VoxException("Rx channel reset before binding");
            return receiver;
        }

        synchronized void closeSender() {
            if (sender == null) senderClosedBeforeBinding = true;
            else sender.close();
            notifyAll();
        }

        synchronized void resetReceiver() {
            if (receiver == null) receiverResetBeforeBinding = true;
            else receiver.reset();
            notifyAll();
        }
    }

    static final class Sender<T> {
        private final long laneId;
        private final long channelId;
        private final PhonAdapter<T> adapter;
        private final Transport transport;
        private int credit;
        private boolean closed;
        private VoxException failure;

        Sender(long laneId, long channelId, PhonAdapter<T> adapter, Transport transport, int credit) {
            this.laneId = laneId;
            this.channelId = channelId;
            this.adapter = adapter;
            this.transport = transport;
            this.credit = credit;
        }

        void send(T value) throws InterruptedException, VoxException {
            byte[] payload = encode(value);
            synchronized (this) {
                while (credit == 0 && !closed && failure == null) wait();
                requireOpen();
                credit--;
            }
            if (!transport.item(laneId, channelId, payload)) {
                synchronized (this) {
                    credit++;
                    notifyAll();
                }
                throw new VoxException("outbound channel queue is full");
            }
        }

        VoxTx.TrySendResult<T> trySend(T value) throws VoxException {
            byte[] payload;
            synchronized (this) {
                if (closed || failure != null) return new VoxTx.TrySendResult.Closed<>(value);
                if (credit == 0) return new VoxTx.TrySendResult.Full<>(value);
                payload = encode(value);
                credit--;
            }
            if (!transport.item(laneId, channelId, payload)) {
                synchronized (this) { credit++; notifyAll(); }
                return new VoxTx.TrySendResult.Full<>(value);
            }
            return new VoxTx.TrySendResult.Sent<>();
        }

        synchronized void grant(int additional) throws VoxException {
            if (additional <= 0) throw new VoxException("channel credit grant must be positive");
            if (closed || failure != null) return;
            if (credit > Integer.MAX_VALUE - additional) {
                throw new VoxException("channel credit overflow");
            }
            credit += additional;
            notifyAll();
        }

        synchronized void close() {
            if (closed) return;
            closed = true;
            transport.close(laneId, channelId);
            notifyAll();
        }

        synchronized void terminate(VoxException reason) {
            if (failure == null) failure = reason;
            closed = true;
            notifyAll();
        }

        private byte[] encode(T value) throws VoxException {
            try {
                return PhonCodec.encode(adapter, value, PhonLimits.defaults());
            } catch (PhonException failure) {
                throw new VoxException("cannot encode channel item", failure);
            }
        }

        private void requireOpen() throws VoxException {
            if (failure != null) throw failure;
            if (closed) throw new VoxException("channel sender is closed");
        }
    }

    static final class Receiver<T> {
        private final long laneId;
        private final long channelId;
        private final PhonAdapter<T> adapter;
        private final Transport transport;
        private final int capacity;
        private final ArrayDeque<T> queue = new ArrayDeque<>();
        private boolean gracefulClose;
        private VoxException failure;
        private boolean reset;

        Receiver(long laneId, long channelId, PhonAdapter<T> adapter, Transport transport, int capacity) {
            this.laneId = laneId;
            this.channelId = channelId;
            this.adapter = adapter;
            this.transport = transport;
            this.capacity = capacity;
        }

        synchronized void item(byte[] payload, SchemaClosure writer) throws VoxException {
            if (gracefulClose || failure != null || reset) {
                throw new VoxException("item received for terminal channel");
            }
            if (queue.size() >= capacity) {
                throw new VoxException("peer exceeded channel credit");
            }
            try {
                byte[] local = PhonCodec.transcode(writer, adapter.schema(), payload, PhonLimits.defaults());
                queue.addLast(PhonCodec.decode(adapter, local, PhonLimits.defaults()));
            } catch (PhonException decodeFailure) {
                throw new VoxException("cannot decode channel item", decodeFailure);
            }
            notifyAll();
        }

        synchronized T receive(Duration timeout)
                throws InterruptedException, VoxException, ReceiveTimeout {
            long remaining = timeout == null ? 0L : timeout.toNanos();
            long started = System.nanoTime();
            while (queue.isEmpty() && !gracefulClose && failure == null && !reset) {
                if (timeout == null) {
                    wait();
                } else {
                    if (remaining <= 0) throw new ReceiveTimeout("timed out waiting for channel item");
                    long millis = Math.max(1L, remaining / 1_000_000L);
                    wait(millis);
                    remaining = timeout.toNanos() - (System.nanoTime() - started);
                }
            }
            if (!queue.isEmpty()) {
                T value = queue.removeFirst();
                // Once a graceful close has arrived, all remaining items are
                // already buffered and the sender cannot use replenished credit.
                if (!gracefulClose && !transport.grant(laneId, channelId, 1)) {
                    terminate(new VoxException("outbound channel credit queue is full"));
                    throw failure;
                }
                return value;
            }
            if (failure != null) throw failure;
            if (reset) throw new VoxException("channel receiver was reset");
            return null;
        }

        synchronized void closeGracefully() { gracefulClose = true; notifyAll(); }

        synchronized void resetByPeer() {
            failure = new VoxException("peer reset channel");
            notifyAll();
        }

        synchronized void reset() {
            if (reset || gracefulClose || failure != null) return;
            reset = true;
            transport.reset(laneId, channelId);
            notifyAll();
        }

        synchronized void terminate(VoxException reason) {
            if (!gracefulClose && failure == null) failure = reason;
            notifyAll();
        }

        PhonAdapter<T> adapter() { return adapter; }
    }

    static final class ReceiveTimeout extends Exception {
        private static final long serialVersionUID = 1L;
        ReceiveTimeout(String message) { super(message); }
    }

    private ChannelRuntime() {}
}
