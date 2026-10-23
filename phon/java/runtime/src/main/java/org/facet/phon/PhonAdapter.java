package org.facet.phon;

/** Explicit, reflection-free adapter between an application type and Phon. */
public interface PhonAdapter<T> {
    SchemaClosure schema();
    void encode(PhonEncoder encoder, T value) throws PhonException;
    T decode(PhonDecoder decoder) throws PhonException;
}
