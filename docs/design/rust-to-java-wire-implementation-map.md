# Rust-to-Java Vox wire implementation map

Status: implementation reference for the first Java TCP interop slice.

This document traces the current Rust implementation from the TCP byte stream
through a unary RPC. It records the source symbols that define the wire
contract so Java work does not derive a second protocol from partial examples.

## Complete connection sequence

For a TCP initiator, the byte/message sequence is:

1. Both peers write the raw six-byte stream-link prologue.
2. The initiator sends a framed fixed transport hello.
3. The acceptor sends a framed fixed transport accept or reject.
4. The initiator sends a framed self-describing Phon `Hello`.
5. The acceptor sends a framed self-describing Phon `HelloYourself`.
6. The initiator sends a framed self-describing Phon `LetsGo`.
7. The service-lane opener sends a compact Phon `LaneOpen`.
8. The peer sends a compact Phon `LaneAccept` or `LaneReject`.
9. Before the first method call, the caller sends an argument
   `SchemaMessage`, followed by the `RequestCall`.
10. Before the first response, the callee sends a response `SchemaMessage`,
    followed by the `RequestResponse`.
11. Later calls for the same method and direction reuse the connection-scoped
    bindings and omit those `SchemaMessage` values.

Every payload after the raw link prologue is wrapped in a four-byte
little-endian length frame by the stream link.

## Stream link and framing

The source of truth is `rust/vox-stream/src/lib.rs`:

- `LINK_MAGIC`;
- `LINK_VERSION`;
- `LINK_FLAG_FD_CAPABLE`;
- `link_prologue`;
- `validate_link_prologue`;
- `StreamLink::split`;
- `frame_len_prefix`; and
- `read_frame_exact`.

A normal TCP peer writes these bytes before any length-prefixed frame:

```text
56 4f 58 4c 01 00
V  O  X  L  v1 flags=0
```

Both directions send this prologue immediately. The FD-capable Unix variant
sets flag bit `0x01` and uses different frame machinery; it is outside the first
Java slice.

Every later stream payload is:

```text
[length: u32 little-endian][payload: length bytes]
```

Java must check the length against its configured bound before allocation. It
must distinguish:

- clean EOF before reading a new frame header;
- EOF partway through the link prologue;
- EOF partway through the four-byte frame header; and
- EOF partway through the frame body.

One writer owner must serialize complete frames. Concurrent callers cannot
interleave bytes.

## Fixed transport prologue

The source of truth is
`rust/vox-core/src/transport_prologue.rs`:

- `TransportHello`;
- `TransportAccept`;
- `TransportReject`;
- `initiate_transport`;
- `accept_transport`; and
- `reject_transport`.

The current version is 9. The fixed eight-byte payloads are:

```text
initiator -> acceptor: VOTH 09 00 00 00
acceptor  -> initiator: VOTA 09 00 00 00
reject:                 VOTR 09 01 00 00
```

The reject reason `1` is `UnsupportedPrologue`. Reserved bytes must be zero.
These eight bytes are each carried inside the stream's outer length frame.
They are not Phon.

Connection builders enforce that this exchange precedes the application
handshake. See these symbols in
`rust/vox-core/src/connection/builders.rs`:

- `ConnectionSourceInitiatorBuilder::establish_connection_inner`;
- `ConnectionTransportInitiatorBuilder::establish_connection_inner`;
- `ConnectionTransportInitiatorBuilder::finish_with_bare_parts`;
- the corresponding acceptor builder paths; and
- `initiate_transport_observed` / `accept_transport_observed`.

## Self-describing Phon connection handshake

The behavior is in `rust/vox-core/src/handshake.rs`:

- `message_schema`;
- `send_handshake`;
- `recv_handshake`;
- `handshake_as_initiator_with_policy`; and
- `handshake_as_acceptor_with_policy`.

The shapes are in `rust/vox-types/src/handshake.rs`:

- `HandshakeMessage`;
- `Hello`;
- `HelloYourself`;
- `LetsGo`;
- `Decline`;
- `Sorry`; and
- `HandshakeResult`.

`vox_phon::to_self_describing` and `from_self_describing`, implemented in
`rust/vox-phon/src/schema.rs`, define each handshake frame's inner payload:

```text
[schema closure length: u32 LE]
[canonical HandshakeMessage schema closure]
[compact HandshakeMessage value]
```

The initiator sends `Hello`, receives `HelloYourself`, then sends `LetsGo`.
Either peer can send `Decline` for policy rejection or `Sorry` for schema or
protocol incompatibility.

