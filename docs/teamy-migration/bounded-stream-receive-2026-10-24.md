# Bounded Rust stream receive checkpoint

Source checkpoint: `20c08f7e7e04d0836e403cc3ba5ca0827fca8ee2` on local branch
`task/bounded-stream-receive-20261024`, based on published
`86b218a192fbe10d59377a715cee039022214cb7`. The later report commit does not represent
another tested runtime revision. No publication, dependency/toolchain changes or
security configuration changes were performed.

## Defect and correction

The prior ordinary Rust stream receiver decoded a peer-authored u32 length and
allocated `vec![0; len]` without checking a local maximum. It could request nearly
4 GiB before the application handshake or quotas. Java already checked its local
frame maximum before body allocation; the malformed-first-response schema fix was
unrelated and remains preserved.

`StreamLink` now defaults to a 16 MiB local incoming payload limit. Its
`max_receive_frame_bytes(u32)` builder configures the limit before splitting;
`TcpLinkSource` propagates the same setting to each new link, including reconnects.
All ordinary `StreamLink` constructors inherit the default. The public constant is
also re-exported by `vox::transport::tcp`. Constructors and associated receive
types remain source-compatible.

The reader compares the decoded **unsigned** u32 with local policy, then performs
checked conversion to usize, and only then allocates/reads the body. Above-limit
frames yield `InvalidData`, followed by EOF on subsequent receives; no skipping,
truncation or resynchronization is attempted. Zero explicitly permits only empty
frames. The framing task no longer adds `BufReader` read-ahead that could consume
body bytes while acquiring the fixed prefix. Callers can still supply a buffered
reader with their own bounded read-ahead. Historical associated receive types are
retained as phantom types to avoid an incidental public type break.

## Compatibility and limits

The policy applies to both initiator and acceptor receive directions, including
frames before/during handshakes. It is local, not negotiated. Valid bounded frames,
empty frames, partial reads, cancellation and clean EOF preserve their behavior.
Previously accepted incoming frames over 16 MiB now require an explicit local
override. No outgoing cap or wire encoding changes: Java clients configured to
receive larger Rust-generated raster frames retain that separate Java policy.

This is not an aggregate memory budget. The existing receive queue holds up to 128
frames plus one being read, and consumers may retain more; at the default, that
can exceed 2 GiB across one saturated connection. Read deadlines, connection
counts, application quotas and throughput tuning are separate concerns. Raising
the limit explicitly raises memory exposure. Removing read-ahead may affect small
frame throughput; no performance benchmark was run.

Adjacent implementations were inspected, not rewritten. Unix `FdStreamLink` and
Unix `LocalLink` retain separate unbounded accumulator/descriptor-count concerns.
Windows `LocalLink` and Iroh wrappers inherit the ordinary stream default but do
not gain wrapper-level configuration APIs in this patch. Higher-level TCP helpers
also inherit the default; callers needing an override can construct `StreamLink`
or `TcpLinkSource` directly. WebSocket/tungstenite and browser framing are separate.

## Validation

Tests used the existing Rust 1.96.0 and Temurin 21.0.8+9 (Java release 17). No giant
body buffers were constructed for rejection tests. New oversized fixtures contain
only the six-byte prologue and four-byte prefix. Their reader asserts that no
body read or read-ahead is requested. Lengths include default+1, the high bit, and
u32::MAX. Boundary tests use eight-byte bodies; the full default and raised bound
are checked arithmetically without allocating those bodies.

- `cargo test --offline --locked -p teamy-vox-stream`: 20 passed. Covers oversized
  prefixes, exact/raised/zero bounds, directional limits, partial/truncated headers
  and bodies, cancellation, normal traffic and two reconnecting TCP source links.
- `cargo test --offline --locked -p teamy-vox-core -p teamy-vox-macros-core`:
  88 core and 25 macro tests passed, preserving the malformed-first-response fix.
- `cargo run --offline --locked -p teamy-vox-xtask -- test-java`: passed generated
  source drift check, 1,934 schema-bundle assertions, 86 Phon assertions, framing,
  runtime, and generated-response integration tests.
- `cargo clippy --offline --locked -p teamy-vox-stream --all-targets -- -D warnings`:
  passed. `rustfmt --edition 2024 --check rust/vox-stream/src/lib.rs rust/vox/src/lib.rs`:
  passed. A whole-repository format pass was not used as a gate because unrelated
  existing files need formatting; incidental formatting was reverted.
- `cargo test --offline --locked -p spec-tests --test java_wire -- --test-threads=2`:
  all 12 real Rust/Java TCP tests passed, including calls and channels in both
  directions, invalid payload, schema evolution, application errors, cancellation
  and disconnect. The shared spec harness emits 363 unused/dead-code warnings;
  these did not fail the gate and were not changed. New oversized-prefix tests
  exercise the link before application-handshake handling; no separate oversized
  handshake test through each high-level role builder was added.
- Independent read-only review at the exact source commit found no remaining
  blocker after spec scope, directional coverage and residual budgets were clarified.

The repo-required Tracey skill/tool was unavailable after discovery. The spec and
`r[impl]/r[verify]` annotations were updated manually; automated Tracey validation
is unrun. Windows, 32-bit, wasm, Iroh direct/relay and descriptor transport hardening
were not qualified by this change. No exposed service or third-party target was
used; network tests are existing/local loopback tests.

Structured results and local log hashes: [validation receipt](bounded-stream-receive-2026-10-24.json).
