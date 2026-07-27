# Phon Java 17 baseline

This directory contains the dependency-free Java 17 Phon runtime. Run its
focused gate from the repository root:

```powershell
phon/java/run-tests.ps1
```

The script invokes `javac --release 17 -Xlint:all -Werror` directly and runs the
test entry point with assertions enabled. It does not require Gradle or Maven.

The current accepted baseline covers the frozen Java vertical slice:

- immutable primitive, record, enum, tuple, list, set, map, option, array,
  tensor, and recursive-reference schema values;
- Rust-compatible BLAKE3 schema identity and self-describing schema bytes;
- all checked-in self-describing Value corpus files;
- bounded aligned compact primitive/container I/O and typed adapters; and
- eager compatibility planning for primitive, record field, enum variant,
  tuple, list, set, map, and option evolution.

Channel, external-capability, and dynamic schemas remain deliberately outside
the Java vertical slice. The schema decoder rejects channel and external
fixtures rather than reinterpreting them. Dynamic self-describing values are
covered by `ValueWire`, while schema-driven `Dynamic` compact fields are not
yet an accepted Java adapter surface.

The compatibility implementation covers the accepted checked-in shapes, but a
future conformance expansion should drive the complete
`conformance/compat/vectors.json` set (including recursive evolution and
generic substitution) through a repository-owned cross-language task. That
work is intentionally not claimed by this focused Phon checkpoint.
