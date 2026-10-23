#!/usr/bin/env bash
# Run after acquiring locked dependencies and activating Rust 1.96 / pinned JDK.
# Keep logs outside the source tree. Does not publish artifacts or branches.
set -euo pipefail
receipt_dir="${1:?usage: validate-java-cutover.sh /absolute/receipt-directory}"
mkdir -p "$receipt_dir"
receipt_dir="$(cd "$receipt_dir" && pwd)"
repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"
git rev-parse HEAD > "$receipt_dir/commit.txt"
git status --porcelain > "$receipt_dir/worktree.txt"
rustc --version > "$receipt_dir/rust.txt"
java -version 2> "$receipt_dir/java.txt"
printf 'stage\texit_code\n' > "$receipt_dir/results.tsv"
run() {
    local stage="$1"
    shift
    local result=0
    "$@" > "$receipt_dir/$stage.log" 2>&1 || result=$?
    printf '%s\t%s\n' "$stage" "$result" | tee -a "$receipt_dir/results.tsv"
    return "$result"
}
run phon cargo test --offline --locked --manifest-path phon/Cargo.toml -p teamy-phon-conformance -p teamy-phon-schema -p teamy-phon-codegen
run codegen cargo test --offline --locked -p teamy-vox-codegen --lib
run rust cargo test --offline --locked -p teamy-vox-core -p teamy-vox-types -p teamy-vox-phon --lib
run java cargo run --offline --locked -p teamy-vox-xtask -- test-java
run wire cargo test --offline --locked -p spec-tests --test java_wire -- --test-threads=2
run typescript pnpm --dir typescript check
export VOX_JAVA_BUILD_JDK='Eclipse Adoptium|21.0.8+9-LTS'
run package-first cargo run --offline --locked -p teamy-vox-xtask -- package-java
sha256sum java/target/teamy-vox-java-*.jar > "$receipt_dir/jar-first.sha256"
run package-second cargo run --offline --locked -p teamy-vox-xtask -- package-java
sha256sum java/target/teamy-vox-java-*.jar > "$receipt_dir/jar-second.sha256"
run reproducibility cmp "$receipt_dir/jar-first.sha256" "$receipt_dir/jar-second.sha256"
git diff --exit-code > "$receipt_dir/final-diff.txt"
sha256sum "$receipt_dir"/*.log > "$receipt_dir/logs.sha256"
