# Desktop registry release

The release source follows `task/bounded-stream-receive-20261024` at
`172bf871b5a2bff90607802771c5d23706d9859a`. This includes the completed modern
Phon/Taxon migration (`5628c00d`), malformed-first-response schema fix
(`e469b86c`), and bounded stream receiver (`20c08f7e`). The legacy remote `main`
and `teamy-main` branches are deliberately unchanged. Earlier documents that
describe every crate as private describe their historical checkpoints.

The initial crates.io release is limited to the dependency closure required by
MFT, terminal and SFM tooling:

- `teamy-phon-schema` 0.2.0-rc.6.
- `teamy-phon`, `teamy-phon-ir`, `teamy-phon-engine`, `teamy-phon-jit`, and
  `teamy-phon-codegen` 0.2.0-rc.7.
- `teamy-vox`, `teamy-vox-core`, `teamy-vox-types`, `teamy-vox-phon`,
  `teamy-vox-schema`, `teamy-vox-stream`, `teamy-vox-websocket`, `teamy-vox-ffi`,
  `teamy-vox-fdpass`, `teamy-vox-rt`, `teamy-vox-rt-macros`,
  `teamy-vox-macros-parse`, `teamy-vox-macros-core`,
  `teamy-vox-service-macros`, and `teamy-vox-codegen` 0.10.0-rc.7.
- `teamy-vox-proto` 0.2.2, containing the terminal service contract.

Other workspace crates remain private. Published dependency aliases retain their
existing Rust names. Facet core/reflection uses published Teamy 0.50.0-rc.7;
JSON/value uses 0.50.0-rc.9. Taxon, Weavy and Copypatch use their existing registry
releases; there is no new release of those packages.

This is the modern wire protocol documented in `cloud-cutover-validation.md`.
Peers using the old Facet-monorepo protocol must upgrade together. Stream receives
default to 16 MiB per frame; callers needing larger inbound frames must opt in.
The bound is not an aggregate queue or process-memory budget.

Desktop qualification uses Rust 1.96.0 on Windows, at most two Cargo build jobs,
and one test thread. The focused Vox gate passed 291 tests (codegen, core, macros,
codec adapter, stream and types); the Phon gate passed 172 tests (schema, engine,
IR, codec, codegen and conformance). The first Phon schema archive passed Cargo's
isolated package verification and publish dry run. Each real publication also
uses Cargo's default archive verification, followed by a registry checksum check.
Platform-specific test counts differ from the recorded Linux
gate. Windows uses Phon's portable executor; macOS native JIT is not exercised.
The earlier recorded Linux Rust–Java and generated-source gates remain distinct
evidence; they are not represented as newly run desktop checks.
