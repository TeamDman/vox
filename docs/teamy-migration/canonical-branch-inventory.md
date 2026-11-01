# Canonical Teamy Vox branch

Audited November 1, 2026 against all 13 live owned branches in
`https://github.com/TeamDman/vox`. The default branch is `teamy-main`; it is not
protected. The existing desktop checkout was clean at
`4d47e18077d0060160c1f789e1305c7c64071b1a`, identical to the remote tip. Its
historical `TeamDman/roam` remote URL redirects to this same renamed repository.

The October 27 merge `4d47e180` already joined the legacy default history with the
qualified published history. Its source tree equals `ca7cc00d` exactly. No second
merge, cherry-pick, history rewrite or package publication is needed to bring
the modern feature/release work onto `teamy-main`.

## Complete owned remote branch inventory

"Ancestor" means the exact branch tip is reachable from `4d47e180`, checked with
`git merge-base --is-ancestor`, rather than inferred from matching file contents.

| Branch | Audited tip | Disposition |
| --- | --- | --- |
| `teamy-main` | `4d47e18077d0060160c1f789e1305c7c64071b1a` | Canonical integrated source |
| `main` | `847adc1c304952c594d883a47b61c7a221c021a2` | Ancestor; legacy Roam source |
| `copilot/vscode-mkm32x6q-fev6` | `b9b96d3078dac94e6c23619867fa0c5236a226e6` | Ancestor; legacy checkpoint |
| `migration/upstream-phon-schema-audit` | `54599c25411952ff9c361083916280599d580cf0` | Ancestor; superseded intermediate audit |
| `migration/standalone-java-integration-2026-10-23` | `319e6a4b564cbd339e21f8012c9a6050356714d2` | Ancestor; standalone extraction |
| `migration/phon-java-cutover-2026-10-23` | `b6c2cd752c417d70d6c22feb5a3334f69c1cd7d9` | Ancestor; protocol cutover |
| `migration/phon-java-cloud-completion-2026-10-23` | `be6813e7a08c06376a8416eb3368bc97c6de1651` | Ancestor; completed Java/codegen/wire qualification |
| `task/malformed-first-response-schema-20261024` | `86b218a192fbe10d59377a715cee039022214cb7` | Ancestor; response schema fix and consumer evidence |
| `task/bounded-stream-receive-20261024` | `172bf871b5a2bff90607802771c5d23706d9859a` | Ancestor; preallocation frame bounds and negative tests |
| `release/teamy-registry-2026-10-26` | `8e671c1d432dbab6965ae3e4f76890b65fd331b8` | Ancestor; registry qualification |
| `release/teamy-fork-readmes-2026-10-26` | `00c79784334320eff6b73919995f12048ea8c31c` | Ancestor; fork provenance docs |
| `migration/preserved-java-late-credit-4a079ac` | `82dc657bfc28321ba5f2336252912985a4ccf834` | Preserved alternative; one unique commit, intentionally not adopted |
| `teamy-feat-shm-bytes` | `63af099f5d5d8c8d1379da7bba1e311ab0acb92a` | Preserved legacy feature; eight unique commits requiring a modern API port |

The local `sync/published-history-20261027` branch points to the canonical merge.
Local `main`, the Copilot branch and the SHM branch match histories accounted for
above. The duplicate `mine` remote has stale tracking refs, not additional live
owned branches. Upstream feature/experiment and dependency-update branches are
not owned fork features and were not merged indiscriminately.

## Included behavior

- Modern Phon/Taxon protocol and standalone Java runtime, code generation,
  terminal contracts and Rust/Java interoperability fixtures.
- Malformed first RPC response schema repair and bounded stream receives
  (16 MiB default per inbound frame, configurable before allocation/body reads).
- Fork package/provenance work, validated Facet proxy ownership repair and Weavy
  archive build policy from the published desktop source.
- Native and browser Iroh adapter in `rust/vox-iroh`, including authenticated
  identity, ALPN, direct/relay test sources and observability. Presence of these
  tests is not a new live-network qualification. No Veilid adapter is present in
  this workspace.

## Preserved work requiring a decision or port

The older Java late-credit commit changes `VoxConnection.java` and two test
sources. It is not equivalent to the chosen runtime: the recorded 19-case
comparison has four different outcomes. See [credit-behavior.md](credit-behavior.md)
and [credit-matrix.tsv](credit-matrix.tsv). The newer migration retained the
SFM-proven baseline and its JDK guard. Adopting the older runtime wholesale would
undo that choice. A focused follow-up must reconcile nonzero-channel,
opening-lane and malformed-credit validation with explicit protocol tests.

`teamy-feat-shm-bytes` adds zero-copy buffers, ownership/slot hooks, a zip-service
example and SHM/audio design notes against `rust/roam-session` and `rust/roam-shm`.
Neither crate exists in the modern workspace. Its recorded test results and
slot-exhaustion deadlocks apply to that old implementation. A modern port must
choose today's ownership/context mechanism and behavior on non-SHM transports;
blindly restoring these old crates would not qualify the current Vox protocol.
The source and notes remain recoverable at the exact tip above. No branch was
deleted or marked merged through a content-discarding merge.

## Validation boundary

The canonical merge already retains the exact Windows-qualified published source.
Historical qualification is recorded in
[desktop-registry-release.md](desktop-registry-release.md),
[cloud-cutover-validation.md](cloud-cutover-validation.md), and
[bounded-stream-receive-2026-10-24.md](bounded-stream-receive-2026-10-24.md).
Those receipts distinguish Windows tests from the earlier Linux Rust/Java gate.

Fresh November 1 checks used Rust 1.96.0, one Cargo job, one Rust test thread and
the existing Java 17.0.6 JDK. Build output stayed under this checkout's `target/`
on G:. No SFM or storytelling files were changed.

- Core: 88 tests passed; macro core: 25 passed, including malformed-response
  schema and invalid-service-signature regressions.
- Windows stream: 16 tests passed, including oversized prefixes before body
  reads/allocation, directional/exact/zero/raised bounds, truncation,
  cancellation and reconnecting TCP. The sandbox first blocked the loopback test
  with Windows socket error 10013; the full stream rerun with loopback access
  passed. The older Linux receipt's 20-test count is platform-specific.
- All committed Java runtime, test, generated and subject sources compiled with
  `javac --release 17 -Xlint:all -Werror`. Schema bundles passed 1,934 assertions,
  Phon passed 86 assertions, and framing, runtime and generated-response suites
  passed. Runtime socket tests required loopback access outside the sandbox.
- The canonical Java generator confirmed that committed generated sources are
  up to date.
- Markdown relative links and `git diff --check` passed.

No new Iroh direct/relay/browser network qualification, complete workspace test,
consumer upgrade, SFM validation or package publication is claimed by this audit.

The only tracked Actions workflow is release-plz, triggered by pushes to `main`.
There were no `teamy-main` workflow runs at audit time. This audit does not change
release triggers or publish packages. Tracey skill/MCP is unavailable in this
session; no implementation or specification rules are changed by this audit.
