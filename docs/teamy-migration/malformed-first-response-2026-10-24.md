# Preserve response bindings after malformed first input

Local tested implementation: `e469b86cdbab46502b913fb2af48dfacbd8e26f0`, based on
published report `be6813e7a08c06376a8416eb3368bc97c6de1651`. This report is a
separate documentation commit. Nothing was pushed or published.

The real Java consumer probe in TeamDman/teamy-terminal at local source
`5039d481a9a281e7a0b86429ddd5f41272d9a9d8` exposed this sequence:

1. First `get_content` call has empty, malformed argument bytes.
2. Rust returns `InvalidPayload`.
3. A valid `get_content` call for a missing session returns its declared
   application error, but Java fails decoding with `method payload schema is
   incompatible`, caused by `trailing bytes: 84`.

The first rejection previously advertised the success type paired with
`VoxError<Infallible>`. That established an incorrect per-method response binding.
Both generated argument decode paths now use the declared application-error type.
The normal API and error discriminants are unchanged. This implements the existing
`schema.tracking.bindings` and `rpc.error.scope` requirements, reviewed manually;
Tracey skill/MCP are unavailable in this executor.

Qualification: 25 macro tests, 88 core tests, 10 generated-service tests and eight
schema-compatibility tests passed under Rust 1.96.0. The fallible snapshot changes
only its decode-error response type; a retained-fallible snapshot covers the other
path. The seven Phon fixtures are unchanged. Changed Rust source formatting passed.

An isolated Terminal checkout at published source
`9078e66b1f327d919d19c4170dd038b9dbb84523` was built with only a command-line Cargo
path override for this macro crate (and its unchanged local parser dependency).
The real Java TCP probe fails reproducibly on the original binary and passes with
that overlay, including subsequent typed errors, session isolation, Unicode PTY
output, pushed CPU PNG, stream cancellation completion and disconnect rejection.
Java uses the unchanged qualified JAR from `5628c00...`, SHA256
`be441a57e11d4981d283306fe471874e625bf2f423c83a91ce8f44dde623d062`.

Binary SHA256 values:

- Original Terminal: `2834a61a8e877f5eb73d3c61590ff276db5f92d148ef8c0d8d41527749de6b78`
- Local macro overlay: `e0856fedbd38c3be8bbf61fd7ae573929b7af406db4de47ac82dfd25898b1d35`

Durable probe, complete receipt and reproduction instructions are in the Terminal
branch `task/vox-terminal-java-integration-20261024`, under
`scripts/vox-java-terminal-probe/` and
`docs/notes/vox-java-consumer-integration-2026-10-24.md`.
This remains an overlay qualification, not a green result for published consumer
pins. After separate publication approval, advance the consumer Rust pins and
rebuild generated dispatchers; a Java-only update cannot repair this defect.

No full Java/library packaging gate was repeated: Java source and artifact bytes
are unchanged. No GPU, desktop, Minecraft, Windows, package installation, network
policy change or main merge ran. The prior Java qualification remains attached to
its original tested source rather than relabeled as this commit's test result.
