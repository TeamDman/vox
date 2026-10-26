# teamy-phon

Published by [TeamDman](https://github.com/TeamDman/vox) as part of the
Teamy fork of [upstream Phon](https://github.com/bearcove/phon). This package is maintained
independently of upstream releases. The original project and its contributors
retain their credits and copyright notices; the MIT OR Apache-2.0 license is unchanged.

The crates.io package is `teamy-phon`; its Rust library is `phon`.
Use the Teamy Facet/Vox/Phon package family consistently: upstream and fork
packages have separate Rust type identities, even when their version numbers match.

```toml
[dependencies]
phon = { package = "teamy-phon", version = "=0.2.0-rc.9" }
```

For application RPC code, start with [teamy-vox](https://crates.io/crates/teamy-vox).
For typed binary encoding, start with [teamy-phon](https://crates.io/crates/teamy-phon).
See [this package's API documentation](https://docs.rs/teamy-phon) and the
[fork release notes](https://github.com/TeamDman/vox/blob/release/teamy-fork-readmes-2026-10-26/docs/teamy-migration/fork-readme-release.md).

## Purpose

Provides the typed Rust entry point: derive schemas and descriptors from Facet metadata, then encode and decode through `phon::api`. Unsupported reflected types return derivation errors; deriving Facet alone does not promise that every Rust type is encodable.

## License

MIT OR Apache-2.0, at your option. See [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE) included in this package.
