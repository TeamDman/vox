# teamy-vox-macros-core

Published by [TeamDman](https://github.com/TeamDman/vox) as part of the
Teamy fork of [upstream Vox](https://github.com/bearcove/vox). This package is maintained
independently of upstream releases. The original project and its contributors
retain their credits and copyright notices; the MIT OR Apache-2.0 license is unchanged.

The crates.io package is `teamy-vox-macros-core`; its Rust library is `vox_macros_core`.
Use the Teamy Facet/Vox/Phon package family consistently: upstream and fork
packages have separate Rust type identities, even when their version numbers match.

```toml
[dependencies]
vox-macros-core = { package = "teamy-vox-macros-core", version = "=0.10.0-rc.8" }
```

For application RPC code, start with [teamy-vox](https://crates.io/crates/teamy-vox).
For typed binary encoding, start with [teamy-phon](https://crates.io/crates/teamy-phon).
See [this package's API documentation](https://docs.rs/teamy-vox-macros-core) and the
[fork release notes](https://github.com/TeamDman/vox/blob/release/teamy-fork-readmes-2026-10-26/docs/teamy-migration/fork-readme-release.md).

## Component overview

Core code generation engine for Vox service procedural macros.

## Role in the Vox stack

`vox-macros-core` is internal codegen infrastructure behind the service-definition layer.

## What this crate provides

- Macro expansion logic shared by public proc-macro entry points
- Token generation for clients, dispatchers, and service detail artifacts

## Fits with

- `vox-service-macros` (public proc-macro crate)
- `vox-macros-parse` (grammar/parser front-end)

Upstream project: <https://github.com/bearcove/vox>. Teamy fork: <https://github.com/TeamDman/vox>.

## License

MIT OR Apache-2.0, at your option. See [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE) included in this package.
