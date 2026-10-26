# teamy-vox-service-macros

Published by [TeamDman](https://github.com/TeamDman/vox) as part of the
Teamy fork of [upstream Vox](https://github.com/bearcove/vox). This package is maintained
independently of upstream releases. The original project and its contributors
retain their credits and copyright notices; the MIT OR Apache-2.0 license is unchanged.

The crates.io package is `teamy-vox-service-macros`; its Rust library is `vox_service_macros`.
Use the Teamy Facet/Vox/Phon package family consistently: upstream and fork
packages have separate Rust type identities, even when their version numbers match.

```toml
[dependencies]
vox-service-macros = { package = "teamy-vox-service-macros", version = "=0.10.0-rc.8" }
```

For application RPC code, start with [teamy-vox](https://crates.io/crates/teamy-vox).
For typed binary encoding, start with [teamy-phon](https://crates.io/crates/teamy-phon).
See [this package's API documentation](https://docs.rs/teamy-vox-service-macros) and the
[fork release notes](https://github.com/TeamDman/vox/blob/release/teamy-fork-readmes-2026-10-26/docs/teamy-migration/fork-readme-release.md).

## Component overview

Procedural macros for generating Vox service clients and dispatchers.

## Role in the Vox stack

`vox-service-macros` powers the schema/source-of-truth layer where Rust traits define service contracts.

## What this crate provides

- `#[vox::service]` expansion support
- Generated service trait plumbing, client stubs, and dispatcher glue

## Fits with

- `vox` as the public API surface
- `vox-macros-core` and `vox-macros-parse` for expansion internals
- `vox-hash` for stable method identity

Upstream project: <https://github.com/bearcove/vox>. Teamy fork: <https://github.com/TeamDman/vox>.

## License

MIT OR Apache-2.0, at your option. See [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE) included in this package.
