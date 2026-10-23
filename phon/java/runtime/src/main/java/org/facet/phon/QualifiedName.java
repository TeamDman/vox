package org.facet.phon;

import java.util.Objects;

/** Canonical revision-one Phon semantic name; no normalization is performed. */
public final class QualifiedName {
    public static final int MAX_BYTES = 255;
    private final String value;

    public QualifiedName(String value) {
        Objects.requireNonNull(value, "value");
        if (value.isEmpty() || value.length() > MAX_BYTES)
            throw new IllegalArgumentException("qualified name length");
        String[] labels = value.split("\\.", -1);
        if (labels.length < 2) throw new IllegalArgumentException("qualified name needs two labels");
        for (String label : labels) {
            if (label.isEmpty() || label.charAt(0) < 'a' || label.charAt(0) > 'z'
                    || label.endsWith("-") || label.contains("--"))
                throw new IllegalArgumentException("invalid qualified name label");
            for (int i = 0; i < label.length(); i++) {
                char c = label.charAt(i);
                if (!(c >= 'a' && c <= 'z') && !(c >= '0' && c <= '9') && c != '-')
                    throw new IllegalArgumentException("invalid qualified name character");
            }
        }
        this.value = value;
    }

    public static QualifiedName application(String value) {
        QualifiedName name = new QualifiedName(value);
        if (value.startsWith("phon.") || value.startsWith("org.bearcove.phon."))
            throw new IllegalArgumentException("reserved qualified name namespace");
        return name;
    }

    public String value() { return value; }
    @Override public String toString() { return value; }
    @Override public boolean equals(Object other) {
        return other instanceof QualifiedName && value.equals(((QualifiedName) other).value);
    }
    @Override public int hashCode() { return value.hashCode(); }
}
