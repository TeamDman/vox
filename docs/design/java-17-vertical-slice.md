# Java 17 Phon/Vox vertical-slice contract

Status: frozen delegation contract for the first Java implementation.

This document fixes the ownership boundaries and public interfaces for the
first Java Phon/Vox implementation. The normative wire behavior remains the
existing Phon and Vox specifications, Rust schema definitions, and checked-in
golden vectors. This document does not create a second protocol.

The first consumer is a Minecraft mod whose oldest supported runtime is Java
17. The implementation therefore cannot require records, sealed types, virtual
threads, or library APIs introduced after Java 17.

## Supported slice

The first accepted slice is:

- Java 17 source and bytecode;
- TCP, including Vox stream framing and both transport prologues;
- one explicitly driven, bidirectional Vox connection;
- multiple service lanes over that connection;
- unary request/response calls;
- generated DTOs, callers, handlers, dispatchers, and descriptors;
- `CompletableFuture` for asynchronous calls;
- Phon canonical schemas, schema closures, schema ids, compatibility plans,
  canonical value encoding, and typed adapters;
- request cancellation, idle timeout, connection shutdown, and bounded decode;
- Rust server to Java client, Java server to Rust client, and calls in both
  directions on one connection; and
- one dependency-free combined runtime JAR.

The following are rejected by Java generation with a diagnostic that identifies
the service, method, and unsupported shape:

- channels;
- file descriptors and other external capabilities;
- dynamic Facet values;
- WebSocket, Unix-domain, named-pipe, shared-memory, Iroh, and in-process
  transports;
- automatic reconnect, retry, replay, or durable requests;
- JIT/optimized Phon execution; and
- borrowing or zero-copy APIs whose lifetime cannot be represented safely in
  Java.

Unsupported wire messages received from a peer close the affected lane or
connection according to the normative specification. They are never ignored or
reinterpreted as an extension point.

## Repository and package layout

Ownership is fixed as follows:

| Path | Java package | Owner |
| --- | --- | --- |
| `phon/java/runtime/src/main/java` | `org.facet.phon` | Phon Java track |
| `phon/java/runtime/src/test/java` | `org.facet.phon` | Phon Java track |
| `vox/java/runtime/src/main/java` | `org.facet.vox` | Vox Java runtime track |
| `vox/java/runtime/src/test/java` | `org.facet.vox` | Vox Java runtime track |
| `vox/java/generated/src/main/java` | `org.facet.vox.generated` | Java generator output |
| `vox/java/subject/src/main/java` | `org.facet.vox.subject` | Vox Java runtime track |
| `phon/rust/phon-codegen/src/targets/java` | Rust | Java generator track |
| `vox/rust/vox-codegen/src/targets/java` | Rust | Java generator track |

Generated files contain a generated-file header and are never hand edited.
Runtime code does not depend on generated test services.

The release assembly combines the Phon and Vox runtime class trees into one JAR
named `vox-java-<version>.jar`. It has:

- no third-party runtime dependency;
- no JNI;
- no `Automatic-Module-Name` split across multiple JARs;
- `Automatic-Module-Name: org.facet.vox`;
- no bundled generated application services; and
- reproducible entry ordering and timestamps when assembled by the repository
  task.

The runtime may be compiled as separate Phon and Vox source sets during
development, but the accepted artifact is the combined JAR. Maven publication
coordinates are deliberately not frozen by this experiment.

## Build contract

The repository task is the owner of Java compilation and packaging. It invokes
the JDK tools directly:

```text
javac --release 17
jar --create
```

The accepted task names are:

```text
cargo xtask codegen --java
cargo xtask check-codegen --java
cargo xtask test-java
cargo xtask package-java
```

`codegen --java` regenerates both Phon DTO/schema sources and Vox service
sources. `check-codegen --java` generates into a temporary directory and fails
on drift. `test-java` compiles runtime, generated fixtures, and the hosted
subject, then runs Java unit/conformance entry points. `package-java` emits the
combined dependency-free JAR only after the Java tests pass.

JDK discovery uses `JAVA_HOME` first and then `PATH`. A missing JDK, a JDK older
than 17, or absence of `javac`/`jar` is a clear task error. Gradle and Maven are
not prerequisites for developing or testing this runtime.