`Hello` carries:

- the initiator's desired identifier parity;
- `ConnectionSettings { parity, max_concurrent_requests,
  initial_channel_credit }`;
- the initiator's canonical schema closure for `Message<'static>`; and
- metadata.

`HelloYourself` carries the acceptor's settings, `Message` schema closure, and
metadata. The acceptor uses the opposite parity. Both sides reject
`initial_channel_credit == 0`.

Some comments still call the exchanged schema a `MessagePayload` schema.
`message_schema()` actually calls
`vox_phon::schema_bytes::<vox_types::Message<'static>>()`; Java must follow the
code.

After the handshake, `message_plan_from_handshake_observed` and
`BareConduit::with_message_plan` use the peer's writer schema to build one
compatibility decode program for the local `Message` shape.

## Compact connection envelope

The canonical wire types are in `rust/vox-types/src/message.rs`.

`Message` has:

```text
lane_id: LaneId (u64)
payload: MessagePayload
```

`MessagePayload`, in declared discriminant order, contains:

1. `ProtocolError`
2. `LaneOpen`
3. `LaneAccept`
4. `LaneReject`
5. `LaneClose`
6. `RequestMessage`
7. `SchemaMessage`
8. `ChannelMessage`
9. `Ping`
10. `Pong`

`rust/vox-core/src/bare_conduit/mod.rs` owns the compact envelope:

- `BareConduitTx::prepare_send` calls `vox_phon::to_vec`;
- `BareConduitRx::ensure_program` parses the peer schema and builds the
  writer-to-reader program; and
- `BareConduitRx::recv` decodes with that cached program.

These post-handshake values do not carry self-describing schema prefixes. The
peer's envelope schema was exchanged once in the handshake.

### Opaque `Payload`

`Payload` and `PayloadAdapter` are defined in
`rust/vox-types/src/message.rs`. During envelope decoding, a request argument,
response value, or channel item is not immediately typed. The opaque adapter
captures the exact borrowed raw byte span as `Payload::Encoded`. Typed decoding
happens later, after the relevant method/direction schema binding is known.

Java needs the same two-phase behavior. Decoding a `Message` must not decode an
opaque payload as a generic tree or copy/reinterpret it under the envelope
schema.

## Dynamic metadata is mandatory protocol machinery

`Metadata` is `facet_value::Value`, defined in
`rust/vox-types/src/metadata.rs`. It is null when empty, or a dynamic object
whose values are strings, byte runs, or unsigned 64-bit integers.

Although dynamic Facet application DTOs are rejected by the first Java
generator slice, the Java wire runtime still needs this specific dynamic value
codec. It is used by:

- `Hello` and `HelloYourself`;
- lane open, accept, reject, and close;
- calls, responses, and cancellation; and
- policy and grant information.

This is not optional for interop. In particular, opening a service lane
requires the string metadata key `vox-service`.

## Service-lane open

The principal implementation points in
`rust/vox-core/src/connection/mod.rs` are:

- `ConnectionHandle::open_lane_with_settings`;
- `handle_open_request`;
- `handle_inbound_open`;
- `handle_inbound_accept`;
- `handle_inbound_reject`; and
- `LaneRequest::new`.

The opener allocates a nonzero lane ID from its parity and sends, on that lane:

```text
LaneOpen {
  connection_settings,
  metadata: { "vox-service": "<service name>" }
}
```

The receiver verifies parity, uniqueness, nonzero initial credit, and the
required service metadata. It answers on the same lane with `LaneAccept` and
its settings, or `LaneReject`.

`LaneClose` terminates a non-control lane. A transport EOF or connection-level
protocol error terminates every active lane and pending request.

## Method identity and an echo call

`rust/vox-types/src/method_identity.rs::method_id_name_only` is normative.
It:

1. converts the service name to kebab case;
2. appends `.`;
3. converts the method name to kebab case;
4. computes BLAKE3; and
5. interprets the first eight digest bytes as a little-endian `u64`.

Types are deliberately absent from the method identity so compatible schema
evolution does not change routing.

The existing generated TypeScript descriptor at
`typescript/generated/testbed.generated.ts` proves:

```text
Testbed.echo = 0x880bc4eee23574be
```

Java should consume the Rust-emitted bit pattern as a Java `long`; it must not
rehash the method name.

### Schema-before-payload ordering

Schema binding behavior is defined by:

- `rust/vox-types/src/schema.rs::SchemaSendTracker`;
- `SchemaRecvTracker`;
- `plan_for_method_args`;
- `rust/vox-core/src/connection/mod.rs::prepare_outbound_batch`;
- `plan_call_schema_send`;
- `plan_response_schema_send`; and
- `run_outbound_worker`.

