# Standalone Teamy Vox migration

**Current branch:** the in-progress full protocol cutover and cloud continuation
instructions are in [cutover-handoff.md](cutover-handoff.md). The validation and
legacy compatibility discussion below describe the earlier preserved checkpoint,
not successful validation of the new protocol.

This branch preserves the owned Java and terminal work from the former Facet
monorepo and joins it to the current standalone Vox history. It is a review
checkpoint, not a package release or a consumer upgrade.

## Sources and history

| Source | Commit | Disposition |
| --- | --- | --- |
| Upstream Vox | `4caa67526d1f599b430f0593ea62a76f53516e2f` | First parent of the Vox integration |
| Owned Facet Java/SFM fix | `e76bef6ade844985e98d8799d139c905fba28c10` | Primary Vox and Phon extraction source |
| Upstream Phon | `e8bceb06ca0840f053366bd364a8f2cc3cecab24` | First parent of the Phon integration |
| Older owned late-credit work | `4a079ac1c8a8bb8a914811ef55945bc1d9a9fef3` | Separate preserved branch; see the credit comparison |
| Local Phon limits fix | `adac882811c42535d6eafc5fea48c794993153e8` | Patch-identical to included `e3193ab547b22989b73937f76043556db20adf5b` |

The repository was renamed from `TeamDman/roam` to `TeamDman/vox`, preserving
repository ID `1137888712`, settings, and existing refs. The original `main`,
`teamy-main`, `teamy-feat-shm-bytes`, and `copilot/vscode-mkm32x6q-fev6` branches
were not rewritten. The older shared-memory work remains on those original
branches; this migration does not claim to port that old API to current Vox.

Before extraction, independent copies preserved all original local Vox/Facet
refs. An audit of 48 local and current owned Facet branch refs found the older
late-credit implementation as the remaining patch-distinct owned Vox/Phon work.
Other differences on newer Facet branches were upstream extraction/removal and
upstream fixes already represented by standalone upstream. Dirty worktrees were
recorded and were not edited.

The extraction uses Git path-limited fast-export/fast-import with complete trees
for each retained commit. It removes only the `vox/` or `phon/` path prefix.
Every extracted tree was compared with its original subtree; every author,
committer (including timestamps), and commit message was compared byte-for-byte.
Unrelated monorepo commits are omitted by path-history simplification. Commit
IDs necessarily change when trees/parents change, and old signatures cannot
authenticate rewritten commits. Original commits remain in the source history
and backups; the maps make the correspondence explicit.

| Map | Retained commits | Extracted tip |
| --- | ---: | --- |
| [Vox](vox-commit-map.json) | 1,602 | `2834b093560eaebf020dfe62bf925078b91db6aa` |
| [Phon](phon-commit-map.json) | 327 | `452ef3bb511ff4b610ff3931bc734027c485c9c3` |
| [Older credit branch](older-credit-commit-map.json) | 1,602 | `82dc657bfc28321ba5f2336252912985a4ccf834` |

The [extraction script](../../tools/migration/extract_history.py) refuses to
replace an existing destination ref and verifies every resulting tree. A trial
export without complete per-commit trees failed verification at a historical
merge; that candidate was not used or published.

Upstream restored the Facet-era Vox workspace in squashed commit
`4b58849b534384cc3c70b60bd2415e3108e21cc0`. Its ordinary Git merge base with the
extraction predates the shared Facet changes. File reconciliation therefore used
the verified common source ancestor `7b0e73a374f41da494a27b413eacc9a9e8739cf2`:

- Vox base: source `04a102f66dbbeb2cfcc5d0700b3d8391571ba779`, extracted
  `6109cdfd2ac41991c12daa2b2ede08414b50c198`.
- Phon base: source `7e81f04b8291b4d2b83f22dc5059f5fe28597d83`, extracted
  `9d703a40ee739cbd40732ed25ee8481a710b96ae`.

Both source histories are parents of their integration commits. Vox's only
content conflict under that common base was the obsolete root `README.md.in`;
upstream's deletion was retained. Phon merged without content conflicts and was
imported under `phon/` with an unsquashed subtree merge. This compatibility
component includes the owned Java runtime and Java schema generator; taking only
`vox/` would have left Java generation and packaging incomplete.

Validation then exposed a real wire-identity change in newer upstream
Phon/Taxon: for example, `DivideByZero` changed from schema ID
`fb754a5995522002` to `bb9bd29e23085ebc` without changing its visible enum
shape. The prepared newer graph is retained on
`migration/upstream-phon-schema-audit` at
`54599c25411952ff9c361083916280599d580cf0`; its Java generation check fails.
The integration branch deliberately retains the legacy Phon Rust and Taxon
implementation from `e76bef6`, including the matching Vox schema-decoder
adapter, conformance corpus, Phon spec and TypeScript implementation, as a
forward compatibility commit. The corpus independently exposed the same
schema-identity mismatch when decoded by the preserved Java runtime. No newer
upstream history is erased.
Adopting the newer identity format and extending the Java runtime is separate
wire-protocol work, not an incidental consequence of moving repositories.