## Phon Java public interface

The Phon track owns the implementations behind these public shapes. Minor
constructor visibility and helper methods may be added, but names and semantic
roles below are frozen for the delegated tracks.

### Schema model

`org.facet.phon.Schema` is the immutable Java representation of a canonical
Phon schema node. It must represent every shape required by the accepted
fixtures: primitives, string, bytes, option, list, map, tuple, record, enum,
and references used for recursive closures.

`org.facet.phon.SchemaClosure` owns a root schema plus its canonical reachable
schema table. It exposes:

```java
Schema root();
SchemaId id();
byte[] canonicalBytes();
```

`org.facet.phon.SchemaId` is an immutable value object over the exact canonical
schema-id bytes. It provides defensive-copy `bytes()`, lowercase hexadecimal
`toString()`, value equality, and value hashing. Its algorithm and byte width
come from the normative Phon schema-id implementation and vectors, not this
document.

### Value model and typed adapters

`org.facet.phon.Value` is the immutable language-neutral tree used by
conformance tests and diagnostics. It must distinguish signed/unsigned integer
domains where the Phon model distinguishes them and must preserve arbitrary
byte arrays without converting them to text.

Application DTOs use typed adapters rather than reflection:

```java
public interface PhonAdapter<T> {
    SchemaClosure schema();
    void encode(PhonEncoder encoder, T value) throws PhonException;
    T decode(PhonDecoder decoder) throws PhonException;
}
```

Generated adapters are stateless singletons exposed as `public static final
PhonAdapter<T> ADAPTER` on each generated DTO. Runtime DTO discovery never uses
classpath scanning or Java reflection.

The codec surface is:

```java
public final class PhonCodec {
    public static <T> byte[] encode(PhonAdapter<T> adapter, T value,
                                    PhonLimits limits);
    public static <T> T decode(PhonAdapter<T> adapter, byte[] bytes,
                               PhonLimits limits);
    public static Value decodeValue(SchemaClosure writer, byte[] bytes,
                                    PhonLimits limits);
}
```

`PhonEncoder` and `PhonDecoder` are public for generated adapters but their
constructors are runtime-owned. They expose typed primitive/container
operations, not raw internal cursors.

### Compatibility

`org.facet.phon.CompatibilityPlan` is built once from writer and reader schema
closures and reused:

```java
public static CompatibilityPlan plan(
        SchemaClosure writer, SchemaClosure reader, PhonLimits limits);

public <T> T decode(byte[] bytes, PhonAdapter<T> reader, PhonLimits limits);
```

Planning must reproduce the checked-in compatibility vectors, including field
matching, optional/defaulted additions, removed writer fields, enum mapping,
recursive references, and explicit incompatible outcomes. Planning failure is
not deferred until an arbitrary later field read.

### Bounds and failures

`org.facet.phon.PhonLimits` is an immutable configuration with finite positive
limits for:

- input bytes;
- schema bytes;
- nesting depth;
- collection entries;
- string/byte-run length;
- referenced schema count; and
- planning work.

Defaults are conservative and tests exercise each bound. No decode path
allocates from an untrusted length before checking the corresponding bound.

`PhonException` is the checked root for schema, planning, encode, decode,
truncation, malformed input, incompatibility, and limit failures. Subclasses or
an error-kind enum may refine it. Errors include a byte offset or schema path
when available and never expose partially initialized DTOs.

## Generated Java contract

The generator consumes the same Rust service descriptors and Phon schemas as
the TypeScript and Swift targets. It does not parse Rust source text.

### DTOs

Generated records and enum payloads are final Java 17 classes with:

- private final fields;
- a validating constructor;
- field accessors named after the schema field;
- value `equals`, `hashCode`, and `toString`;
- a static canonical `SCHEMA`;
- a static typed `ADAPTER`; and
- stable unsigned handling that rejects out-of-range Java values.

The generator may use Java records only after the minimum runtime moves beyond
Java 17 and the contract is deliberately revised; the first slice uses ordinary
classes to keep construction and validation explicit.

### Service descriptors

