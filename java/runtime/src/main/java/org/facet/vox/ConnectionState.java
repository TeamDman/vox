package org.facet.vox;

public enum ConnectionState {
    NEW,
    TRANSPORT_NEGOTIATING,
    HANDSHAKING,
    OPEN,
    CLOSING,
    CLOSED,
    FAILED
}
