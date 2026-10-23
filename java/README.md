# Vox Java runtime vertical slice

This directory contains the dependency-free Java 17 Vox runtime experiment.
It is intentionally split into `runtime` and `subject` source trees, while the
accepted release artifact will combine Vox and Phon classes into one JAR.

The current runtime implements:

- bounded TCP stream framing with the normative `VOXL` version 1 link prologue;
- the normative version 9 `TransportHello`, `TransportAccept`, and
  `TransportReject` exchange;
- explicit single-owner connection driving;
- finite outbound, lane, pending-request, frame, schema, and timeout bounds;
- lane-owned request identifiers, correlation, cancellation, idle timeout,
  late-response discard, and terminal shutdown propagation;
- request-scoped typed `Tx`/`Rx` channels with negotiated credit, bounded
  queues, role-keyed auxiliary schemas, ordered close/reset propagation, and
  cancellation that reaches both the request and its channels;
- service descriptors, registries, dispatch interfaces, and exactly-once
  inbound response ownership; and
- the hosted-subject process and socket lifecycle for the existing
  `SUBJECT_MODE`, `PEER_ADDR`, `LISTEN_PORT`, and inactivity settings.

## Integration seam

`VoxConnection.establishSelfDescribingHandshake` exchanges the generated Phon
handshake schema, validates peer connection settings, and binds the peer's
message schema before driving application calls. The checked-in generated
handshake/message schemas and terminal bindings are verified by `test-java`.
The standalone repository includes the owned Phon Java runtime under `phon/java`.

`WireConstants` is the one temporary hand-written projection of Rust protocol
constants. The generator integration replaces it with a generated source file;
runtime and tests must not acquire additional copies.

## Focused Java 17 gates

The runtime now compiles directly against the dependency-free Phon Java
runtime; the temporary test-only `PhonAdapter` fixture has been removed. The
repository tasks discover JDK tools through `JAVA_HOME` and then `PATH`:

```console
cargo run --locked -p teamy-vox-xtask -- test-java
cargo run --locked -p teamy-vox-xtask -- package-java
```

`test-java` checks generated-source drift, compiles main, generated, test, and
subject sources with `javac --release 17 -Xlint:all -Werror`, and runs the Phon
conformance and focused Vox tests. `package-java` repeats that gate, builds only
the Phon and Vox runtime classes plus the generated `Terminal*` service
bindings, and writes
`java/target/teamy-vox-java-<version>.jar`. It assembles the JAR twice to verify
byte-for-byte reproducibility, compiles and runs a fresh Java consumer against
the artifact (including a generated terminal request), and rejects unresolved
non-JDK dependencies with `jdeps`. Generated testbed/application bindings are
intentionally excluded from the runtime artifact.

For reproducible SDK selection, set `VOX_JAVA_BUILD_JDK` to
`Eclipse Adoptium|21.0.8+9-LTS` and set `JAVA_HOME` to that JDK. Java package and
module names remain unchanged. This migration does not publish the artifact or
replace SFM's existing source-build lock; see the
[migration evidence](../docs/teamy-migration/README.md).

`GeneratedResponseIntegrationTest` additionally proves that complete
`Result<T, VoxError<E>>` response bytes round-trip without a private outer tag.
