# Full Phon/Taxon cutover: unfinished cloud handoff

Checkpoint date: 2026-10-23. Branch:
`migration/phon-java-cutover-2026-10-23` in `TeamDman/vox`.
This is a preservation checkpoint, **not a validated consumer revision**.
The user requested a full upgrade; old and new peers must upgrade together.
The work stopped here to allow the desktop to be powered off and work resumed
in a cloud environment. No consumer changes have been made.

## Preserved history

This branch extends the previously published migration checkpoint
`319e6a4b564cbd339e21f8012c9a6050356714d2`. It restores the modern Rust/TypeScript
Phon candidate from `54599c25411952ff9c361083916280599d580cf0` and implements the
corresponding Java changes on top. Earlier migration branches, original refs,
and extraction maps remain intact. The separate older late-credit branch
`82dc657bfc28321ba5f2336252912985a4ccf834` remains available; its different behavior
has not been silently substituted for the e76bef6 baseline.

Immediately before handoff, the original repository audit verified 3,033 refs
and all 122 previously dirty files unchanged, across Facet, Vox, terminal, mover,
and SFM. The private audit inventory is not required to continue this branch.

## Current implementation

- Root and nested Cargo.lock files were resolved independently. Both graphs use
  registry `taxon = 0.2.0-rc.5`; Phon's dependency is exact-pinned. The legacy
  `teamy-taxon` crate is removed from the current tree but retained in history.
  This joins Phon's schema types to the Taxon identity already used by registry
  `teamy-facet-core`, rather than keeping two aliased incompatible types.
- Java SchemaIdentity now follows the newer Taxon type-parameter prefix and
  Semantic/Channel/External hashing. It includes iterative SCC discovery and
  explicit work/byte/depth limits; these new limits still need adversarial tests.
- Java models and schema wire parsing cover Semantic, Channel and External;
  qualified-name validation and canonical PHONSCM1 bundles are implemented.
  Bundle admission checks references, arity, identifiers, ordering, interning,
  UTF-8 and limits. Review and cross-language conformance are still required.
- Schema code generation now emits PHONSCM1 bundles, and generated Java sources
  were refreshed. `SchemaClosure.fromSchemaBundleBytes` loads these bundles.
  The existing Vox root/auxiliary closure envelope (`fromBundleBytes`) remains a
  separate format and must not be replaced accidentally.
- Generic structural compact inspection follows Semantic's representation.
  Typed compatibility rejects Semantic without a registered handler. Per the
  upstream spec, matching representations alone do not establish semantic
  equivalence. This does not implement a Java durable-file/region executor.
- Existing Java runtime and late-credit behavior are preserved. WireCodec's
  generated-schema initialization changed; channel-credit code did not.

The relevant implementations are under `phon/java/runtime`,
`phon/rust/phon-codegen/src/targets/java`, and
`rust/vox-codegen/src/targets/java`. New Rust oracle fixtures and their generator
test are `phon/conformance/java-cutover` and
`phon/rust/phon-conformance/tests/java_cutover.rs`.

## Actual validation at this checkpoint

Passed:

- Offline root and nested Cargo metadata/lock resolution and duplicate-tree
  inspection. Each graph contains one registry Taxon and no teamy-taxon.
- `cargo run --offline --locked -p teamy-vox-xtask -- codegen --java`.
- The Java gate's deterministic generated-source check: sources are up to date.
- Rust oracle fixture generation and Rust bundle decode round trips for seven
  cases: region-ref Semantic, concrete Semantic u32, generic tuple, Tx, Rx,
  External without metadata and External with metadata. Initial fixture creation
  used `PHON_UPDATE_JAVA_CUTOVER_GOLDENS=1`; rerun without that variable to verify
  the checked-in fixture bytes before accepting the cutover.

Failed:

`cargo run --offline --locked -p teamy-vox-xtask -- test-java` exits 1 at javac:

1. `TerminalPresentationCapabilitiesResult.java:46`: `code too large` for
   `schemaClosureBundle()`. A complete bundle emitted as a single Java byte-array
   initializer exceeds the JVM method limit. Make bundle emission use bounded
   chunk helpers or another deterministic encoding that respects method and
   constant-pool limits; test a large fixture. Fix the generator and regenerate,
   not just this generated class.
2. `VoxJavaSubject.java:278,281,318,321` still references `CANONICAL_SCHEMAS`.
   Update its `schema` helper near line 351 from `byte[][]` / `fromCanonicalBytes`
   to the new bundle API and update those four references to `SCHEMA_BUNDLE`.

