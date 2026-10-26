# Teamy Phon

TeamDman maintains this fork of [upstream Phon](https://github.com/bearcove/phon)
inside [the Teamy Vox repository](https://github.com/TeamDman/vox/tree/release/teamy-registry-2026-10-26/phon).
Its `teamy-phon*` crates are published independently of upstream `phon*` releases.
The original authors, contributors, copyright notices, and MIT OR Apache-2.0
licenses are retained.

Phon is a schema-aware binary format and execution pipeline. `teamy-phon` derives
schemas and descriptors from Teamy Facet metadata and provides the typed
`phon::api` entry point. Schema/value handling, IR, the portable interpreter,
optional target-dependent JIT, and source generation live in separate crates.

```toml
[dependencies]
facet = { package = "teamy-facet", version = "=0.50.0-rc.7" }
phon = { package = "teamy-phon", version = "=0.2.0-rc.8" }
```

Use the whole matching Teamy dependency graph. A type implementing Facet can
still be unsupported by Phon derivation; inspect the returned error rather than
assuming every reflected type has a supported encoding. Older Facet-monorepo
peers must migrate with their matching Vox protocol and generated schemas.

This fork preserves the maintained Java runtime and generator, and uses the
modern Taxon schema identity shared by the qualified Teamy Facet graph. The
[desktop release notes](../docs/teamy-migration/desktop-registry-release.md)
and [cutover validation](../docs/teamy-migration/cloud-cutover-validation.md)
state what was tested. Windows validation uses the portable executor and does
not establish native JIT behavior on other targets.

See the [specification](docs/content/spec.md), [typed API crate](rust/phon/README.md),
[code generator](rust/phon-codegen/README.md), and
[upstream repository](https://github.com/bearcove/phon).

## License

MIT OR Apache-2.0, at your option. See [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE). Existing attribution and contribution terms
remain unchanged.
