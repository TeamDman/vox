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

## Evidence

- `vox/test-fixtures/terminal/terminal-contract-v1.json` remains the immutable
  unary contract; `terminal-contract-v2.json` adds the typed subscription,
  bounded coalescing invariants, epochs, and publication telemetry.
- `spec-proto` tests round-trip a representative RGBA snapshot through Phon
  and verify every fixture method id.
- The Java generator test compiles both the existing Review fixture and this
  Terminal service with `javac --release 17`.
