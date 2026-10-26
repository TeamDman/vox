# teamy-vox-types

Published by [TeamDman](https://github.com/TeamDman/vox) as part of the
Teamy fork of [upstream Vox](https://github.com/bearcove/vox). This package is maintained
independently of upstream releases. The original project and its contributors
retain their credits and copyright notices; the MIT OR Apache-2.0 license is unchanged.

The crates.io package is `teamy-vox-types`; its Rust library is `vox_types`.
Use the Teamy Facet/Vox/Phon package family consistently: upstream and fork
packages have separate Rust type identities, even when their version numbers match.

```toml
[dependencies]
vox-types = { package = "teamy-vox-types", version = "=0.10.0-rc.9" }
```

For application RPC code, start with [teamy-vox](https://crates.io/crates/teamy-vox).
For typed binary encoding, start with [teamy-phon](https://crates.io/crates/teamy-phon).
See [this package's API documentation](https://docs.rs/teamy-vox-types) and the
[fork release notes](https://github.com/TeamDman/vox/blob/release/teamy-fork-readmes-2026-10-26/docs/teamy-migration/fork-readme-release.md).

## Component overview

Protocol and runtime data model shared across Vox implementations.

## Role in the Vox stack

`vox-types` spans the `RequestScope`, `Lane`, and `Connection` layers by defining shared message and control types.

## What this crate provides

- Wire-level and runtime-facing enums/structs used by the protocol
- Request/response and channel-related types
- Common error and metadata types consumed by runtime and transports

## Fits with

- `vox`, `vox-core`, and transport crates (`vox-stream`, `vox-websocket`)
- `vox-codegen` when generating non-Rust bindings

Upstream project: <https://github.com/bearcove/vox>. Teamy fork: <https://github.com/TeamDman/vox>.

## License

MIT OR Apache-2.0, at your option. See [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE) included in this package.
