# teamy-vox-codegen

Published by [TeamDman](https://github.com/TeamDman/vox) as part of the
Teamy fork of [upstream Vox](https://github.com/bearcove/vox). This package is maintained
independently of upstream releases. The original project and its contributors
retain their credits and copyright notices; the MIT OR Apache-2.0 license is unchanged.

The crates.io package is `teamy-vox-codegen`; its Rust library is `vox_codegen`.
Use the Teamy Facet/Vox/Phon package family consistently: upstream and fork
packages have separate Rust type identities, even when their version numbers match.

```toml
[dependencies]
vox-codegen = { package = "teamy-vox-codegen", version = "=0.10.0-rc.9" }
```

For application RPC code, start with [teamy-vox](https://crates.io/crates/teamy-vox).
For typed binary encoding, start with [teamy-phon](https://crates.io/crates/teamy-phon).
See [this package's API documentation](https://docs.rs/teamy-vox-codegen) and the
[fork release notes](https://github.com/TeamDman/vox/blob/release/teamy-fork-readmes-2026-10-26/docs/teamy-migration/fork-readme-release.md).

## Component overview

Language binding generator for Vox service descriptors.

## Role in the Vox stack

`vox-codegen` bridges Rust-defined schemas to non-Rust clients/servers above the RPC surface.

## What this crate provides

- TypeScript and Swift code generation targets
- Rendering of service descriptors into client/server scaffolding

## Fits with

- `vox` service definitions and generated descriptors
- `vox-hash` and `vox-types` for shared protocol identity and type model

Upstream project: <https://github.com/bearcove/vox>. Teamy fork: <https://github.com/TeamDman/vox>.

## License

MIT OR Apache-2.0, at your option. See [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE) included in this package.
