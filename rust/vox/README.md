# teamy-vox

Published by [TeamDman](https://github.com/TeamDman/vox) as part of the
Teamy fork of [upstream Vox](https://github.com/bearcove/vox). This package is maintained
independently of upstream releases. The original project and its contributors
retain their credits and copyright notices; the MIT OR Apache-2.0 license is unchanged.

The crates.io package is `teamy-vox`; its Rust library is `vox`.
Use the Teamy Facet/Vox/Phon package family consistently: upstream and fork
packages have separate Rust type identities, even when their version numbers match.

```toml
[dependencies]
vox = { package = "teamy-vox", version = "=0.10.0-rc.9" }
```

For application RPC code, start with [teamy-vox](https://crates.io/crates/teamy-vox).
For typed binary encoding, start with [teamy-phon](https://crates.io/crates/teamy-phon).
See [this package's API documentation](https://docs.rs/teamy-vox) and the
[fork release notes](https://github.com/TeamDman/vox/blob/release/teamy-fork-readmes-2026-10-26/docs/teamy-migration/fork-readme-release.md).

## Component overview

High-level Rust API for defining, implementing, and consuming Vox services.

## Role in the Vox stack

`vox` sits at the RPC surface (`Requests / Channels`) and exposes the developer-facing service model.

## What this crate provides

- `#[vox::service]`-driven service definitions and generated clients/dispatchers
- Core RPC traits and types re-exported for app-level use
- Feature-gated transport facade via `vox::transport`:
  - `transport-tcp` -> `vox::transport::tcp`
  - `transport-local` -> `vox::transport::local`
- Integration point for the rest of the Rust runtime crates

## Fits with

- `vox-core` for connection/driver/runtime internals
- `vox-types` for protocol data model
- `vox-service-macros` for code generation from service traits

Upstream project: <https://github.com/bearcove/vox>. Teamy fork: <https://github.com/TeamDman/vox>.

## License

MIT OR Apache-2.0, at your option. See [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE) included in this package.
