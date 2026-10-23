# Cloud Phon/Taxon cutover qualification

This continuation starts at `b6c2cd752c417d70d6c22feb5a3334f69c1cd7d9`.
It retains the current protocol exclusively; peers must upgrade together.
The prior checkpoint and historical credit implementation remain in Git history.

## Changes

- Java schema tables, DTO closures and inline list closures use bounded hexadecimal
  constants, decoded with Java 17 `HexFormat`. This avoids a bytecode instruction
  per byte and the JVM method limit. A 262,147-byte generated fixture is compiled,
  executed and compared byte-for-byte with its Rust input.
- The Java wire subject consumes `SCHEMA_BUNDLE`. PHONSCM1 tables remain distinct
  from the Vox root/auxiliary closure envelope.
- Schema closures validate references, arity, identities and resource bounds before
  recursive encoding. Bare-schema encoding also bounds reference depth and work.
- Vox now treats the second `schema_from_bytes` result as owned allocation bytes.
  Phon already rejects trailing input; Vox no longer mistakes that allocation
  counter for consumed wire bytes. A malformed-entry regression preserves strict
  trailing-byte rejection.
- The Java timeout test waits for the driver's cancellation signal before checking
  its count. Completing the future and sending cancellation are distinct scheduler
  actions. The assertion remains; runtime cancellation and credit code are unchanged.

The root and nested locked graphs use one registry `taxon 0.2.0-rc.5` and registry
`teamy-facet` / `teamy-facet-core 0.50.0-rc.7`. There is no legacy `teamy-taxon`
identity or compatibility adapter. Locks and all seven Rust fixture bytes are unchanged.

## Reproduction and evidence

With locked dependencies acquired, activate Rust 1.96.0 and Eclipse Adoptium
Temurin 21.0.8+9-LTS, then run:

```sh
bash tools/migration/validate-java-cutover.sh /tmp/vox-cutover-receipt
```

The runner records the exact commit, dirty state, tools, per-stage exits, log
hashes and two independent Java package hashes. It runs the Phon corpus/schema/
codegen tests, Vox codegen and core/type/Phon tests, Java runtime and conformance,
12 real Rust–Java TCP tests, TypeScript checks and reproducible packaging. Java
compilation uses `--release 17`. The package gate checks the exact JDK identity.
This is cloud Linux qualification, not Windows, macOS native JIT, Swift runtime,
Minecraft or the hardened container recipe's execution receipt.

The Java admission tests compare all seven independent Rust fixture bytes and
identities, upstream point-bundle and Semantic bare-schema goldens, generic compact
substitution and the existing recursive-schema corpus. They test every truncation,
trailing bytes, magic/version/tags, malformed strings and references, ordering,
duplicates, unused/out-of-range symbols, hash mismatch, arity, undeclared variables,
qualified names, and count/byte/work/depth limits. Deep in-memory graphs are rejected
before recursive emission. Semantic representation equality does not grant typed
semantic compatibility; generic structural inspection remains supported.

During investigation, one Java runtime run failed with `local request cancelled`;
it passed on direct retry. A repeated diagnostic run separately reproduced the
scheduler race in the timeout test. After synchronizing that test, ten complete
runtime repetitions passed. The original cancellation failure was not reproduced;
retain it as an observed transient rather than attributing it conclusively to the
fixed test race. Subsequent gates must remain visible in the receipt.

The unmodified historical 19-case credit test from
`82dc657bfc28321ba5f2336252912985a4ccf834` was run against this runtime. It retained
exactly the baseline's 15 passes and four differences documented in
[credit-behavior.md](credit-behavior.md): zero-channel and opening-lane acceptance,
and control/unopened-lane diagnostic differences. Those older expectations were
not substituted into this branch. Malformed-credit cases in that matrix retain
the original outcomes. Tracey skill/MCP is unavailable, so its synchronization
check remains unrun.

## Consumer boundary

No consumer pin is selected automatically by a library test. The available
`TeamDman/teamy-terminal` checkout and fresh remote `main` are both
`2b7833157d0cd7e5d4f2922bed2ee5f6ef112b53`. That tree has no Vox consumer or
control/streaming workspace. Its known Facet identity mismatch remains: cancellation
uses `d0c3140` while the CLI expects `2bfd3c2`. Updating that older template would
not prepare the terminal consumer described in the handoff.

Terminal work needs the actual control/streaming source lineage transferred into
an authorized cloud checkout. Reconcile both Vox dependencies and their Facet
identities there, then test control RPC, streaming and cancellation together.
SFM source is absent here. Continue from the fresh
`capture/sfm435-issue610-cloud` lineage in its authorized environment, read its
current instructions, update the main-source projection and artifact provenance,
and regenerate and validate the affected contexts. Do not use an older desktop
checkout. Consumer branch publication still requires separate confirmation.