The first echo call on a connection sends, on the service lane:

```text
SchemaMessage {
  method_id: 0x880bc4eee23574be,
  direction: Args,
  schemas: canonical closure for (String,)
}

RequestMessage {
  id: caller-allocated request id,
  body: Call {
    method_id: 0x880bc4eee23574be,
    channels: [],
    metadata: empty/null,
    args: compact bytes for the one-element String tuple,
    schemas: []
  }
}
```

The outbound worker sends the schema frame successfully before it sends the
payload frame. Although `RequestCall` and `RequestResponse` retain `schemas`
fields for compatibility and forwarding paths, the normal current send path
moves new bindings to standalone `SchemaMessage` values and clears the inline
field.

The receiver records `(method_id, Args) -> schema bytes`, then dispatches the
call. Typed argument decoding is in
`rust/vox/src/schema_deser.rs::schema_deserialize_args_*`.

The first response similarly sends:

```text
SchemaMessage {
  method_id: 0x880bc4eee23574be,
  direction: Response,
  schemas: canonical closure for Result<String, VoxError<Infallible>>
}

RequestMessage {
  id: the original request id,
  body: Response {
    metadata: empty/null,
    ret: compact bytes for the full wire Result,
    schemas: []
  }
}
```

Response decoding uses
`schema_deserialize_response` or
`schema_deserialize_response_borrowed`. Later calls reuse both bindings.

## Responses and application errors

The response payload is not a bare success DTO, and there is no independent
wire-level success/error tag outside Phon.

`rust/vox-types/src/calls.rs` is normative:

- `SinkCall::reply`;
- `ReplySink::send_error`;
- `ReplySink::send_typed_error`; and
- `CallResult`.

Every response payload has the Phon shape:

```text
Result<T, VoxError<E>>
```

For an application error, `SinkCall::reply` maps:

```text
Err(E) -> Err(VoxError::User(Box<E>))
```

The other variants of `VoxError`, declared in
`rust/vox-types/src/vox_error.rs`, are:

1. `User`
2. `UnknownMethod`
3. `InvalidPayload`
4. `Cancelled`
5. `ConnectionClosed`
6. `ConnectionShutdown`
7. `SendFailed`
8. `TimedOut`
9. `Indeterminate`

Their zero-based enum discriminants follow that declaration order.

The Java runtime can internally expose a decoded result object, but it must not
invent an extra transport tag. If `ServiceLane.call` returns raw bytes, those
bytes must be understood as the full response-wire `Result`. Generated clients
need an adapter for `Result<T, VoxError<E>>`, synthesized from the method's
success and application-error adapters. Generated dispatchers must likewise
encode the full result rather than pass a bare success or application DTO to
`InboundCall.respond`.

Transport failures can still complete Java futures exceptionally. A peer-sent
`VoxError` is protocol data in the response payload.

## Cancellation and close

Cancellation is a `RequestMessage` on the same lane with the same request ID:

```text
RequestMessage {
  id: request being cancelled,
  body: Cancel {
    metadata
  }
}
```

`rust/vox-core/src/driver.rs`, around the `RequestBody::Cancel` handling in the
driver loop, aborts the matching in-flight handler and terminalizes associated
request channels. A response that arrives after local cancellation must not be
correlated to a later request.

Close behavior:

- `LaneClose` closes one non-control service lane;
- `ProtocolError` describes a connection-level violation and is followed by
  transport close;
- clean or failed transport close terminates all lanes and pending calls; and
- local shutdown must also make every pending call terminal.

## Java wire code generation required

The Java generator must gain a wire target analogous to
`vox/xtask/src/main.rs::codegen_swift_wire`.

Generate types, schemas, and adapters for:

- `Parity`;
- `BindingDirection`;
- `ConnectionSettings`;
- `ProtocolError`;
- `Ping` / `Pong`;
- `LaneOpen` / `LaneAccept` / `LaneReject` / `LaneClose`;
- `RequestCall` / `RequestResponse` / `RequestCancel`;
- `RequestBody` / `RequestMessage`;
- `SchemaMessage`;
- channel message types, even if channel operation remains rejected at the
  generated service surface;
- `MessagePayload`; and
- `Message`.

Generate the handshake graph separately:

- `Hello`;
- `HelloYourself`;
- `LetsGo`;
- `EstablishmentRejectReason`;
- `Decline`;
- `Sorry`; and
- `HandshakeMessage`.