## Package and artifact identities

Maintained Vox and Phon packages use `teamy-vox*` and `teamy-phon*` identities.
The legacy identity helper is retained as the private `teamy-taxon` package.
Phon/Taxon compatibility packages remain at `0.2.0-rc.5`; they do not claim to
implement the newer Phon schema formats merely because upstream history was joined.
The terminal service contract is `teamy-vox-proto`. Dependency aliases and library
names remain `vox`, `vox_types`, `phon`, `facet`, `spec_proto`, and their existing
companions. In particular, `#[vox::service]` still emits paths through `::vox`;
consumers must retain that alias, including when another dependency alias is
re-exported locally as `vox`.

The graph uses existing published Teamy Facet core/reflection packages at
`=0.50.0-rc.7`, format/JSON/value at `=0.50.0-rc.9`, and Figue at `=6.0.0-rc.5`.
Root and nested Phon Cargo.lock files are committed. Cargo metadata and duplicate
trees verify that each graph has one Teamy Facet core identity and no parallel
official Facet/Phon/Vox packages. Unrelated dependencies are not renamed.

There is still a deliberate Taxon boundary: published `teamy-facet-core rc.7`
enables its `taxon` bridge and depends on registry `taxon 0.2.0-rc.5`, while the
preserved Phon implementation uses local `teamy-taxon 0.2.0-rc.5`. Those schema
types have distinct Rust identities. The migration's Phon derivation projects
Facet `Shape` values itself and does not call `facet_core::taxon_bridge`; the
focused compile and Java schema checks exercise that path. Do not pass values
from the Facet Taxon bridge directly into Phon APIs. Unifying this boundary
requires a separately tested Facet/Phon identity decision, not a version-number
substitution or an unreviewed patch of published Facet dependencies.

All local packages have `publish = false`. This migration neither publishes
crates nor assigns new public Maven coordinates. Java package names and the
automatic module name remain `org.facet.*` / `org.facet.vox`. The staged artifact
filename is `java/target/teamy-vox-java-<version>.jar`, avoiding a collision with
the existing SFM artifact. Consumer pins are deliberately unchanged.

Existing consumer audit:

The [resolved identity report](dependency-identities.json) records the exact
versions, sources and dependency paths used for this comparison.

- `teamy-terminal-control` uses upstream Vox `2ca38f41`; terminal streaming uses
  owned Facet commit `f2afdece`. Cargo metadata resolves Vox `0.10.0-rc.0` and
  `0.10.0-rc.5`, and both registry and Git sources of Facet/Phon `rc.5`.
  These are distinct Rust type identities even where versions match. Upgrading
  only one direct dependency would leave incompatible `Facet` traits/shapes.
- SFM branch `capture/sfm435-issue610-cloud` at
  `8e349578d18511d8d5993b66a3ad3c7c957e5db4` keeps its Rust graph on `f2afdece`.
  Its repaired Java artifact is sourced from `e76bef6`, with Temurin
  `21.0.8+9-LTS` and locked BLAKE3 prefix
  `549fcd37165645ee461a70f536ae47e1fadd45e8`. Nothing here changes that lock.
- `teamy-mover` uses the older Facet pin `23ad4dcd0` and has no Vox dependency yet.
  Adding transfer commands or updating its pins is a separate reviewed change.

One Rust graph is necessary for local type compatibility; it does not establish
wire compatibility with existing consumers. Generated-schema, Java runtime,
channel, and later consumer integration checks remain separate evidence.

## Validation and publication boundary

See [validation.json](validation.json) and [credit-behavior.md](credit-behavior.md)
for the exact passed, failed, and unrun checks. Validation uses Rust 1.96.0 and
Temurin 21.0.8+9, targeting Java 17 bytecode. Code executes inside rootless Podman
with no network, read-only source snapshots (without Git metadata), finite CPU,
memory and PID limits, and dedicated Linux/toolchain caches. Dependency acquisition
is a separate step. No home, credentials, or container/agent sockets are mounted.
A writable cache is a trust boundary; these caches must not be shared with
untrusted projects or a different toolchain/target.

The inherited root release workflow triggers only on `main`; migration branches
do not trigger it. Workflows inside `phon/.github` are subtree files, not active
root workflows. Default/consumer branches and repository workflow settings are
unchanged. Reconcile the older default branch's CI/Pages/release configuration
explicitly before any future default-branch migration; no blanket disablement or
automatic publication is part of this checkpoint.
