# Generated Java fixture

`cargo xtask codegen --java` writes the Java 17 unary fixture below this
directory. The fixture is generated from Rust `ServiceDescriptor` and Facet
shape values; it does not parse Rust source text.

`cargo xtask check-codegen --java` regenerates the same in-memory file set and
fails on missing, changed, or unexpected Java compilation units. The
`vox-codegen` Java target also compiles an equivalent fixture with
`javac --release 17` in its focused Rust test.

The first fixture deliberately contains only the frozen accepted slice:

- `echo(String) -> String`;
- `inspect(NestedRequest) -> NestedResponse`; and
- `divide(DivideRequest) -> Result<DivideResponse, DivideByZero>`.

Channels, dynamic Facet values, file descriptors, and opaque/external shapes
fail generation with a diagnostic naming the service, method, and rejected
shape. The full Testbed service contains channels and is therefore not silently
filtered into a misleading partial Java service.

Generated files are not hand edited. They compile against the Phon and Vox Java
runtime interfaces frozen in `docs/design/java-17-vertical-slice.md`.

Each method also receives a generated response adapter for the complete
`Result<T, VoxError<E>>` Phon wire shape. Clients unwrap successful results,
retain declared `User(E)` errors as `VoxResult`, and surface infrastructure
variants exceptionally. No generated or runtime-specific outer payload tag is
added.