Each generated service exposes one immutable descriptor:

```java
public final class EchoServiceDescriptor {
    public static final ServiceDescriptor INSTANCE;
}
```

`ServiceDescriptor` contains the service name and ordered `MethodDescriptor`
values. A method descriptor contains the canonical 64-bit method id, method
name, argument adapter, return adapter, and declared application-error adapter
when present. Method ids are generated by the existing Rust algorithm and
emitted as Java `long` bit patterns; Java does not independently hash names.

### Handler and dispatcher

For service `Echo`, generation emits:

```java
public interface EchoHandler {
    CompletableFuture<String> echo(CallContext context, String value);
}

public final class EchoDispatcher implements ServiceDispatcher {
    public EchoDispatcher(EchoHandler handler);
}
```

Methods with a declared application error return
`CompletableFuture<VoxResult<OkType, ErrorType>>`. Transport/protocol failures
complete the future exceptionally and are not encoded as application errors.

The dispatcher switches on the generated method id, decodes arguments through
the generated adapter, invokes the handler once, and sends exactly one terminal
response unless cancellation or connection shutdown has already made the
request terminal. Unknown method ids produce the normative unknown-method
error.

### Caller

Generation emits:

```java
public final class EchoClient {
    public EchoClient(ServiceLane lane);
    public CompletableFuture<String> echo(String value);
    public CompletableFuture<String> echo(String value, CallOptions options);
}
```

The caller delegates request ownership and correlation to `ServiceLane`; it
does not implement a private transport loop or codec.

## Vox Java runtime interface

### Explicit driver ownership

The connection is not secretly driven by generated callers. The embedding
chooses either a blocking driver loop or an executor-owned task:

```java
public final class VoxConnection implements AutoCloseable {
    public static VoxConnection connect(
            InetSocketAddress address, ConnectionOptions options)
            throws IOException, VoxException;

    public static VoxConnection accept(
            Socket socket, ServiceRegistry services,
            ConnectionOptions options)
            throws IOException, VoxException;

    public void drive() throws IOException, VoxException;
    public CompletableFuture<Void> start(Executor executor);
    public ServiceLane openLane(ServiceDescriptor service,
                                LaneOptions options);
    public ConnectionState state();
    public CompletableFuture<Void> closed();
    public void close();
}
```

Exactly one caller may own `drive`; `start` reserves that ownership before
submitting work. Calls made before a driver starts may queue only within the
finite outbound bound. `close` is idempotent, makes pending calls terminal, and
causes `drive`/`closed` to finish.

### TCP and framing

`org.facet.vox.tcp` is not a separate artifact in the first slice. TCP support
is part of the combined runtime.

The implementation follows, in order:

1. Vox stream-link prologue and its version/flags;
2. 32-bit little-endian bounded message framing;
3. transport `TransportHello` / `TransportAccept` / `TransportReject`;
4. self-describing Phon connection handshake;
5. connection messages, lane messages, schema binding, and request traffic.

The Java implementation uses the constants and byte vectors generated from the
Rust source of truth. It does not duplicate magic/version values in generator
templates and runtime classes.

No frame buffer is allocated before the 32-bit length is checked against
`ConnectionOptions.maxFrameBytes()`. EOF mid-prefix and EOF mid-frame are
distinct malformed/truncated failures. One writer owner serializes complete
frames; concurrent callers cannot interleave bytes.

### Lanes, calls, and dispatch

The runtime-owned interfaces are:

```java
public interface ServiceDispatcher {
    ServiceDescriptor descriptor();
    CompletableFuture<Void> dispatch(InboundCall call);
}

public final class ServiceRegistry {
    public ServiceRegistry register(ServiceDispatcher dispatcher);
}

public final class ServiceLane implements AutoCloseable {
    public CompletableFuture<byte[]> call(
            MethodDescriptor method, byte[] encodedArguments,
            CallOptions options);
    public LaneState state();
    public void close();
}
```

`ServiceRegistry` rejects duplicate service identities. `ServiceLane` owns
request-id allocation, pending-call correlation, schema-binding reuse, timeout,
cancellation, and lane-close propagation.

