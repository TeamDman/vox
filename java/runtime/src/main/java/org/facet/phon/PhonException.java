package org.facet.phon;

/** Checked failure from schema processing or Phon wire I/O. */
public class PhonException extends Exception {
    private static final long serialVersionUID = 1L;
    public enum Kind {
        SCHEMA, PLANNING, ENCODE, DECODE, TRUNCATED, MALFORMED, INCOMPATIBLE, LIMIT
    }

    private final Kind kind;
    private final int byteOffset;
    private final String schemaPath;

    public PhonException(Kind kind, String message) {
        this(kind, message, -1, null, null);
    }

    public PhonException(Kind kind, String message, int byteOffset, String schemaPath) {
        this(kind, message, byteOffset, schemaPath, null);
    }

    public PhonException(Kind kind, String message, Throwable cause) {
        this(kind, message, -1, null, cause);
    }

    private PhonException(Kind kind, String message, int byteOffset, String schemaPath, Throwable cause) {
        super(message, cause);
        this.kind = kind;
        this.byteOffset = byteOffset;
        this.schemaPath = schemaPath;
    }

    public Kind kind() { return kind; }
    public int byteOffset() { return byteOffset; }
    public String schemaPath() { return schemaPath; }
}
