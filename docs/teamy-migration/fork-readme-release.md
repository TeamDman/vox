# Fork README update

This follow-up to source `8e671c1d432dbab6965ae3e4f76890b65fd331b8` identifies
TeamDman's independent fork in the Vox and Phon root READMEs and in all 22
selected package READMEs. Ten packages gain a README. Each package explicitly
includes its README, retains both license files, identifies the fork repository,
and links to its own Teamy docs.rs page. Upstream repositories and credits remain
visible; no upstream release or maintenance status is implied by these versions.

The source changes are documentation, package metadata, release versions, and
the corresponding internal dependency constraints and lockfile entries. Runtime,
macro, generator, test, and license source files are unchanged from the qualified
initial release. Third-party dependency versions are unchanged.

The follow-up versions are `teamy-phon-schema 0.2.0-rc.7`, the other five selected
Phon packages at `0.2.0-rc.8`, the fifteen selected Vox packages at
`0.10.0-rc.8`, and `teamy-vox-proto 0.2.3`. They are new immutable crate versions;
the already uploaded initial versions cannot be edited in place. This source
document describes the release candidate, not confirmation of registry acceptance.

The initial [desktop qualification](desktop-registry-release.md) and separate
[cutover validation](cloud-cutover-validation.md) still describe the implementation.
Cargo package verification and registry archive/index checks are required for
each follow-up upload, followed by the clean registry consumer and MFT checks.
Do not substitute local patches for that final consumer qualification.