Cancelling a returned `CompletableFuture` sends `RequestCancel` when the request
was committed. Cancellation before commitment removes the request without
publishing it. A late response after a terminal cancellation is observed and
discarded according to the protocol; it never completes a later call.

`CallOptions` contains a finite idle timeout and optional immutable metadata.
The first slice does not retry. `CallContext` exposes request id, peer/lane
metadata, cancellation state, and a cancellation future, but no transport
socket.

### Threading

Connection state is owned by the driver. Public methods communicate with it
through bounded queues and futures. Generated handler continuations may run on
an embedding-supplied handler executor, but all protocol state transitions are
serialized back through the driver.

The implementation must not:

- hold a runtime monitor while invoking user code;
- complete user futures while holding the connection state lock;
- block the driver waiting for a handler future;
- use the common fork-join pool implicitly; or
- require one operating-system thread per in-flight request.

Minecraft may supply executors that marshal completion to its client thread;
the Vox runtime itself has no Minecraft dependency.

### Runtime bounds and terminal behavior

`ConnectionOptions` has finite positive defaults for frame bytes, queued
outbound bytes/messages, pending requests, open lanes, schema bytes/count,
handshake timeout, idle timeout, and close timeout.

Protocol errors close the connection and complete all affected futures
exceptionally. Unknown service or method and incompatible method schemas are
request/lane failures where the normative protocol permits them. Disconnect
never leaves a pending call incomplete.

The hosted Java subject:

- reads its peer address and role from the same harness contract as existing
  subjects;
- supports both caller and handler directions;
- prints machine-readable readiness only after the connection is usable;
- exits promptly on peer disconnect or explicit shutdown;
- has a finite inactivity deadline; and
- cannot leave a non-daemon executor alive after terminal connection failure.

## Conformance and acceptance

The Java implementation is accepted only when all of these are automated:

### Phon

- every applicable value corpus file decodes and re-encodes byte-for-byte;
- Java schema ids match Rust for the accepted shape corpus;
- Java canonical schema bytes match Rust;
- compatibility vectors match Rust outcomes;
- nested DTO, recursive schema, Unicode, byte array, integer boundaries,
  truncation, invalid discriminant, duplicate key, excessive depth, and
  excessive length cases are covered; and
- each public decode/planning bound has a failing fixture.

### Generation

- two consecutive generations are byte-identical;
- checked-in generated fixtures have no drift;
- generated sources compile with `javac --release 17`;
- `echo(String)`, a nested DTO, and a fallible method compile against the frozen
  runtime interfaces; and
- every explicitly unsupported shape fails generation with a focused message.

### Vox

- Rust server / Java client;
- Java server / Rust client;
- Java and Rust each call the other over one established connection;
- compatible schema evolution succeeds;
- incompatible call schemas fail without dispatching the handler;
- repeated calls reuse a schema binding;
- unknown method, invalid payload, cancellation, idle timeout, and disconnect
  have terminal expected outcomes;
- transport and connection prologue vectors match;
- frame-length and queue bounds fail before unbounded allocation; and
- the Java subject exits after disconnect and inactivity.

The first three services are fixed:

```text
echo(String) -> String
inspect(NestedRequest) -> NestedResponse
divide(DivideRequest) -> Result<DivideResponse, DivideByZero>
```

The combined JAR is then compiled into a tiny Java-only smoke consumer and
inspected to prove it has no unresolved non-JDK classes. SFM packaging and
Minecraft launch tests consume that frozen JAR; they do not build Facet as part
of a mod build.

## Delegation and integration rule

The contract checkpoint containing this document is pushed before implementation
worktrees are created. Phon Java, Java generation, and Vox Java runtime agents
start from that exact SHA.

If an implementation discovers that a frozen public interface is impossible or
contradicts the normative protocol, the agent records the contradiction and
stops at the boundary. The coordinator changes this document in the integration
branch, gates and pushes a new checkpoint, and explicitly rebases the delegation
plan. Agents do not independently fork the public API.

Integration order is Phon, generation, then runtime. Temporary hand-written DTOs
and descriptors are allowed in the runtime worktree, but are replaced by the
generated fixtures during integration. Conformance is the arbiter when a
language implementation disagrees with intuition.
