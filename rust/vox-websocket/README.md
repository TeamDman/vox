# teamy-vox-websocket

Published by [TeamDman](https://github.com/TeamDman/vox) as part of the
Teamy fork of [upstream Vox](https://github.com/bearcove/vox). This package is maintained
independently of upstream releases. The original project and its contributors
retain their credits and copyright notices; the MIT OR Apache-2.0 license is unchanged.

The crates.io package is `teamy-vox-websocket`; its Rust library is `vox_websocket`.
Use the Teamy Facet/Vox/Phon package family consistently: upstream and fork
packages have separate Rust type identities, even when their version numbers match.

```toml
[dependencies]
vox-websocket = { package = "teamy-vox-websocket", version = "=0.10.0-rc.9" }
```

For application RPC code, start with [teamy-vox](https://crates.io/crates/teamy-vox).
For typed binary encoding, start with [teamy-phon](https://crates.io/crates/teamy-phon).
See [this package's API documentation](https://docs.rs/teamy-vox-websocket) and the
[fork release notes](https://github.com/TeamDman/vox/blob/release/teamy-fork-readmes-2026-10-26/docs/teamy-migration/fork-readme-release.md).

## Component overview

WebSocket transport binding for Vox links.

## Role in the Vox stack

`vox-websocket` implements the `Link` layer on top of WebSocket binary frames.

## What this crate provides

- Client/server link adapters over WebSocket transports
- Native and wasm-friendly runtime integration points

## Fits with

- `vox-core` for connection and lane orchestration
- `vox-types` for protocol payloads and control messages
- `vox` for generated clients and service dispatchers

Upstream project: <https://github.com/bearcove/vox>. Teamy fork: <https://github.com/TeamDman/vox>.

## License

MIT OR Apache-2.0, at your option. See [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE) included in this package.
