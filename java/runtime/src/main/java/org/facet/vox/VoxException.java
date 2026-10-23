package org.facet.vox;

/** Checked protocol/runtime failure. */
public class VoxException extends Exception {
    private static final long serialVersionUID = 1L;

    public VoxException(String message) {
        super(message);
    }

    public VoxException(String message, Throwable cause) {
        super(message, cause);
    }
}
