# Teamy Vox

This is [TeamDman's independently maintained fork](https://github.com/TeamDman/vox)
of [upstream Vox](https://github.com/bearcove/vox), with the matching
[upstream Phon](https://github.com/bearcove/phon) history maintained under `phon/`.
The crates.io packages are named `teamy-vox*` and `teamy-phon*`; they are separate
from the upstream packages. Upstream authors and contributors retain their
credits, copyright notices, and MIT OR Apache-2.0 licensing.

Vox defines RPC services as Rust traits. The service macro generates clients and
dispatchers; the matching Phon codec carries typed values and channels. This fork
preserves Teamy's Java runtime/generator and terminal contracts, uses the published
Teamy Facet graph, and includes reviewed fixes for malformed first RPC responses
and bounded stream receives. These are fork changes, not a claim about the current
status of upstream releases.

## Rust dependencies

```toml
[dependencies]
facet = { package = "teamy-facet", version = "=0.50.0-rc.7" }
vox = { package = "teamy-vox", version = "=0.10.0-rc.8" }
```

Keep the `vox` dependency alias: `#[vox::service]` emits paths through `::vox`.
Use matching fork packages throughout the graph; upstream and Teamy Facet types
are distinct Rust identities.

```rust
#[vox::service]
trait Echo {
    async fn echo(&self, value: String) -> String;
}

struct EchoService;

impl Echo for EchoService {
    async fn echo(&self, value: String) -> String {
        value
    }
}
```

This defines a service; starting a transport and driving a connection are separate
steps. See the [crate overview](rust/vox/README.md), [Rust examples](rust-examples),
and [API documentation](https://docs.rs/teamy-vox).

## Protocol and release scope

The modern Phon/Taxon protocol requires old Facet-monorepo peers to upgrade
together. A Rust dependency update alone does not establish wire compatibility.
Stream links default to a 16 MiB receive-frame bound checked before allocation;
this is not a total queue or process-memory limit.

The [desktop release notes](docs/teamy-migration/desktop-registry-release.md)
identify the selected package closure and Windows checks. The
[completed cutover report](docs/teamy-migration/cloud-cutover-validation.md)
records separate Linux/Java validation. Earlier migration handoffs remain in the
repository as historical records; their unfinished/private-package status is not
the status of this release preparation. Legacy `main` and `teamy-main` history has
not been replaced by this release branch.

See [Phon](phon/README.md) for the binary-format layers and
[upstream Vox](https://github.com/bearcove/vox) for upstream development.

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/bearcove/vox/blob/main/LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](https://github.com/bearcove/vox/blob/main/LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.