All variant orders, constants, schema IDs, and schema closures must be emitted
from the Rust source of truth. Java templates must not duplicate them by hand.

The runtime also needs:

- self-describing handshake encode/decode;
- compact envelope encode/decode against the negotiated writer schema;
- opaque payload-span capture;
- the constrained dynamic metadata value codec;
- connection-scoped schema binding storage;
- full response-wire `Result<T, VoxError<E>>` adapters; and
- bounded frame/schema/value decoding.

## Existing evidence and harness entry points

Golden vectors live under:

- `test-fixtures/golden-vectors/wire`;
- `test-fixtures/golden-vectors/result`; and
- the other primitive/composite vector directories.

They are generated by
`rust/vox-core/src/bin/generate_golden_vectors.rs`. The wire directory README
still says “postcard”; the current generator's `encode_message` path must be
checked before treating that stale label as authoritative.

Focused Rust tests and entry points:

- stream framing/prologue and truncation tests in
  `rust/vox-stream/src/lib.rs`;
- transport ordering in `rust/vox/tests/driver_transport_tests.rs`;
- fixed transport-prologue tests in `rust/vox-core/src/tests/mod.rs`;
- connection, schema-ordering, call, cancellation, and close behavior in
  `rust/vox-core/src/tests/driver_tests.rs`; and
- schema compatibility decoding in `rust/vox/src/schema_deser.rs`.

Cross-language harness integration:

- add `Java` to
  `spec/spec-tests/src/harness.rs::SubjectLanguage`;
- add the Java launch command to `subject_cmd_for_language`;
- use `accept_subject_spec` for Rust harness -> Java subject;
- use `run_subject_client_scenario` for Java client -> Rust harness;
- add Java combinations to the matrix generator in `xtask/src/main.rs`;
- first server-direction proof:
  `spec/spec-tests/tests/cases/testbed.rs::run_rpc_echo_roundtrip`;
- first client-direction proof:
  `run_subject_calls_echo`; and
- later use `run_rpc_user_error_roundtrip` for the application-error arm.

The hosted subject contract uses `PEER_ADDR`, `SUBJECT_MODE`, and
`CLIENT_SCENARIO`; server-listen mode reports `LISTEN_ADDR=...`.

## Smallest actual Rust-to-Java echo slice

The shortest vertical path with real wire proof is:

1. Generate Java handshake and `Message` wire shapes, adapters, canonical
   schema closures, and method constants.
2. Implement the raw `VOXL` stream prologue, bounded four-byte LE framing, and
   fixed `VOTH`/`VOTA` transport exchange.
3. Implement the self-describing `Hello`/`HelloYourself`/`LetsGo` handshake,
   including constrained dynamic metadata and negotiated `Message` decode plan.
4. Start with Java as a subject client connecting to the Rust harness.
5. Open a `Testbed` service lane using `vox-service=Testbed`.
6. Implement only `SchemaMessage`, `RequestCall`, `RequestResponse`, and the
   codecs needed for:
   - `(String,)`;
   - `Result<String, VoxError<Infallible>>`; and
   - the envelope/metadata types surrounding them.
7. Register the Java subject and pass `run_subject_calls_echo`.
8. Add the Java acceptor/server role and pass `run_rpc_echo_roundtrip`.
9. Only then expand to user errors, cancellation, timeout, nested DTOs, and
   bidirectional calls on one connection.

Starting with the Java client avoids implementing inbound lane authorization
and dispatch at the same time as the bootstrap wire. It still proves every
lower layer: stream framing, both prologues, the Phon handshake, the negotiated
envelope, lane open, schema binding, request correlation, and a typed response.

## Integration risks

- Dynamic metadata is required even though dynamic application DTO generation
  is rejected.
- `Payload` must remain an opaque raw span until method schema selection.
- `Message` compatibility planning cannot be replaced with a same-version
  shortcut.
- Schema frames must be committed before their payload frame.
- A response is the full `Result<T, VoxError<E>>`, not a bare DTO and not a new
  out-of-band tagged result.
- The first Java generator checkpoint currently emits bare return adapters in
  places where integration needs a synthesized response-wire adapter.
- Java signed `long` values must preserve unsigned protocol bit patterns.
- Lane and request allocators must honor negotiated parity.
- The wire golden-vector README contains stale “postcard” terminology.
- Hand-coded magic values or variant orders will drift; generate them from
  Rust.
- Bounds must apply before allocation to stream frames, handshake schema
  closures, metadata, schema tables, collections, and payloads.
