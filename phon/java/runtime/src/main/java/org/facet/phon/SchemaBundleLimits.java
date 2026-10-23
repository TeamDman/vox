package org.facet.phon;

/** Caller-controlled bounds for canonical PHONSCM1 schema bundles. */
public final class SchemaBundleLimits {
    public static final SchemaBundleLimits DEFAULT = new SchemaBundleLimits(
            64 * 1024 * 1024, 16 * 1024 * 1024, 1_000_000, 4096, 128, 1_000_000);
    private final int totalBytes, ownedBytes, strings, schemas, nesting, work;
    public SchemaBundleLimits(int totalBytes, int ownedBytes, int strings, int schemas, int nesting, int work) {
        if (totalBytes <= 0 || ownedBytes <= 0 || strings <= 0 || schemas <= 0 || nesting <= 0 || work <= 0)
            throw new IllegalArgumentException("schema bundle limits must be positive");
        this.totalBytes=totalBytes;this.ownedBytes=ownedBytes;this.strings=strings;
        this.schemas=schemas;this.nesting=nesting;this.work=work;
    }
    public int totalBytes(){return totalBytes;}
    public int ownedBytes(){return ownedBytes;}
    public int strings(){return strings;}
    public int schemas(){return schemas;}
    public int nesting(){return nesting;}
    public int work(){return work;}
    PhonLimits phonLimits(){return new PhonLimits(totalBytes,totalBytes,nesting,work,totalBytes,schemas,work);}
}
