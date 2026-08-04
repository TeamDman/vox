# Java 17 terminal contract

This experiment adds a small, generated `Terminal` service for the optional
Teamy Studio/Vox presentation lane. `vox/spec/spec-proto/src/terminal.rs` is
the only API source of truth; Java files under `vox/java/generated` are
generated with `cargo xtask codegen --java`.

## Ownership and fallback

The Java side owns the Minecraft panel, input focus, texture upload, and the
Java-local terminal fallback. Rust is an optional service client/server. A
missing or disconnected Vox peer is represented as a normal capability/error
state and must not prevent mounted editing or the in-game terminal from
working.

Rasterization ownership is explicit protocol data. Each advertised
`TerminalPresentationMode` carries the closed `TerminalRasterizationOwner`
value `Server` or `Client`; peers reject an unknown discriminant and never
infer ownership from a renderer id prefix. The owner is metadata for the
renderer, not a third user-selectable axis. Server-owned renderers rasterize
before Vox and therefore pair with pixel transports. A later semantic-cell
contract may advertise client-owned renderers that rasterize after Vox.

## Message shape

Control operations remain unary and intentionally avoid file descriptors,
dynamic values, and remote UI objects. `connect` negotiates capabilities and
creates a session; `capabilities` and `resize` report the current session;
`send_text`, `send_key`, and `send_mouse` carry input; `snapshot` returns the
latest recovery frame; `subscribe_frames` opens a typed, request-scoped
`Tx<TerminalFrameEvent>`; and `cancel`/`disconnect` close the corresponding
operation or session. Every request and response carries a session id and
monotonic client or server sequence. Transport cancellation and disconnect
remain Vox infrastructure outcomes; `TerminalError` is reserved for
structured application errors.

The live path is push-based. Rust publishes immediately after PTY state
mutation, bounded by Vox channel credit plus one newest pending full-state
frame. Superseded pending work is coalesced before rendering where possible;
no rendering or terminal lock is held while waiting for credit or transport.
The first event is a full resynchronization. Java validates connection/session
epochs and monotonic sequence numbers, keeps only the latest accepted frame,
and uses unary `snapshot` only for bootstrap or recovery—not steady polling.
Each event carries producer counters and mutation-to-send/credit-wait timing so
latency and boundedness can be verified without relying on perception alone.

`TerminalSnapshot.payload` is bounded bytes whose representation is selected
by `TerminalFrameEncoding`: structured cells, RGBA8, BGRA8, or PNG. The
snapshot also carries stride, full-frame versus dirty-tile metadata, cursor
state, a bounded linear selection range, and OSC 133 prompt/command ranges
with explicit presence flags. The prompt range is the shell-reported A-to-B
interval; the command range is the B/C-to-D interval and includes the reported
exit status when available. A decoder checks the negotiated
`max_frame_bytes` before allocating. The contract fixture uses a 512×256
logical maximum and a 16 MiB frame maximum as conservative defaults; an
embedding may advertise smaller values.

The Java 17 generator currently accepts the scalar/record/unit-enum shapes used
here. Presence flags keep the snapshot shape compatible with the Java target;
the frame payload remains a bounded byte run so Java can
upload a texture or fall back to structured-cell rendering without a second
wire contract.

## Presentation contract V3 (terminal fixture V5)

The current raster API uses `renderer_id` as its canonical renderer identity;
the older `backend_id` field remains legacy snapshot vocabulary only. The
capability response supplies independent `default_renderer_id` and
`default_transport_id` values and enumerates every valid atomic tuple of
`renderer_id`, `damage_mode_id`, `transport_id`, and `transport_version` in
`modes`. Every entry in `modes` is selectable, and the two defaults must
identify a tuple in that collection. Clients select one advertised tuple
rather than assembling unvalidated axes.

