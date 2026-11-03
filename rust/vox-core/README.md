# teamy-vox-core

Published by [TeamDman](https://github.com/TeamDman/vox) as part of the
Teamy fork of [upstream Vox](https://github.com/bearcove/vox). This package is maintained
independently of upstream releases. The original project and its contributors
retain their credits and copyright notices; the MIT OR Apache-2.0 license is unchanged.

The crates.io package is `teamy-vox-core`; its Rust library is `vox_core`.
Use the Teamy Facet/Vox/Phon package family consistently: upstream and fork
packages have separate Rust type identities, even when their version numbers match.

```toml
[dependencies]
vox-core = { package = "teamy-vox-core", version = "=0.10.0-rc.9" }
```

For application RPC code, start with [teamy-vox](https://crates.io/crates/teamy-vox).
For typed binary encoding, start with [teamy-phon](https://crates.io/crates/teamy-phon).
See [this package's API documentation](https://docs.rs/teamy-vox-core) and the
[fork release notes](https://github.com/TeamDman/vox/blob/release/teamy-fork-readmes-2026-10-26/docs/teamy-migration/fork-readme-release.md).

## Component overview

Core runtime for connections, lanes, drivers, and conduits.

## Role in the Vox stack

`vox-core` primarily implements the `Connection`, `Lane`, and `Conduit` layers.

## What this crate provides

- Connection builders (`initiator`, `acceptor`) and handles
- Driver runtime for dispatching inbound calls and issuing outbound calls
- Connection lifecycle primitives and runtime glue

## Fits with

- `vox` for service-facing APIs
- Link/transport crates (`vox-stream`, `vox-websocket`, `vox-local`)
- `vox-types` for protocol state and message types

Upstream project: <https://github.com/bearcove/vox>. Teamy fork: <https://github.com/TeamDman/vox>.

## Resource limits for an untrusted peer

Connection builders accept an opt-in `ConnectionResourceLimits` via
`.resource_limits(limits)`. The default is `None`, without connection-level
lifetime quotas. Driver channel mailboxes still have a fixed maximum of 16
pending writer-schema updates per channel, separate from item credit; exceeding
that limit closes the connection rather than blocking the lane. Driver-backed
receivers reject items exceeding advertised credit. Raw and proxy lanes must
enforce their own corresponding inbound-credit and queue bounds. The
configured local handshake settings must fit the local
request and initial-credit ceilings; peer handshake and service-lane settings
that exceed the peer ceilings are rejected. An accepted peer lane advertises
the locally configured request limit and channel capacity, rather than copying
the opener's values. Applying this option through the lower-level transport or
conduit builders protects both inbound lanes and the control lane.

A starting configuration for a Mover receiver can be constructed as follows;
the file protocol must choose its own chunk size, timeouts, concurrent session
limit, authentication, and disk quotas. Values here are examples, not a
process-wide memory guarantee:

```rust
use vox_core::ConnectionResourceLimits;

let limits = ConnectionResourceLimits {
    max_service_lanes: 1,
    max_peer_lane_opens: 32,
    max_inbound_requests_per_lane: 4,
    max_initial_channel_credit: 4,
    max_peer_channel_credit: 16,
    max_peer_concurrent_requests: 64,
    max_peer_calls_per_lane: 1_000_000,
    max_peer_channel_ids_per_lane: 64,
    max_schemas_per_lane: 128,
    max_schema_bytes_per_lane: 4 * 1024 * 1024,
    max_message_processing_time: std::time::Duration::from_secs(30),
};
// Example with an already configured Link and a Mover lane acceptor:
// vox_core::acceptor_on(link).max_concurrent_requests(4)
//     .channel_capacity(4).resource_limits(limits).on_lane(acceptor)
//     .establish_connection().await?;
```

The lane-open, peer-call, distinct peer channel-ID, and schema budgets are lifetime
quotas. Repeated schemas consume count and byte quota even if they replace an
existing binding. Early channel delivery is supported within the ID quota;
the ID budget also bounds peer-created retained terminal and debug entries.
Exhausting a peer-call, channel or schema quota closes the connection; exhausting the
lane-open quota sends a rejection and closes it. The live-lane quota is reusable
after a lane closes. Locally opened lanes and application-created resources
remain the application's responsibility.

In bounded mode, `max_message_processing_time` also closes the connection if
one inbound message cannot finish routing or sending its control response in
time. This makes queue backpressure finite, so choose a value that allows the
application to consume a full queue under expected load. It cannot preempt a
blocking synchronous callback or replace handshake and application deadlines.

If a resource-triggered rejection or protocol-error response cannot be flushed
within one second, the runtime stops waiting, cancels its outbound worker, and calls the conduit's
`abort` hook. `BareConduit` forwards this to the link; on native targets,
`StreamLink` aborts its writer task. The default hook on other conduits and
links is a no-op, and the WASM task abort is a no-op, so those environments
need their own forced-shutdown behavior.

The accumulated grant ceiling is enforced by `Driver`; a raw lane consumer or
`proxy_lanes` forwards channel messages without that driver check. Such users
must enforce credit limits on their own or on the protected destination. The
initial peer credit is checked during establishment in all modes.

For byte streams also set `StreamLink::max_receive_frame_bytes` and
`StreamLink::receive_queue_capacity` **before** creating the connection. The
stream receive queue can hold its configured number of complete frames plus
one frame being read. In addition, each active lane has a 64-message routing
queue and each inbound channel has item slots sized by local initial credit,
up to 16 separate ordered writer-schema slots, and a terminal slot; frames
may remain held by handlers and consumers. Multiply the configured counts by
the maximum frame size when assessing memory, then account separately for
decoding, schema plans, outbound queues, allocator overhead, TLS, application
buffers, and concurrent connections. These settings alone are not a proven
whole-process memory bound, decoder-complexity limit, or application upload
quota; other Link implementations need their own transport limits.

## License

MIT OR Apache-2.0, at your option. See [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE) included in this package.
