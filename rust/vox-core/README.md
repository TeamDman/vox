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

## License

MIT OR Apache-2.0, at your option. See [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE) included in this package.