Known tuples that cannot currently be selected are reported separately in
`unavailable_presentations`. Each `TerminalPresentationUnavailable` repeats
the exact renderer, closed rasterization owner, damage mode, transport, and
transport version and carries a structured `TerminalError`. This lets a
cached capability response preserve working CPU modes while explaining a GPU
initialization failure with its original actionable message. An unavailable
tuple is never a selectable mode, and selecting one is rejected with the
advertised error without silently substituting a fallback.

`TerminalRasterSubscribeRequest` and every `TerminalRasterFrameEvent` carry a
client-chosen opaque `presentation_generation`. It changes whenever any member
of the selected tuple changes. Frame sequences are comparable only within one
connection epoch, session epoch, and presentation generation. The first event
of a fresh generation is a complete resynchronization; an event from a retired
generation is stale even when its numeric sequence is newer. Unsupported tuples
leave the current presentation active rather than silently selecting a
different renderer or transport.

The immutable V4 fixture records presentation contract V2 and advertises
`rust-cpu-fontdue` and `rust-gpu-slug`, both server-owned, across `full-png`,
`full-raw-rgba`, and `dirty-raw-rgba`. V5 records presentation contract V3:
the same three CPU tuples remain valid modes while all three GPU tuples are
unavailable with an `UnsupportedCapability` error describing the Vulkan
device failure. The unchanged RGBA/PNG payload layout remains
`frame_contract_version` 1. V1 through V4 remain immutable historical
fixtures, so there is no compatibility alias for the former
`transport_generation` name in the current raster API.

Raster subscriptions remain request-scoped typed channels on a nonzero service
lane. Closing that presentation lane terminates its subscriptions while the
control lane and independent sibling lanes remain usable; a fresh presentation
lane may be opened on the same connection. Disconnecting the terminal session
is a separate application operation.

## Renderer evidence (terminal fixture V6)

`TerminalRasterFrame.renderer` is a defaulted trailing record containing
bounded evidence for the exact raster frame. It is deliberately not a third
presentation selector: renderer and transport remain the independent user
controls, and `TerminalRasterizationOwner` remains closed metadata on each
advertised tuple. The enclosing event still supplies canonical renderer,
damage-mode, transport, generation, epoch, and sequence identity.

The evidence record exposes fixed timing and retained-resource counters for
CPU and GPU implementations. GPU frames can additionally identify the Vulkan
device and compiled shader source and report geometry construction, uploads,
command recording, submission, completion wait, readback, packetization, and
target/geometry-cache reuse. Explicit presence flags distinguish a measured
zero from a stage that is not applicable to a CPU renderer or the selected
transport. Identity strings are diagnostic and bounded by the enclosing Vox
message limit; no handles, unbounded maps, or dynamically selected values
cross the wire.

V6 extends V5 without changing presentation selection or raster payload
layout. Older peers decode the new trailing field as
`TerminalRasterRendererTelemetry::default()`; generated Java retains the
constructor that omits the defaulted record.

## Evidence

- `vox/test-fixtures/terminal/terminal-contract-v1.json` remains the immutable
  unary contract; `terminal-contract-v2.json` adds the typed subscription,
  bounded coalescing invariants, epochs, and publication telemetry;
  `terminal-contract-v3.json` records the first versioned raster transports,
  `terminal-contract-v4.json` records presentation contract V2, and
  `terminal-contract-v5.json` records typed negative presentation
  capabilities in V3 without modifying V4. `terminal-contract-v6.json` adds
  bounded per-frame renderer evidence without making it a selectable axis or
  changing V5.
- `spec-proto` tests round-trip selectable CPU modes and unavailable GPU
  tuples with actionable errors, all three pixel transports, both
  rasterization-owner values, subscribe/event presentation generations, and
  verify every fixture method id.
- The Java generator test compiles both the existing Review fixture and this
  Terminal service with `javac --release 17`; the Java runtime gate also proves
  presentation-lane close, sibling/control-lane survival, and replacement.
