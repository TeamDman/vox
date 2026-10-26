# teamy-phon-ir

Published by [TeamDman](https://github.com/TeamDman/vox) as part of the
Teamy fork of [upstream Phon](https://github.com/bearcove/phon). This package is maintained
independently of upstream releases. The original project and its contributors
retain their credits and copyright notices; the MIT OR Apache-2.0 license is unchanged.

The crates.io package is `teamy-phon-ir`; its Rust library is `phon_ir`.
Use the Teamy Facet/Vox/Phon package family consistently: upstream and fork
packages have separate Rust type identities, even when their version numbers match.

```toml
[dependencies]
phon-ir = { package = "teamy-phon-ir", version = "=0.2.0-rc.8" }
```

For application RPC code, start with [teamy-vox](https://crates.io/crates/teamy-vox).
For typed binary encoding, start with [teamy-phon](https://crates.io/crates/teamy-phon).
See [this package's API documentation](https://docs.rs/teamy-phon-ir) and the
[fork release notes](https://github.com/TeamDman/vox/blob/release/teamy-fork-readmes-2026-10-26/docs/teamy-migration/fork-readme-release.md).

## Purpose

Defines the intermediate representation, descriptors, and thunk bindings shared by the Phon interpreter and optional JIT. This is execution infrastructure; most applications should depend on `teamy-phon`.

## License

MIT OR Apache-2.0, at your option. See [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE) included in this package.
