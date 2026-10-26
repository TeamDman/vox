# teamy-phon-schema

Published by [TeamDman](https://github.com/TeamDman/vox) as part of the
Teamy fork of [upstream Phon](https://github.com/bearcove/phon). This package is maintained
independently of upstream releases. The original project and its contributors
retain their credits and copyright notices; the MIT OR Apache-2.0 license is unchanged.

The crates.io package is `teamy-phon-schema`; its Rust library is `phon_schema`.
Use the Teamy Facet/Vox/Phon package family consistently: upstream and fork
packages have separate Rust type identities, even when their version numbers match.

```toml
[dependencies]
phon-schema = { package = "teamy-phon-schema", version = "=0.2.0-rc.7" }
```

For application RPC code, start with [teamy-vox](https://crates.io/crates/teamy-vox).
For typed binary encoding, start with [teamy-phon](https://crates.io/crates/teamy-phon).
See [this package's API documentation](https://docs.rs/teamy-phon-schema) and the
[fork release notes](https://github.com/TeamDman/vox/blob/release/teamy-fork-readmes-2026-10-26/docs/teamy-migration/fork-readme-release.md).

## Purpose

Defines the schema and value model, schema identities, and self-describing codec used by Phon. Use this lower-level crate when inspecting or exchanging schema data; ordinary typed Rust encoding starts with `teamy-phon`.

## License

MIT OR Apache-2.0, at your option. See [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE) included in this package.
