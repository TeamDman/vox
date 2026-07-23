package org.facet.phon;

/** Finite resource limits applied before allocation or recursive work. */
public final class PhonLimits {
    public static final PhonLimits DEFAULT = new PhonLimits(
            16 * 1024 * 1024, 1024 * 1024, 128, 1_000_000,
            16 * 1024 * 1024, 4096, 1_000_000);

    private final int inputBytes;
    private final int schemaBytes;
    private final int nestingDepth;
    private final int collectionEntries;
    private final int byteRunLength;
    private final int referencedSchemas;
    private final int planningWork;

    public PhonLimits(int inputBytes, int schemaBytes, int nestingDepth,
                      int collectionEntries, int byteRunLength,
                      int referencedSchemas, int planningWork) {
        this.inputBytes = positive("inputBytes", inputBytes);
        this.schemaBytes = positive("schemaBytes", schemaBytes);
        this.nestingDepth = positive("nestingDepth", nestingDepth);
        this.collectionEntries = positive("collectionEntries", collectionEntries);
        this.byteRunLength = positive("byteRunLength", byteRunLength);
        this.referencedSchemas = positive("referencedSchemas", referencedSchemas);
        this.planningWork = positive("planningWork", planningWork);
    }

    private static int positive(String name, int value) {
        if (value <= 0) throw new IllegalArgumentException(name + " must be positive");
        return value;
    }

    public int inputBytes() { return inputBytes; }
    public int schemaBytes() { return schemaBytes; }
    public int nestingDepth() { return nestingDepth; }
    public int collectionEntries() { return collectionEntries; }
    public int byteRunLength() { return byteRunLength; }
    public int referencedSchemas() { return referencedSchemas; }
    public int planningWork() { return planningWork; }
}
