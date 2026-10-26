# teamy-phon-codegen

Published by [TeamDman](https://github.com/TeamDman/vox) as part of the
Teamy fork of [upstream Phon](https://github.com/bearcove/phon). This package is maintained
independently of upstream releases. The original project and its contributors
retain their credits and copyright notices; the MIT OR Apache-2.0 license is unchanged.

The crates.io package is `teamy-phon-codegen`; its Rust library is `phon_codegen`.
Use the Teamy Facet/Vox/Phon package family consistently: upstream and fork
packages have separate Rust type identities, even when their version numbers match.

```toml
[dependencies]
phon-codegen = { package = "teamy-phon-codegen", version = "=0.2.0-rc.8" }
```

For application RPC code, start with [teamy-vox](https://crates.io/crates/teamy-vox).
For typed binary encoding, start with [teamy-phon](https://crates.io/crates/teamy-phon).
See [this package's API documentation](https://docs.rs/teamy-phon-codegen) and the
[fork release notes](https://github.com/TeamDman/vox/blob/release/teamy-fork-readmes-2026-10-26/docs/teamy-migration/fork-readme-release.md).

## Purpose

Generates target-language types and schema constants from Facet types or schema bundles. This fork includes the maintained Java generator alongside the other target implementations. Keep generated sources and the consuming runtime on the same qualified protocol generation.

## License

MIT OR Apache-2.0, at your option. See [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE) included in this package.
