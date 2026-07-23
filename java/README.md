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
- service descriptors, registries, dispatch interfaces, and exactly-once
  inbound response ownership; and
- the hosted-subject process and socket lifecycle for the existing
  `SUBJECT_MODE`, `PEER_ADDR`, `LISTEN_PORT`, and inactivity settings.

## Integration seam

`VoxConnection.establishSelfDescribingHandshake` is deliberately fail-closed.
It must be replaced during integration by the normative self-describing Phon
connection handshake using the Java schemas/adapters from the Phon track.
After that, the same driver must encode/decode the generated canonical
`ConnectionMessage` schema and turn its queued `CallCommand`, `CancelCommand`,
and `CloseLaneCommand` values into the corresponding protocol messages.

Until those generated schemas exist, the hosted subject announces a listening
address when required by the harness but never claims that a Vox connection is
usable. It exits on the explicit handshake failure and cannot strand a
non-daemon executor.

`WireConstants` is the one temporary hand-written projection of Rust protocol
constants. The generator integration replaces it with a generated source file;
runtime and tests must not acquire additional copies.

## Focused Java 17 gate

The runtime now compiles directly against the dependency-free Phon Java
runtime; the temporary test-only `PhonAdapter` fixture has been removed. The
repository task compiles main, generated, test, and subject sources together
and runs:

```text
javac --release 17 -Xlint:all -Werror ...
java -ea org.facet.vox.tcp.StreamFramingTest
java -ea org.facet.vox.VoxRuntimeTest
```

`GeneratedResponseIntegrationTest` additionally proves that complete
`Result<T, VoxError<E>>` response bytes round-trip without a private outer tag.
