# vox-stream

Byte-stream transport binding for Vox over `AsyncRead`/`AsyncWrite`.

## Role in the Vox stack

`vox-stream` implements the `Link` layer using length-prefixed framing on stream transports.

## What this crate provides

- Framing and link adapters for TCP/Unix/stdio style byte streams
- Local IPC transport support (Unix sockets on Unix, named pipes on Windows)
- Runtime-compatible transport glue for connection establishment

## Fits with

- `vox-core` connection and driver runtime
- `vox-local` as a compatibility shim re-export
- `vox-types` for transport message payloads

Part of the Vox workspace: <https://github.com/bearcove/vox>

## Local receive bounds

`StreamLink` defaults to a 16 MiB maximum **incoming payload** (the four-byte
prefix is excluded), matching Java's default frame limit. Every `StreamLink`
constructor inherits it: `new`, `tcp`, `unix`, `stdio`, and Windows
`named_pipe_client`. Before `split`, callers can set
`.max_receive_frame_bytes(limit_in_bytes: u32)`. Zero accepts only empty frames;
there is no implicit unlimited mode. `TcpLinkSource` exposes the same builder and
applies it to every new connection, including reconnects. The constant
`DEFAULT_MAX_RECEIVE_FRAME_BYTES` is also re-exported through
`vox::transport::tcp`.

Configure each receiving endpoint independently: the acceptor's limit protects
against incoming client frames, and the initiator's protects against incoming
server frames. The limit applies before transport/application handshakes too;
peers do not negotiate or override it. For example, an accepted socket uses
`StreamLink::tcp(socket).max_receive_frame_bytes(8 * 1024 * 1024)` and a reconnecting
client uses `tcp_link_source(address).max_receive_frame_bytes(8 * 1024 * 1024)`.

The unsigned length is checked before narrowing to `usize`, allocating a body,
or requesting its bytes. An oversized prefix produces `InvalidData`, then EOF
on subsequent receives. It is not drained, skipped, or truncated. Normal empty
frames, bounded frames, partial reads, cancellation and clean EOF keep their
existing semantics. The framing reader does not add read-ahead buffering; a
caller-provided buffered `AsyncRead` can still prefetch according to its own bounds.
Existing constructor signatures and associated receive types are preserved.

Compatibility: formerly accepted frames larger than 16 MiB now fail unless the
receiving endpoint explicitly opts into a higher local bound. Larger Java raster
responses (such as an SFM client configured for 65 MiB) need their own Java receive
setting; this Rust **receive-only** policy does not cap Rust's outgoing frames.
Outbound encoding still checks the independent `u32` prefix ceiling. Raising a
bound deliberately increases memory exposure.

This is a per-frame bound, not a per-process or per-connection byte budget: the
existing queue can retain 128 received frames, plus a frame being read and frames
held by consumers. It does not impose read deadlines or application upload quotas.
`FdStreamLink`/Unix `LocalLink` use separate descriptor-capable framing and are not
covered by this fix; their accumulator and descriptor-count bounds remain separate
work. WebSocket/tungstenite and browser transports have their own framing rules.
Wrappers using ordinary `StreamLink` (including Iroh and Windows `LocalLink`)
inherit the default; wrapper-specific configuration APIs are outside this patch.
