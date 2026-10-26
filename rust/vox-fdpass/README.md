# teamy-vox-fdpass

Published by [TeamDman](https://github.com/TeamDman/vox) as part of the
Teamy fork of [upstream Vox](https://github.com/bearcove/vox). This package is maintained
independently of upstream releases. The original project and its contributors
retain their credits and copyright notices; the MIT OR Apache-2.0 license is unchanged.

The crates.io package is `teamy-vox-fdpass`; its Rust library is `vox_fdpass`.
Use the Teamy Facet/Vox/Phon package family consistently: upstream and fork
packages have separate Rust type identities, even when their version numbers match.

```toml
[dependencies]
vox-fdpass = { package = "teamy-vox-fdpass", version = "=0.10.0-rc.8" }
```

For application RPC code, start with [teamy-vox](https://crates.io/crates/teamy-vox).
For typed binary encoding, start with [teamy-phon](https://crates.io/crates/teamy-phon).
See [this package's API documentation](https://docs.rs/teamy-vox-fdpass) and the
[fork release notes](https://github.com/TeamDman/vox/blob/release/teamy-fork-readmes-2026-10-26/docs/teamy-migration/fork-readme-release.md).

## Component overview

Cross-platform descriptor/handle passing primitives for local IPC scenarios.

## Role in the Vox stack

`vox-fdpass` is a low-level transport helper used below `Link` setup for passing OS resources between peers.

## What this crate provides

- Unix FD passing support
- Windows handle passing support
- Utilities to integrate descriptor passing into local transport bootstrapping

## Fits with

- `vox-local` and stream-based local connection setup

Upstream project: <https://github.com/bearcove/vox>. Teamy fork: <https://github.com/TeamDman/vox>.

## License

MIT OR Apache-2.0, at your option. See [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE) included in this package.