Java runtime/conformance did not execute because compilation stopped first.
No current-protocol Rust/Java wire, codegen unit, runtime credit, malformed-input,
or reproducible-JAR success is claimed. Prior `validation.json` results apply to
the preserved legacy checkpoint only. Tracey skill/MCP was unavailable locally;
its requested spec synchronization check remains unrun. TypeScript checks for
the restored newer source also remain unrun; use `pnpm check`, never `npx tsc`.

## Toolchain and safe reproduction

The checks used Linux x86_64, Rust 1.96.0, and Eclipse Adoptium Temurin
21.0.8+9-LTS with `javac --release 17`. A reconstructible pinned toolchain recipe
is [Containerfile.validation](../../tools/migration/Containerfile.validation).
No desktop image or cache is required for correctness. Cloud workers must obtain
the pinned images/dependencies through their authorized environment setup before
using `--offline`; the desktop's named caches were not published.

Run builds with no network after acquisition, source mounted read-only and copied
to scratch, no home/credentials/sockets, read-only container root, dropped
capabilities, no-new-privileges, and finite CPU/memory/PID limits. Desktop checks
used 2 CPUs, 1.5 GiB memory, 256 PIDs, 768 MiB work scratch and 256 MiB tmp.
Use dedicated writable cargo/target caches segregated by trust, Rust toolchain
and target; a writable build cache is a trust boundary.

After fixing Java generation, run from the repository root:

```sh
cargo run --locked -p teamy-vox-xtask -- codegen --java
cargo run --locked -p teamy-vox-xtask -- test-java
cargo test --locked --manifest-path phon/Cargo.toml -p teamy-phon-conformance --test java_cutover
cargo test --locked --manifest-path phon/Cargo.toml -p teamy-phon-conformance -p teamy-phon-schema
cargo test --locked -p teamy-vox-codegen --lib
chmod +x java/subject/subject-java.sh
cargo test --locked -p spec-tests --test java_wire -- --test-threads=2
cargo test --locked -p teamy-vox-core -p teamy-vox-types --lib
VOX_JAVA_BUILD_JDK='Eclipse Adoptium|21.0.8+9-LTS' cargo run --locked -p teamy-vox-xtask -- package-java
```

Also retain/retest the existing channel regressions and the recorded late-credit
comparison in `credit-behavior.md`. Do not turn the older branch's differing
expectations into new passing fixtures without reviewing behavior.

## Remaining work before any consumer cutover

1. Resolve the two concrete Java compilation failures above.
2. Add Java checks against the seven independent Rust fixtures and existing
   upstream point-bundle / Semantic bare-schema goldens. Test canonical bytes,
   identities, generic substitution, recursive schemas and semantic distinctions.
3. Add positive and malformed-input tests: all truncations, magic/version/tags,
   field names, trailing data, string-table order/duplicates/unused/out-of-range,
   schema order/duplicates/missing refs/arity/undeclared vars/hash mismatch,
   invalid qualified names, and byte/count/work/depth limits. Review encoder
   recursion and schema-closure validation as well as the decoder. Do not weaken
   existing tests to accept changed fixtures.
4. Run deterministic codegen, Java conformance/runtime, actual Rust-to-Java and
   Java-to-Rust RPC, malformed-credit regression and reproducible artifact checks.
   Package success is required before selecting a consumer pin.
5. Update affected consumers in isolated checkouts to the same validated Vox SHA
   and Java artifact provenance/hash. Do not add Vox to mover in this phase.

No consumer files, commits, pins or artifacts were created or published in this
cutover phase. `teamy-terminal` currently has separate control/streaming Vox
sources; both must be reconciled with its Facet graph. SFM must start from its
latest `capture/sfm435-issue610-cloud` lineage (last known
`8e349578d18511d8d5993b66a3ad3c7c957e5db4`), verify fresh remote state, and use its
main-source-projection workflow and current checkout instructions. Preserve the
Java e76bef6 late-credit baseline; an old desktop 1.19.2 checkout is not the latest
cloud source. Consult teamy-docs and SFM readiness instructions before editing.

Vox migration branch publication is authorized. Consumer changes and local
validation/commits are authorized, but consumer branch publication was not
explicitly granted in this delegation: report the exact proposed repository and
branch before pushing. No default-branch merges, force-pushes, package releases,
security/network changes, application restarts, or mover implementation are part
of this handoff.
