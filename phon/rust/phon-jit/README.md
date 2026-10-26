# teamy-phon-jit

Published by [TeamDman](https://github.com/TeamDman/vox) as part of the
Teamy fork of [upstream Phon](https://github.com/bearcove/phon). This package is maintained
independently of upstream releases. The original project and its contributors
retain their credits and copyright notices; the MIT OR Apache-2.0 license is unchanged.

The crates.io package is `teamy-phon-jit`; its Rust library is `phon_jit`.
Use the Teamy Facet/Vox/Phon package family consistently: upstream and fork
packages have separate Rust type identities, even when their version numbers match.

```toml
[dependencies]
phon-jit = { package = "teamy-phon-jit", version = "=0.2.0-rc.9" }
```

For application RPC code, start with [teamy-vox](https://crates.io/crates/teamy-vox).
For typed binary encoding, start with [teamy-phon](https://crates.io/crates/teamy-phon).
See [this package's API documentation](https://docs.rs/teamy-phon-jit) and the
[fork release notes](https://github.com/TeamDman/vox/blob/release/teamy-fork-readmes-2026-10-26/docs/teamy-migration/fork-readme-release.md).

## Purpose

Provides the optional copy-and-patch backend for supported targets. It lowers Phon IR using native stencils. Availability depends on Weavy and the target; the desktop Windows qualification exercised the portable interpreter, not native JIT execution.

## License

MIT OR Apache-2.0, at your option. See [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE) included in this package.
