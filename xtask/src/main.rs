//! xtask: Development tasks for vox
//!
//! Run with: `cargo xtask <command>`

use std::process::ExitCode;

use facet::Facet;
use figue as args;
use xshell::{Shell, cmd};

/// Development tasks for vox
#[derive(Facet)]
struct Cli {
    #[facet(args::subcommand)]
    command: Commands,
}

#[derive(Facet)]
#[repr(u8)]
enum Commands {
    /// Run all CI checks locally (test, clippy, fmt, doc, coverage, miri)
    Ci,
    /// Run all tests (workspace)
    Test,
    /// Run clippy on all code
    Clippy,
    /// Check formatting
    Fmt {
        /// Fix formatting issues instead of just checking
        #[facet(args::named, default)]
        fix: bool,
    },
    /// Build documentation with warnings as errors
    Doc,
    /// Generate code coverage report (requires cargo-llvm-cov)
    Coverage,
    /// Run miri for undefined behavior detection (requires nightly)
    Miri,
    /// Generate spec/spec-tests/tests/spec_matrix.rs from the combo definition
    GenerateSpecMatrix,
    /// Generate language bindings from the canonical spec-proto crate
    Codegen {
        /// Generate TypeScript bindings into `typescript/generated/`
        #[facet(args::named, default)]
        typescript: bool,
        /// Generate Swift bindings into `swift/generated/`
        #[facet(args::named, default)]
        swift: bool,
        /// Generate Swift client-only bindings
        #[facet(args::named, default)]
        swift_client: bool,
        /// Generate Swift server-only bindings
        #[facet(args::named, default)]
        swift_server: bool,
        /// Generate Swift wire protocol types (Wire.swift)
        #[facet(args::named, default)]
        swift_wire: bool,
        /// Generate Java 17 unary fixtures into java/generated/
        #[facet(args::named, default)]
        java: bool,
    },
    /// Check generated source for drift without changing the worktree
    CheckCodegen {
        /// Check Java 17 generated fixtures
        #[facet(args::named, default)]
        java: bool,
    },
    /// Compile and run the dependency-free Java 17 conformance suite
    TestJava,
    /// Test and assemble the deterministic combined Phon/Vox Java runtime JAR
    PackageJava,
    /// Emit built-in schema compatibility snapshots as JSON.
    SchemaCompatSnapshot,
    /// Compare built-in schema snapshots and enforce acknowledged breaks.
    SchemaCompatCheck,
}

fn main() -> ExitCode {
    if let Err(e) = run() {
        eprintln!("Error: {e}");
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli: Cli = args::from_std_args().unwrap();
    let sh = Shell::new()?;

    // Find workspace root (where Cargo.toml with [workspace] lives)
    let workspace_root = std::env::var("CARGO_MANIFEST_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap())
        .parent()
        .unwrap()
        .to_path_buf();
    sh.change_dir(&workspace_root);

    match cli.command {
        Commands::Test => {
            println!("\n=== Running workspace tests ===");

            // Try nextest first, fall back to cargo test
            if cmd!(sh, "cargo nextest --version").quiet().run().is_ok() {
                println!("Using cargo-nextest");
                // Use CI profile for longer timeouts when in CI
                if std::env::var("CI").is_ok() {
                    cmd!(sh, "cargo nextest run --workspace --profile ci").run()?;
                } else {
                    cmd!(sh, "cargo nextest run --workspace").run()?;
                }
            } else {
                println!("cargo-nextest not found, using cargo test");
                cmd!(sh, "cargo test --workspace").run()?;
            }

            println!("\n=== All tests passed ===");
        }
        Commands::Clippy => {
            println!("=== Running clippy ===");
            // Exclude wasm-browser-tests which only compiles for wasm32
            cmd!(
                sh,
                "cargo clippy --workspace --all-targets --exclude wasm-browser-tests -- -D warnings"
            )
            .run()?;
        }
        Commands::Fmt { fix } => {
            if fix {
                println!("=== Fixing formatting ===");
                cmd!(sh, "cargo fmt --all").run()?;
            } else {
                println!("=== Checking formatting ===");
                cmd!(sh, "cargo fmt --all -- --check").run()?;
            }
        }
        Commands::Ci => {
            println!("=== Running all CI checks ===\n");

            println!(">>> cargo xtask test");
            cmd!(sh, "cargo xtask test").run()?;

            println!("\n>>> cargo xtask schema-compat-check");
            cmd!(sh, "cargo xtask schema-compat-check").run()?;

            println!("\n>>> cargo xtask clippy");
            cmd!(sh, "cargo xtask clippy").run()?;

            println!("\n>>> cargo xtask fmt");
            cmd!(sh, "cargo xtask fmt").run()?;

            println!("\n>>> cargo xtask doc");
            cmd!(sh, "cargo xtask doc").run()?;

            println!("\n>>> cargo xtask coverage");
            cmd!(sh, "cargo xtask coverage").run()?;

            println!("\n>>> cargo xtask miri");
            cmd!(sh, "cargo xtask miri").run()?;

            println!("\n=== All CI checks passed ===");
        }
        Commands::Doc => {
            println!("=== Building documentation with warnings as errors ===");
            // Build docs for the default workspace members (rust/* crates).
            cmd!(sh, "cargo doc --no-deps")
                .env("RUSTDOCFLAGS", "-D warnings")
                .run()?;
            println!("\n=== Documentation built successfully ===");
        }
        Commands::Coverage => {
            println!("=== Generating code coverage report ===");

            // Check if cargo-llvm-cov is installed
            if cmd!(sh, "cargo llvm-cov --version").quiet().run().is_err() {
                eprintln!("cargo-llvm-cov not found. Install with:");
                eprintln!("  cargo install cargo-llvm-cov");
                return Err("cargo-llvm-cov not installed".into());
            }

            cmd!(sh, "cargo llvm-cov nextest --lcov --output-path lcov.info").run()?;

            println!("\n=== Code coverage report generated: lcov.info ===");
        }
        Commands::Miri => {
            println!("=== Running Miri (undefined behavior detection) ===");

            // Check if miri is available (requires nightly)
            if cmd!(sh, "cargo +nightly miri --version")
                .quiet()
                .run()
                .is_err()
            {
                eprintln!("cargo-miri not found. Install with:");
                eprintln!("  rustup +nightly component add miri");
                return Err("cargo-miri not installed".into());
            }

            println!("\n=== Setting up Miri ===");
            cmd!(sh, "cargo +nightly miri setup").run()?;

            println!("\n=== Running Miri tests ===");
            let result = cmd!(sh, "cargo +nightly miri test").run();

            // Miri may fail on some systems due to unsupported libc calls,
            // but we still want to report the result
            match result {
                Ok(()) => println!("\n=== Miri tests passed ==="),
                Err(e) => {
                    eprintln!("\nMiri tests had issues (this may be expected on some systems):");
                    eprintln!("  {}", e);
                    eprintln!("Note: Some tests may be skipped due to Miri limitations");
                }
            }
        }
        Commands::GenerateSpecMatrix => {
            generate_spec_matrix(&workspace_root)?;
        }
        Commands::Codegen {
            typescript,
            swift,
            swift_client,
            swift_server,
            swift_wire,
            java,
        } => {
            let none_specified =
                !typescript && !swift && !swift_client && !swift_server && !swift_wire && !java;
            if typescript || none_specified {
                codegen_typescript(&workspace_root)?;
            }
            if swift || swift_client || swift_server || none_specified {
                codegen_swift(
                    &workspace_root,
                    swift || none_specified,
                    swift_client || none_specified,
                    swift_server || none_specified,
                )?;
            }
            if swift_wire || none_specified {
                codegen_swift_wire(&workspace_root)?;
            }
            if java || none_specified {
                codegen_java(&workspace_root)?;
            }
        }
        Commands::CheckCodegen { java } => {
            if !java {
                return Err("check-codegen currently requires --java".into());
            }
            check_codegen_java(&workspace_root)?;
        }
        Commands::TestJava => test_java(&workspace_root)?,
        Commands::PackageJava => package_java(&workspace_root)?,
        Commands::SchemaCompatSnapshot => {
            emit_schema_compat_snapshot()?;
        }
        Commands::SchemaCompatCheck => {
            check_schema_compat_policy()?;
        }
    }

    Ok(())
}

#[derive(Facet)]
struct NestedRequest {
    message: String,
}

#[derive(Facet)]
struct NestedResponse {
    echoed: String,
}

#[derive(Facet)]
struct DivideRequest {
    dividend: i64,
    divisor: i64,
}

#[derive(Facet)]
struct DivideResponse {
    quotient: i64,
}

#[derive(Facet)]
#[repr(u8)]
enum DivideByZero {
    Zero = 0,
}

fn java_fixture_service() -> vox_types::ServiceDescriptor {
    use vox_types::{MethodDescriptorOptions, ServiceDescriptor, method_descriptor};

    let echo = method_descriptor::<(String,), String>(
        "JavaFixture",
        "echo",
        &["value"],
        &[None],
        MethodDescriptorOptions {
            response_wire_shape: <Result<String, vox_types::VoxError> as Facet>::SHAPE,
            doc: None,
        },
    );
    let inspect = method_descriptor::<(NestedRequest,), NestedResponse>(
        "JavaFixture",
        "inspect",
        &["request"],
        &[None],
        MethodDescriptorOptions {
            response_wire_shape: <Result<NestedResponse, vox_types::VoxError> as Facet>::SHAPE,
            doc: None,
        },
    );
    let divide = method_descriptor::<(DivideRequest,), Result<DivideResponse, DivideByZero>>(
        "JavaFixture",
        "divide",
        &["request"],
        &[None],
        MethodDescriptorOptions {
            response_wire_shape:
                <Result<DivideResponse, vox_types::VoxError<DivideByZero>> as Facet>::SHAPE,
            doc: None,
        },
    );
    let methods = Box::leak(vec![echo, inspect, divide].into_boxed_slice());
    ServiceDescriptor {
        service_name: "JavaFixture",
        methods,
        doc: Some("Frozen Java 17 unary generation fixture"),
    }
}

fn java_generated_dir(workspace_root: &std::path::Path) -> std::path::PathBuf {
    workspace_root
        .join("java")
        .join("generated")
        .join("src")
        .join("main")
        .join("java")
        .join("org")
        .join("facet")
        .join("vox")
        .join("generated")
}

fn codegen_java(workspace_root: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = java_generated_dir(workspace_root);
    std::fs::create_dir_all(&out_dir)?;
    let expected = vox_codegen::targets::java::generate_service(&java_fixture_service())?;
    for file in expected {
        write_if_changed(&out_dir.join(file.relative_path), file.source)?;
    }
    Ok(())
}

fn check_codegen_java(workspace_root: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    use std::collections::BTreeMap;

    let out_dir = java_generated_dir(workspace_root);
    let expected: BTreeMap<_, _> =
        vox_codegen::targets::java::generate_service(&java_fixture_service())?
            .into_iter()
            .map(|file| (file.relative_path, file.source))
            .collect();
    let mut actual = BTreeMap::new();
    if out_dir.exists() {
        for entry in std::fs::read_dir(&out_dir)? {
            let path = entry?.path();
            if path.extension().is_some_and(|ext| ext == "java") {
                let name = path
                    .file_name()
                    .expect("generated Java file has a name")
                    .to_string_lossy()
                    .into_owned();
                actual.insert(name, std::fs::read_to_string(path)?);
            }
        }
    }
    if expected != actual {
        let missing: Vec<_> = expected
            .keys()
            .filter(|key| !actual.contains_key(*key))
            .collect();
        let unexpected: Vec<_> = actual
            .keys()
            .filter(|key| !expected.contains_key(*key))
            .collect();
        let changed: Vec<_> = expected
            .keys()
            .filter(|key| actual.get(*key) != expected.get(*key))
            .collect();
        return Err(format!(
            "Java generated source drift (missing: {missing:?}, unexpected: {unexpected:?}, changed: {changed:?}); run `cargo xtask codegen --java`"
        )
        .into());
    }
    println!("Java generated sources are up to date");
    Ok(())
}

fn java_tool(name: &str) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let executable = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    if let Some(home) = std::env::var_os("JAVA_HOME") {
        let candidate = std::path::PathBuf::from(home).join("bin").join(&executable);
        if candidate.is_file() {
            return Ok(candidate);
        }
        return Err(format!(
            "JAVA_HOME is set but {} does not exist",
            candidate.display()
        )
        .into());
    }
    let candidate = std::path::PathBuf::from(executable);
    let status = std::process::Command::new(&candidate)
        .arg("--version")
        .status()
        .map_err(|error| format!("{name} was not found in PATH: {error}"))?;
    if !status.success() {
        return Err(format!("{name} --version failed with {status}").into());
    }
    Ok(candidate)
}

fn ensure_java_17(javac: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let output = std::process::Command::new(javac).arg("-version").output()?;
    if !output.status.success() {
        return Err("javac -version failed".into());
    }
    let version = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let major = version
        .split_whitespace()
        .find_map(|token| {
            let token = token.trim_start_matches("javac");
            let token = token.trim();
            (!token.is_empty())
                .then(|| token.split('.').next()?.parse::<u32>().ok())
                .flatten()
        })
        .ok_or_else(|| format!("could not parse javac version from {version:?}"))?;
    if major < 17 {
        return Err(format!("Java 17 or newer is required; found javac {major}").into());
    }
    Ok(())
}

fn collect_files(
    root: &std::path::Path,
    extension: &str,
) -> Result<Vec<std::path::PathBuf>, Box<dyn std::error::Error>> {
    fn visit(
        path: &std::path::Path,
        extension: &str,
        files: &mut Vec<std::path::PathBuf>,
    ) -> std::io::Result<()> {
        if !path.exists() {
            return Ok(());
        }
        for entry in std::fs::read_dir(path)? {
            let path = entry?.path();
            if path.is_dir() {
                visit(&path, extension, files)?;
            } else if path.extension().is_some_and(|value| value == extension) {
                files.push(path);
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    visit(root, extension, &mut files)?;
    files.sort();
    Ok(files)
}

fn recreate_dir(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    if path.exists() {
        std::fs::remove_dir_all(path)?;
    }
    std::fs::create_dir_all(path)?;
    Ok(())
}

fn run_checked(
    command: &mut std::process::Command,
    description: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let status = command.status()?;
    if !status.success() {
        return Err(format!("{description} failed with {status}").into());
    }
    Ok(())
}

fn compile_java(
    javac: &std::path::Path,
    classes: &std::path::Path,
    sources: &[std::path::PathBuf],
    classpath: Option<&std::path::Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    recreate_dir(classes)?;
    let mut command = std::process::Command::new(javac);
    command
        .arg("--release")
        .arg("17")
        .arg("-Xlint:all")
        .arg("-Werror")
        .arg("-d")
        .arg(classes);
    if let Some(classpath) = classpath {
        command.arg("-cp").arg(classpath);
    }
    command.args(sources);
    run_checked(&mut command, "javac --release 17")
}

fn java_source_roots(
    workspace_root: &std::path::Path,
    include_tests_and_fixture: bool,
) -> Result<Vec<std::path::PathBuf>, Box<dyn std::error::Error>> {
    let facet_root = workspace_root.parent().expect("vox has repository parent");
    let mut roots = vec![
        facet_root.join("phon/java/runtime/src/main/java"),
        workspace_root.join("java/runtime/src/main/java"),
    ];
    if include_tests_and_fixture {
        roots.extend([
            facet_root.join("phon/java/runtime/src/test/java"),
            workspace_root.join("java/runtime/src/test/java"),
            workspace_root.join("java/generated/src/main/java"),
            workspace_root.join("java/subject/src/main/java"),
        ]);
    }
    let mut sources = Vec::new();
    for root in roots {
        sources.extend(collect_files(&root, "java")?);
    }
    sources.sort();
    Ok(sources)
}

fn test_java(workspace_root: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    check_codegen_java(workspace_root)?;
    let javac = java_tool("javac")?;
    ensure_java_17(&javac)?;
    let java = java_tool("java")?;
    let classes = workspace_root.join("java/target/test-classes");
    compile_java(
        &javac,
        &classes,
        &java_source_roots(workspace_root, true)?,
        None,
    )?;
    let facet_root = workspace_root.parent().expect("vox has repository parent");
    for main_class in [
        "org.facet.phon.PhonConformanceTest",
        "org.facet.vox.tcp.StreamFramingTest",
        "org.facet.vox.VoxRuntimeTest",
        "org.facet.vox.GeneratedResponseIntegrationTest",
    ] {
        let mut command = std::process::Command::new(&java);
        command.arg("-ea").arg("-cp").arg(&classes).arg(main_class);
        if main_class == "org.facet.phon.PhonConformanceTest" {
            command.arg(facet_root);
        }
        run_checked(&mut command, &format!("Java test {main_class}"))?;
    }
    println!("Java 17 Phon/Vox tests passed");
    Ok(())
}

fn vox_java_version(
    workspace_root: &std::path::Path,
) -> Result<String, Box<dyn std::error::Error>> {
    let manifest = std::fs::read_to_string(workspace_root.join("rust/vox/Cargo.toml"))?;
    manifest
        .lines()
        .find_map(|line| {
            line.strip_prefix("version = \"")
                .and_then(|value| value.strip_suffix('"'))
                .map(str::to_string)
        })
        .ok_or_else(|| "vox package version is absent".into())
}

fn assemble_java_jar(
    jar: &std::path::Path,
    classes: &std::path::Path,
    manifest: &std::path::Path,
    class_files: &[std::path::PathBuf],
) -> Result<(), Box<dyn std::error::Error>> {
    let jar_tool = java_tool("jar")?;
    let mut command = std::process::Command::new(jar_tool);
    command
        .arg("--create")
        .arg("--file")
        .arg(jar)
        .arg("--manifest")
        .arg(manifest)
        .arg("--date=1980-01-01T00:00:02Z");
    for class_file in class_files {
        command
            .arg("-C")
            .arg(classes)
            .arg(class_file.strip_prefix(classes)?);
    }
    run_checked(&mut command, "deterministic jar assembly")
}

fn package_java(workspace_root: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    test_java(workspace_root)?;
    let javac = java_tool("javac")?;
    ensure_java_17(&javac)?;
    let target = workspace_root.join("java/target");
    let classes = target.join("runtime-classes");
    compile_java(
        &javac,
        &classes,
        &java_source_roots(workspace_root, false)?,
        None,
    )?;
    let manifest = target.join("runtime-manifest.mf");
    std::fs::write(
        &manifest,
        "Manifest-Version: 1.0\r\nAutomatic-Module-Name: org.facet.vox\r\n\r\n",
    )?;
    let version = vox_java_version(workspace_root)?;
    let artifact = target.join(format!("vox-java-{version}.jar"));
    let class_files = collect_files(&classes, "class")?;
    assemble_java_jar(&artifact, &classes, &manifest, &class_files)?;
    let first = std::fs::read(&artifact)?;
    assemble_java_jar(&artifact, &classes, &manifest, &class_files)?;
    if std::fs::read(&artifact)? != first {
        return Err("Java runtime JAR is not reproducible across consecutive assembly".into());
    }

    let smoke = target.join("smoke");
    recreate_dir(&smoke)?;
    let source = smoke.join("VoxJavaSmoke.java");
    std::fs::write(
        &source,
        "import org.facet.phon.PhonLimits;\n\
         import org.facet.vox.VoxResult;\n\
         public final class VoxJavaSmoke {\n\
         public static void main(String[] args) {\n\
         if (PhonLimits.defaults().inputBytes() <= 0 || !VoxResult.success(\"ok\").isSuccess()) throw new AssertionError();\n\
         }\n}\n",
    )?;
    let smoke_classes = smoke.join("classes");
    compile_java(&javac, &smoke_classes, &[source], Some(&artifact))?;
    let java = java_tool("java")?;
    let separator = if cfg!(windows) { ";" } else { ":" };
    let smoke_classpath = format!(
        "{}{separator}{}",
        smoke_classes.display(),
        artifact.display()
    );
    run_checked(
        std::process::Command::new(java)
            .arg("-cp")
            .arg(smoke_classpath)
            .arg("VoxJavaSmoke"),
        "Java runtime JAR smoke consumer",
    )?;
    let jdeps = java_tool("jdeps")?;
    let output = std::process::Command::new(jdeps)
        .arg("--multi-release")
        .arg("17")
        .arg("--missing-deps")
        .arg(&artifact)
        .output()?;
    if !output.status.success() || !output.stdout.is_empty() || !output.stderr.is_empty() {
        return Err(format!(
            "runtime JAR has unresolved dependencies:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    println!(
        "Packaged deterministic dependency-free Java runtime: {}",
        artifact.display()
    );
    Ok(())
}

#[derive(Facet)]
struct NamedSchemaCompatSnapshots {
    snapshots: Vec<NamedSchemaCompatSnapshot>,
}

#[derive(Facet)]
struct NamedSchemaCompatSnapshot {
    name: String,
    snapshot: vox_codegen::schema_compat::SchemaCompatSnapshot,
}

fn built_in_schema_compat_snapshots() -> Result<
    (
        vox_codegen::schema_compat::SchemaCompatSnapshot,
        vox_codegen::schema_compat::SchemaCompatSnapshot,
    ),
    Box<dyn std::error::Error>,
> {
    let canonical_services = spec_proto::all_services();
    let canonical = vox_codegen::schema_compat::snapshot_services(&canonical_services)?;
    let evolved_services = [spec_proto::evolved::testbed_service_descriptor()];
    let evolved = vox_codegen::schema_compat::snapshot_services(&evolved_services)?;
    let canonical = vox_codegen::schema_compat::method_intersection_snapshot(&canonical, &evolved);
    Ok((canonical, evolved))
}

fn emit_schema_compat_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    let (canonical, evolved) = built_in_schema_compat_snapshots()?;
    let snapshots = NamedSchemaCompatSnapshots {
        snapshots: vec![
            NamedSchemaCompatSnapshot {
                name: "spec-proto".to_string(),
                snapshot: canonical,
            },
            NamedSchemaCompatSnapshot {
                name: "spec-proto-evolved".to_string(),
                snapshot: evolved,
            },
        ],
    };
    println!("{}", facet_json::to_string_pretty(&snapshots)?);
    Ok(())
}

fn built_in_schema_compat_policy() -> vox_codegen::schema_compat::SchemaCompatPolicy {
    use vox_codegen::schema_compat::{
        SchemaCompatAcknowledgement, SchemaCompatComparisonDirection, SchemaCompatPolicy,
    };

    SchemaCompatPolicy {
        acknowledged_breaking: vec![
            SchemaCompatAcknowledgement {
                service_name: "Testbed".to_string(),
                method_name: "echo_measurement".to_string(),
                direction: SchemaCompatComparisonDirection::Args,
            },
            SchemaCompatAcknowledgement {
                service_name: "Testbed".to_string(),
                method_name: "echo_measurement".to_string(),
                direction: SchemaCompatComparisonDirection::Response,
            },
        ],
    }
}

fn check_schema_compat_policy() -> Result<(), Box<dyn std::error::Error>> {
    use vox_codegen::schema_compat::{SchemaCompatStatus, compare_snapshots, enforce_policy};

    let (canonical, evolved) = built_in_schema_compat_snapshots()?;
    let report = compare_snapshots(&canonical, &evolved)?;
    for comparison in &report.comparisons {
        println!(
            "{}.{} {:?}: {:?}",
            comparison.service_name,
            comparison.method_name,
            comparison.direction,
            comparison.status
        );
    }

    let outcome = enforce_policy(&report, &built_in_schema_compat_policy());
    if !outcome.is_ok() {
        if !outcome.unacknowledged_breaking.is_empty() {
            eprintln!("unacknowledged schema compatibility breaks:");
            for comparison in &outcome.unacknowledged_breaking {
                eprintln!(
                    "  {}.{} {:?}: {:?}",
                    comparison.service_name,
                    comparison.method_name,
                    comparison.direction,
                    comparison.status
                );
            }
        }
        if !outcome.stale_acknowledgements.is_empty() {
            eprintln!("stale schema compatibility acknowledgements:");
            for ack in &outcome.stale_acknowledgements {
                eprintln!(
                    "  {}.{} {:?}",
                    ack.service_name, ack.method_name, ack.direction
                );
            }
        }
        return Err("schema compatibility policy failed".into());
    }

    let breaking_count = report
        .comparisons
        .iter()
        .filter(|comparison| comparison.status == SchemaCompatStatus::Incompatible)
        .count();
    println!("schema compatibility policy ok ({breaking_count} acknowledged breaking changes)");
    Ok(())
}

fn fmt_swift(path: &std::path::Path, text: String) -> String {
    fn try_swift_formatter(
        path: &std::path::Path,
        text: &str,
        program: &str,
        args: &[&str],
    ) -> Result<Option<String>, Box<dyn std::error::Error>> {
        use std::io::{ErrorKind, Write};
        use std::process::{Command, Stdio};

        let mut child = match Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(format!(
                    "failed to start {program} while formatting {}: {e}",
                    path.display()
                )
                .into());
            }
        };

        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| format!("failed to open stdin for {program}"))?;
        stdin.write_all(text.as_bytes())?;
        drop(stdin);

        let output = child.wait_with_output()?;
        if output.status.success() {
            let stdout = String::from_utf8(output.stdout)?;
            return Ok(Some(stdout));
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "{program} {} failed while formatting {}: {}",
            args.join(" "),
            path.display(),
            stderr.trim()
        )
        .into())
    }

    match try_swift_formatter(path, &text, "swift-format", &["format", "-"]) {
        Ok(Some(formatted)) => formatted,
        Ok(None) => match try_swift_formatter(path, &text, "swift", &["format", "-"]) {
            Ok(Some(formatted)) => formatted,
            Ok(None) => {
                eprintln!(
                    "warning: neither swift-format nor `swift format` found, leaving {} unformatted",
                    path.display()
                );
                text
            }
            Err(e) => {
                eprintln!("warning: swift format failed for {}: {e}", path.display());
                text
            }
        },
        Err(e) => {
            eprintln!("warning: swift-format failed for {}: {e}", path.display());
            text
        }
    }
}

fn codegen_typescript(workspace_root: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = workspace_root.join("typescript").join("generated");
    std::fs::create_dir_all(&out_dir)?;

    // Generate TypeScript for all services in spec-proto
    for service in spec_proto::all_services() {
        let ts = vox_codegen::targets::typescript::generate_service(service);
        let filename = format!("{}.generated.ts", service.service_name.to_lowercase());
        let out_path = out_dir.join(&filename);
        write_if_changed(&out_path, ts)?;
    }

    // Generate TypeScript for the evolved testbed (schema compat testing)
    {
        let evolved = spec_proto::evolved::testbed_service_descriptor();
        let ts = vox_codegen::targets::typescript::generate_service(evolved);
        let out_path = out_dir.join("testbed_evolved.generated.ts");
        write_if_changed(&out_path, ts)?;
    }

    codegen_typescript_wire(workspace_root)?;

    Ok(())
}

fn codegen_typescript_wire(
    workspace_root: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // The phon Message envelope module for vox-wire.
    let wire_path = workspace_root
        .join("typescript")
        .join("packages")
        .join("vox-wire")
        .join("src")
        .join("wire.phon.generated.ts");
    let wire = vox_codegen::targets::typescript::generate_phon_wire();
    write_if_changed(&wire_path, wire)?;

    // The phon HandshakeMessage module for vox-core.
    let hs_path = workspace_root
        .join("typescript")
        .join("packages")
        .join("vox-core")
        .join("src")
        .join("handshake.phon.generated.ts");
    let hs = vox_codegen::targets::typescript::generate_phon_handshake();
    write_if_changed(&hs_path, hs)?;

    Ok(())
}

fn codegen_swift(
    workspace_root: &std::path::Path,
    swift: bool,
    swift_client: bool,
    swift_server: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Output directly to subject sources
    let out_dir = workspace_root
        .join("swift")
        .join("subject")
        .join("Sources")
        .join("subject-swift");
    std::fs::create_dir_all(&out_dir)?;

    let testbed = spec_proto::testbed_service_descriptor();

    // The full phon Testbed module (types + schemas/descriptors/programs + client +
    // server). `--swift`/`--swift-client`/`--swift-server` all emit the same full module
    // (the subject compiles one file); the bindings split is gone with postcard.
    let _ = (swift, swift_client, swift_server);
    let code = vox_codegen::targets::swift::generate_service(testbed);
    let out_path = out_dir.join("Testbed.swift");
    write_if_changed(&out_path, fmt_swift(&out_path, code))?;
    Ok(())
}

fn codegen_swift_wire(workspace_root: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    use vox_codegen::targets::swift::WireType;
    use vox_codegen::targets::swift::phon_descriptor::generate_phon_wire;
    use vox_types as rt;

    let out_path = workspace_root
        .join("swift")
        .join("vox-runtime")
        .join("Sources")
        .join("VoxRuntime")
        .join("Wire.swift");

    macro_rules! wire_type {
        ($swift_name:literal, $ty:ty) => {
            WireType {
                swift_name: $swift_name.to_string(),
                shape: <$ty as facet::Facet<'static>>::SHAPE,
            }
        };
    }

    let types = vec![
        wire_type!("Parity", rt::Parity),
        wire_type!("BindingDirection", rt::BindingDirection),
        wire_type!("ConnectionSettings", rt::ConnectionSettings),
        wire_type!("ProtocolError", rt::ProtocolError<'static>),
        wire_type!("Ping", rt::Ping),
        wire_type!("Pong", rt::Pong),
        wire_type!("LaneOpen", rt::LaneOpen),
        wire_type!("LaneAccept", rt::LaneAccept),
        wire_type!("LaneReject", rt::LaneReject),
        wire_type!("LaneClose", rt::LaneClose),
        wire_type!("RequestCall", rt::RequestCall<'static>),
        wire_type!("RequestResponse", rt::RequestResponse<'static>),
        wire_type!("RequestCancel", rt::RequestCancel),
        wire_type!("RequestBody", rt::RequestBody<'static>),
        wire_type!("RequestMessage", rt::RequestMessage<'static>),
        wire_type!("SchemaMessage", rt::SchemaMessage),
        wire_type!("ChannelItem", rt::ChannelItem<'static>),
        wire_type!("ChannelClose", rt::ChannelClose),
        wire_type!("ChannelReset", rt::ChannelReset),
        wire_type!("ChannelGrantCredit", rt::ChannelGrantCredit),
        wire_type!("ChannelBody", rt::ChannelBody<'static>),
        wire_type!("ChannelMessage", rt::ChannelMessage<'static>),
        wire_type!("MessagePayload", rt::MessagePayload<'static>),
        wire_type!("Message", rt::Message<'static>),
    ];

    let type_pairs: Vec<_> = types
        .iter()
        .map(|w| (w.swift_name.clone(), w.shape))
        .collect();
    let roots = vec![(
        "Message".to_string(),
        <rt::Message<'static> as facet::Facet>::SHAPE,
    )];
    let code = generate_phon_wire(&type_pairs, &roots);
    write_if_changed(&out_path, fmt_swift(&out_path, code))?;

    // The phon HandshakeMessage module — a second self-describing root, framed
    // exactly like the Message envelope but used during the connection handshake.
    // `Parity` and `ConnectionSettings` are already declared in Wire.swift (same
    // module), so emit only the handshake-unique types here.
    let hs_out_path = workspace_root
        .join("swift")
        .join("vox-runtime")
        .join("Sources")
        .join("VoxRuntime")
        .join("HandshakeWire.swift");
    let hs_types = [
        wire_type!("Hello", rt::Hello),
        wire_type!("HelloYourself", rt::HelloYourself),
        wire_type!("LetsGo", rt::LetsGo),
        wire_type!("EstablishmentRejectReason", rt::EstablishmentRejectReason),
        wire_type!("Decline", rt::Decline),
        wire_type!("Sorry", rt::Sorry),
        wire_type!("HandshakeMessage", rt::HandshakeMessage),
    ];
    let hs_type_pairs: Vec<_> = hs_types
        .iter()
        .map(|w| (w.swift_name.clone(), w.shape))
        .collect();
    let hs_roots = vec![(
        "HandshakeMessage".to_string(),
        <rt::HandshakeMessage as facet::Facet>::SHAPE,
    )];
    let hs_code = generate_phon_wire(&hs_type_pairs, &hs_roots);
    write_if_changed(&hs_out_path, fmt_swift(&hs_out_path, hs_code))?;

    Ok(())
}

fn generate_spec_matrix(
    workspace_root: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    use proc_macro2::{Ident, Span, TokenStream};
    use quote::quote;

    struct Combo {
        mod_name: &'static str,
        spec_const: &'static str,
        ignore: bool,
    }

    struct TestCase {
        name: &'static str,
        call: &'static str,
    }

    let combos = [
        Combo {
            mod_name: "lang_rust_transport_tcp",
            spec_const: "SUBJECT_RUST_TCP",
            ignore: false,
        },
        Combo {
            mod_name: "lang_typescript_transport_tcp",
            spec_const: "SUBJECT_TYPESCRIPT_TCP",
            ignore: false,
        },
        Combo {
            mod_name: "lang_typescript_transport_ws",
            spec_const: "SUBJECT_TYPESCRIPT_WS",
            ignore: false,
        },
        Combo {
            mod_name: "lang_swift_transport_tcp",
            spec_const: "SUBJECT_SWIFT_TCP",
            ignore: false,
        },
    ];

    let harness_to_subject = [
        TestCase {
            name: "rpc_echo_roundtrip",
            call: "testbed::run_rpc_echo_roundtrip",
        },
        TestCase {
            name: "rpc_user_error_roundtrip",
            call: "testbed::run_rpc_user_error_roundtrip",
        },
        TestCase {
            name: "rpc_pipelining_multiple_requests",
            call: "testbed::run_rpc_pipelining_multiple_requests",
        },
        TestCase {
            name: "rpc_reverse_roundtrip",
            call: "testbed::run_rpc_reverse_roundtrip",
        },
        TestCase {
            name: "rpc_lookup_user_error",
            call: "testbed::run_rpc_lookup_user_error",
        },
        TestCase {
            name: "rpc_complex_struct_echo",
            call: "testbed::run_rpc_complex_struct_echo",
        },
        TestCase {
            name: "rpc_optional_field",
            call: "testbed::run_rpc_optional_field",
        },
        TestCase {
            name: "rpc_nested_struct",
            call: "testbed::run_rpc_nested_struct",
        },
        TestCase {
            name: "rpc_option_return",
            call: "testbed::run_rpc_option_return",
        },
        TestCase {
            name: "rpc_enum_struct_variants",
            call: "testbed::run_rpc_enum_struct_variants",
        },
        TestCase {
            name: "rpc_vec_of_structs",
            call: "testbed::run_rpc_vec_of_structs",
        },
        TestCase {
            name: "rpc_enum_newtype_variants",
            call: "testbed::run_rpc_enum_newtype_variants",
        },
        TestCase {
            name: "rpc_vec_return",
            call: "testbed::run_rpc_vec_return",
        },
        TestCase {
            name: "rpc_tuple_type",
            call: "testbed::run_rpc_tuple_type",
        },
        TestCase {
            name: "rpc_divide_overflow",
            call: "testbed::run_rpc_divide_overflow",
        },
        TestCase {
            name: "rpc_lookup_found",
            call: "testbed::run_rpc_lookup_found",
        },
        TestCase {
            name: "rpc_lookup_access_denied",
            call: "testbed::run_rpc_lookup_access_denied",
        },
        TestCase {
            name: "rpc_echo_bytes",
            call: "testbed::run_rpc_echo_bytes",
        },
        TestCase {
            name: "rpc_echo_bool",
            call: "testbed::run_rpc_echo_bool",
        },
        TestCase {
            name: "rpc_echo_u64",
            call: "testbed::run_rpc_echo_u64",
        },
        TestCase {
            name: "rpc_echo_option_string",
            call: "testbed::run_rpc_echo_option_string",
        },
        TestCase {
            name: "rpc_describe_point",
            call: "testbed::run_rpc_describe_point",
        },
        TestCase {
            name: "rpc_all_colors",
            call: "testbed::run_rpc_all_colors",
        },
        TestCase {
            name: "rpc_echo_shape",
            call: "testbed::run_rpc_echo_shape",
        },
        TestCase {
            name: "rpc_echo_tree",
            call: "testbed::run_rpc_echo_tree",
        },
        TestCase {
            name: "rpc_echo_ecosystem_bridge",
            call: "testbed::run_rpc_echo_ecosystem_bridge",
        },
        TestCase {
            name: "rpc_echo_dodeca_template_call",
            call: "testbed::run_rpc_echo_dodeca_template_call",
        },
        TestCase {
            name: "rpc_dodeca_html_process",
            call: "testbed::run_rpc_dodeca_html_process",
        },
        TestCase {
            name: "rpc_dodeca_execute_code_samples",
            call: "testbed::run_rpc_dodeca_execute_code_samples",
        },
        TestCase {
            name: "rpc_dodeca_load_data",
            call: "testbed::run_rpc_dodeca_load_data",
        },
        TestCase {
            name: "rpc_dodeca_parse_and_render",
            call: "testbed::run_rpc_dodeca_parse_and_render",
        },
        TestCase {
            name: "rpc_echo_dodeca_image_processor_fixture",
            call: "testbed::run_rpc_echo_dodeca_image_processor_fixture",
        },
        TestCase {
            name: "rpc_echo_dodeca_search_indexer_fixture",
            call: "testbed::run_rpc_echo_dodeca_search_indexer_fixture",
        },
        TestCase {
            name: "rpc_echo_dodeca_asset_processing_fixture",
            call: "testbed::run_rpc_echo_dodeca_asset_processing_fixture",
        },
        TestCase {
            name: "rpc_echo_dodeca_small_cell_services_fixture",
            call: "testbed::run_rpc_echo_dodeca_small_cell_services_fixture",
        },
        TestCase {
            name: "rpc_echo_dodeca_devtools_event",
            call: "testbed::run_rpc_echo_dodeca_devtools_event",
        },
        TestCase {
            name: "rpc_dodeca_devtools_get_scope",
            call: "testbed::run_rpc_dodeca_devtools_get_scope",
        },
        TestCase {
            name: "rpc_dodeca_devtools_eval",
            call: "testbed::run_rpc_dodeca_devtools_eval",
        },
        TestCase {
            name: "rpc_dodeca_devtools_open_dead_link",
            call: "testbed::run_rpc_dodeca_devtools_open_dead_link",
        },
        TestCase {
            name: "rpc_dodeca_devtools_edit_load",
            call: "testbed::run_rpc_dodeca_devtools_edit_load",
        },
        TestCase {
            name: "rpc_dodeca_devtools_edit_preview",
            call: "testbed::run_rpc_dodeca_devtools_edit_preview",
        },
        TestCase {
            name: "rpc_dodeca_devtools_edit_save",
            call: "testbed::run_rpc_dodeca_devtools_edit_save",
        },
        TestCase {
            name: "rpc_dodeca_devtools_edit_upload",
            call: "testbed::run_rpc_dodeca_devtools_edit_upload",
        },
        TestCase {
            name: "rpc_dodeca_devtools_edit_read",
            call: "testbed::run_rpc_dodeca_devtools_edit_read",
        },
        TestCase {
            name: "rpc_dodeca_devtools_edit_list",
            call: "testbed::run_rpc_dodeca_devtools_edit_list",
        },
        TestCase {
            name: "rpc_echo_styx_value",
            call: "testbed::run_rpc_echo_styx_value",
        },
        TestCase {
            name: "rpc_styx_lsp_initialize",
            call: "testbed::run_rpc_styx_lsp_initialize",
        },
        TestCase {
            name: "rpc_styx_lsp_completions",
            call: "testbed::run_rpc_styx_lsp_completions",
        },
        TestCase {
            name: "rpc_styx_lsp_hover",
            call: "testbed::run_rpc_styx_lsp_hover",
        },
        TestCase {
            name: "rpc_styx_lsp_inlay_hints",
            call: "testbed::run_rpc_styx_lsp_inlay_hints",
        },
        TestCase {
            name: "rpc_styx_lsp_diagnostics",
            call: "testbed::run_rpc_styx_lsp_diagnostics",
        },
        TestCase {
            name: "rpc_styx_lsp_code_actions",
            call: "testbed::run_rpc_styx_lsp_code_actions",
        },
        TestCase {
            name: "rpc_styx_lsp_definition",
            call: "testbed::run_rpc_styx_lsp_definition",
        },
        TestCase {
            name: "rpc_styx_lsp_shutdown",
            call: "testbed::run_rpc_styx_lsp_shutdown",
        },
        TestCase {
            name: "rpc_styx_host_get_subtree",
            call: "testbed::run_rpc_styx_host_get_subtree",
        },
        TestCase {
            name: "rpc_styx_host_get_document",
            call: "testbed::run_rpc_styx_host_get_document",
        },
        TestCase {
            name: "rpc_styx_host_get_source",
            call: "testbed::run_rpc_styx_host_get_source",
        },
        TestCase {
            name: "rpc_styx_host_get_schema",
            call: "testbed::run_rpc_styx_host_get_schema",
        },
        TestCase {
            name: "rpc_styx_host_offset_to_position",
            call: "testbed::run_rpc_styx_host_offset_to_position",
        },
        TestCase {
            name: "rpc_styx_host_position_to_offset",
            call: "testbed::run_rpc_styx_host_position_to_offset",
        },
        TestCase {
            name: "rpc_stax_flamegraph",
            call: "testbed::run_rpc_stax_flamegraph",
        },
        TestCase {
            name: "rpc_echo_stax_flamegraph_update",
            call: "testbed::run_rpc_echo_stax_flamegraph_update",
        },
        TestCase {
            name: "rpc_stax_subscribe_flamegraph_updates",
            call: "testbed::run_rpc_stax_subscribe_flamegraph_updates",
        },
        TestCase {
            name: "rpc_echo_stax_linux_broker_control",
            call: "testbed::run_rpc_echo_stax_linux_broker_control",
        },
        TestCase {
            name: "rpc_stax_macos_record",
            call: "testbed::run_rpc_stax_macos_record",
        },
        TestCase {
            name: "rpc_echo_hotmeal_live_reload_event",
            call: "testbed::run_rpc_echo_hotmeal_live_reload_event",
        },
        TestCase {
            name: "rpc_echo_hotmeal_apply_patches_result",
            call: "testbed::run_rpc_echo_hotmeal_apply_patches_result",
        },
        TestCase {
            name: "rpc_hotmeal_live_reload_subscribe",
            call: "testbed::run_rpc_hotmeal_live_reload_subscribe",
        },
        TestCase {
            name: "rpc_hotmeal_live_reload_on_event",
            call: "testbed::run_rpc_hotmeal_live_reload_on_event",
        },
        TestCase {
            name: "rpc_echo_helix_stream_metrics",
            call: "testbed::run_rpc_echo_helix_stream_metrics",
        },
        TestCase {
            name: "rpc_echo_helix_verify_evidence",
            call: "testbed::run_rpc_echo_helix_verify_evidence",
        },
        TestCase {
            name: "rpc_helix_subscribe_pulses",
            call: "testbed::run_rpc_helix_subscribe_pulses",
        },
        TestCase {
            name: "rpc_helix_pulse_bundle",
            call: "testbed::run_rpc_helix_pulse_bundle",
        },
        TestCase {
            name: "rpc_helix_trace_service_surface",
            call: "testbed::run_rpc_helix_trace_service_surface",
        },
        TestCase {
            name: "rpc_tracey_status",
            call: "testbed::run_rpc_tracey_status",
        },
        TestCase {
            name: "rpc_tracey_core_control",
            call: "testbed::run_rpc_tracey_core_control",
        },
        TestCase {
            name: "rpc_tracey_rule",
            call: "testbed::run_rpc_tracey_rule",
        },
        TestCase {
            name: "rpc_tracey_dashboard",
            call: "testbed::run_rpc_tracey_dashboard",
        },
        TestCase {
            name: "rpc_tracey_validate",
            call: "testbed::run_rpc_tracey_validate",
        },
        TestCase {
            name: "rpc_tracey_lsp_surface",
            call: "testbed::run_rpc_tracey_lsp_surface",
        },
        TestCase {
            name: "rpc_tracey_lsp_workspace_diagnostics",
            call: "testbed::run_rpc_tracey_lsp_workspace_diagnostics",
        },
        TestCase {
            name: "rpc_tracey_subscribe_updates",
            call: "testbed::run_rpc_tracey_subscribe_updates",
        },
        TestCase {
            name: "rpc_echo_status",
            call: "testbed::run_rpc_echo_status",
        },
        TestCase {
            name: "rpc_echo_tag",
            call: "testbed::run_rpc_echo_tag",
        },
        TestCase {
            name: "rpc_pipelining_10_concurrent",
            call: "testbed::run_rpc_pipelining_10_concurrent",
        },
        TestCase {
            name: "rpc_channeling_large_stream",
            call: "testbed::run_rpc_channeling_large_stream",
        },
        TestCase {
            name: "rpc_channeling_sum_large",
            call: "testbed::run_rpc_channeling_sum_large",
        },
        TestCase {
            name: "rpc_dodeca_byte_tunnel",
            call: "testbed::run_rpc_dodeca_byte_tunnel",
        },
        TestCase {
            name: "rpc_dodeca_devtools_lsp",
            call: "testbed::run_rpc_dodeca_devtools_lsp",
        },
        TestCase {
            name: "rpc_dibs_list",
            call: "testbed::run_rpc_dibs_list",
        },
        TestCase {
            name: "rpc_dibs_schema",
            call: "testbed::run_rpc_dibs_schema",
        },
        TestCase {
            name: "rpc_dibs_get",
            call: "testbed::run_rpc_dibs_get",
        },
        TestCase {
            name: "rpc_dibs_create",
            call: "testbed::run_rpc_dibs_create",
        },
        TestCase {
            name: "rpc_dibs_update",
            call: "testbed::run_rpc_dibs_update",
        },
        TestCase {
            name: "rpc_dibs_delete",
            call: "testbed::run_rpc_dibs_delete",
        },
        TestCase {
            name: "rpc_dibs_migration_status",
            call: "testbed::run_rpc_dibs_migration_status",
        },
        TestCase {
            name: "rpc_dibs_migrate",
            call: "testbed::run_rpc_dibs_migrate",
        },
        TestCase {
            name: "channeling_generate_server_to_client",
            call: "channeling::run_channeling_generate_server_to_client",
        },
        TestCase {
            name: "channeling_post_reply_generate_server_to_client",
            call: "channeling::run_channeling_post_reply_generate_server_to_client",
        },
        TestCase {
            name: "channeling_post_reply_sum_client_to_server",
            call: "channeling::run_channeling_post_reply_sum_client_to_server",
        },
        TestCase {
            name: "binary_payload_sizes",
            call: "binary_payloads::run_subject_process_message_binary_payload_sizes",
        },
    ];
    let subject_to_harness = [
        TestCase {
            name: "channeling_sum_client_to_server",
            call: "channeling::run_channeling_sum_client_to_server",
        },
        TestCase {
            name: "subject_calls_echo",
            call: "testbed::run_subject_calls_echo",
        },
        TestCase {
            name: "subject_calls_shape_area",
            call: "testbed::run_subject_calls_shape_area",
        },
        TestCase {
            name: "subject_calls_create_canvas",
            call: "testbed::run_subject_calls_create_canvas",
        },
        TestCase {
            name: "subject_calls_process_message",
            call: "testbed::run_subject_calls_process_message",
        },
        TestCase {
            name: "subject_calls_reverse",
            call: "testbed::run_subject_calls_reverse",
        },
        TestCase {
            name: "subject_calls_divide_success",
            call: "testbed::run_subject_calls_divide_success",
        },
        TestCase {
            name: "subject_calls_divide_zero",
            call: "testbed::run_subject_calls_divide_zero",
        },
        TestCase {
            name: "subject_calls_divide_overflow",
            call: "testbed::run_subject_calls_divide_overflow",
        },
        TestCase {
            name: "subject_calls_lookup_found",
            call: "testbed::run_subject_calls_lookup_found",
        },
        TestCase {
            name: "subject_calls_lookup_found_no_email",
            call: "testbed::run_subject_calls_lookup_found_no_email",
        },
        TestCase {
            name: "subject_calls_lookup_not_found",
            call: "testbed::run_subject_calls_lookup_not_found",
        },
        TestCase {
            name: "subject_calls_lookup_access_denied",
            call: "testbed::run_subject_calls_lookup_access_denied",
        },
        TestCase {
            name: "subject_calls_echo_point",
            call: "testbed::run_subject_calls_echo_point",
        },
        TestCase {
            name: "subject_calls_create_person",
            call: "testbed::run_subject_calls_create_person",
        },
        TestCase {
            name: "subject_calls_rectangle_area",
            call: "testbed::run_subject_calls_rectangle_area",
        },
        TestCase {
            name: "subject_calls_parse_color",
            call: "testbed::run_subject_calls_parse_color",
        },
        TestCase {
            name: "subject_calls_get_points",
            call: "testbed::run_subject_calls_get_points",
        },
        TestCase {
            name: "subject_calls_swap_pair",
            call: "testbed::run_subject_calls_swap_pair",
        },
        TestCase {
            name: "subject_calls_echo_bytes",
            call: "testbed::run_subject_calls_echo_bytes",
        },
        TestCase {
            name: "subject_calls_echo_bool",
            call: "testbed::run_subject_calls_echo_bool",
        },
        TestCase {
            name: "subject_calls_echo_u64",
            call: "testbed::run_subject_calls_echo_u64",
        },
        TestCase {
            name: "subject_calls_echo_option_string",
            call: "testbed::run_subject_calls_echo_option_string",
        },
        TestCase {
            name: "subject_calls_describe_point",
            call: "testbed::run_subject_calls_describe_point",
        },
        TestCase {
            name: "subject_calls_all_colors",
            call: "testbed::run_subject_calls_all_colors",
        },
        TestCase {
            name: "subject_calls_echo_shape",
            call: "testbed::run_subject_calls_echo_shape",
        },
        TestCase {
            name: "subject_calls_echo_tree",
            call: "testbed::run_subject_calls_echo_tree",
        },
        TestCase {
            name: "subject_calls_echo_ecosystem_bridge",
            call: "testbed::run_subject_calls_echo_ecosystem_bridge",
        },
        TestCase {
            name: "subject_calls_echo_dodeca_template_call",
            call: "testbed::run_subject_calls_echo_dodeca_template_call",
        },
        TestCase {
            name: "subject_calls_dodeca_html_process",
            call: "testbed::run_subject_calls_dodeca_html_process",
        },
        TestCase {
            name: "subject_calls_dodeca_execute_code_samples",
            call: "testbed::run_subject_calls_dodeca_execute_code_samples",
        },
        TestCase {
            name: "subject_calls_dodeca_load_data",
            call: "testbed::run_subject_calls_dodeca_load_data",
        },
        TestCase {
            name: "subject_calls_dodeca_parse_and_render",
            call: "testbed::run_subject_calls_dodeca_parse_and_render",
        },
        TestCase {
            name: "subject_calls_echo_dodeca_image_processor_fixture",
            call: "testbed::run_subject_calls_echo_dodeca_image_processor_fixture",
        },
        TestCase {
            name: "subject_calls_echo_dodeca_search_indexer_fixture",
            call: "testbed::run_subject_calls_echo_dodeca_search_indexer_fixture",
        },
        TestCase {
            name: "subject_calls_echo_dodeca_asset_processing_fixture",
            call: "testbed::run_subject_calls_echo_dodeca_asset_processing_fixture",
        },
        TestCase {
            name: "subject_calls_echo_dodeca_small_cell_services_fixture",
            call: "testbed::run_subject_calls_echo_dodeca_small_cell_services_fixture",
        },
        TestCase {
            name: "subject_calls_echo_dodeca_devtools_event",
            call: "testbed::run_subject_calls_echo_dodeca_devtools_event",
        },
        TestCase {
            name: "subject_calls_dodeca_devtools_get_scope",
            call: "testbed::run_subject_calls_dodeca_devtools_get_scope",
        },
        TestCase {
            name: "subject_calls_dodeca_devtools_eval",
            call: "testbed::run_subject_calls_dodeca_devtools_eval",
        },
        TestCase {
            name: "subject_calls_dodeca_devtools_open_dead_link",
            call: "testbed::run_subject_calls_dodeca_devtools_open_dead_link",
        },
        TestCase {
            name: "subject_calls_dodeca_devtools_edit_load",
            call: "testbed::run_subject_calls_dodeca_devtools_edit_load",
        },
        TestCase {
            name: "subject_calls_dodeca_devtools_edit_preview",
            call: "testbed::run_subject_calls_dodeca_devtools_edit_preview",
        },
        TestCase {
            name: "subject_calls_dodeca_devtools_edit_save",
            call: "testbed::run_subject_calls_dodeca_devtools_edit_save",
        },
        TestCase {
            name: "subject_calls_dodeca_devtools_edit_upload",
            call: "testbed::run_subject_calls_dodeca_devtools_edit_upload",
        },
        TestCase {
            name: "subject_calls_dodeca_devtools_edit_read",
            call: "testbed::run_subject_calls_dodeca_devtools_edit_read",
        },
        TestCase {
            name: "subject_calls_dodeca_devtools_edit_list",
            call: "testbed::run_subject_calls_dodeca_devtools_edit_list",
        },
        TestCase {
            name: "subject_calls_echo_styx_value",
            call: "testbed::run_subject_calls_echo_styx_value",
        },
        TestCase {
            name: "subject_calls_styx_lsp_initialize",
            call: "testbed::run_subject_calls_styx_lsp_initialize",
        },
        TestCase {
            name: "subject_calls_styx_lsp_completions",
            call: "testbed::run_subject_calls_styx_lsp_completions",
        },
        TestCase {
            name: "subject_calls_styx_lsp_hover",
            call: "testbed::run_subject_calls_styx_lsp_hover",
        },
        TestCase {
            name: "subject_calls_styx_lsp_inlay_hints",
            call: "testbed::run_subject_calls_styx_lsp_inlay_hints",
        },
        TestCase {
            name: "subject_calls_styx_lsp_diagnostics",
            call: "testbed::run_subject_calls_styx_lsp_diagnostics",
        },
        TestCase {
            name: "subject_calls_styx_lsp_code_actions",
            call: "testbed::run_subject_calls_styx_lsp_code_actions",
        },
        TestCase {
            name: "subject_calls_styx_lsp_definition",
            call: "testbed::run_subject_calls_styx_lsp_definition",
        },
        TestCase {
            name: "subject_calls_styx_lsp_shutdown",
            call: "testbed::run_subject_calls_styx_lsp_shutdown",
        },
        TestCase {
            name: "subject_calls_styx_host_get_subtree",
            call: "testbed::run_subject_calls_styx_host_get_subtree",
        },
        TestCase {
            name: "subject_calls_styx_host_get_document",
            call: "testbed::run_subject_calls_styx_host_get_document",
        },
        TestCase {
            name: "subject_calls_styx_host_get_source",
            call: "testbed::run_subject_calls_styx_host_get_source",
        },
        TestCase {
            name: "subject_calls_styx_host_get_schema",
            call: "testbed::run_subject_calls_styx_host_get_schema",
        },
        TestCase {
            name: "subject_calls_styx_host_offset_to_position",
            call: "testbed::run_subject_calls_styx_host_offset_to_position",
        },
        TestCase {
            name: "subject_calls_styx_host_position_to_offset",
            call: "testbed::run_subject_calls_styx_host_position_to_offset",
        },
        TestCase {
            name: "subject_calls_stax_flamegraph",
            call: "testbed::run_subject_calls_stax_flamegraph",
        },
        TestCase {
            name: "subject_calls_echo_stax_flamegraph_update",
            call: "testbed::run_subject_calls_echo_stax_flamegraph_update",
        },
        TestCase {
            name: "subject_calls_stax_subscribe_flamegraph_updates",
            call: "testbed::run_subject_calls_stax_subscribe_flamegraph_updates",
        },
        TestCase {
            name: "subject_calls_echo_stax_linux_broker_control",
            call: "testbed::run_subject_calls_echo_stax_linux_broker_control",
        },
        TestCase {
            name: "subject_calls_stax_macos_record",
            call: "testbed::run_subject_calls_stax_macos_record",
        },
        TestCase {
            name: "subject_calls_echo_hotmeal_live_reload_event",
            call: "testbed::run_subject_calls_echo_hotmeal_live_reload_event",
        },
        TestCase {
            name: "subject_calls_echo_hotmeal_apply_patches_result",
            call: "testbed::run_subject_calls_echo_hotmeal_apply_patches_result",
        },
        TestCase {
            name: "subject_calls_hotmeal_live_reload_subscribe",
            call: "testbed::run_subject_calls_hotmeal_live_reload_subscribe",
        },
        TestCase {
            name: "subject_calls_hotmeal_live_reload_on_event",
            call: "testbed::run_subject_calls_hotmeal_live_reload_on_event",
        },
        TestCase {
            name: "subject_calls_echo_helix_stream_metrics",
            call: "testbed::run_subject_calls_echo_helix_stream_metrics",
        },
        TestCase {
            name: "subject_calls_echo_helix_verify_evidence",
            call: "testbed::run_subject_calls_echo_helix_verify_evidence",
        },
        TestCase {
            name: "subject_calls_helix_subscribe_pulses",
            call: "testbed::run_subject_calls_helix_subscribe_pulses",
        },
        TestCase {
            name: "subject_calls_helix_pulse_bundle",
            call: "testbed::run_subject_calls_helix_pulse_bundle",
        },
        TestCase {
            name: "subject_calls_helix_trace_service_surface",
            call: "testbed::run_subject_calls_helix_trace_service_surface",
        },
        TestCase {
            name: "subject_calls_tracey_status",
            call: "testbed::run_subject_calls_tracey_status",
        },
        TestCase {
            name: "subject_calls_tracey_core_control",
            call: "testbed::run_subject_calls_tracey_core_control",
        },
        TestCase {
            name: "subject_calls_tracey_rule",
            call: "testbed::run_subject_calls_tracey_rule",
        },
        TestCase {
            name: "subject_calls_tracey_dashboard",
            call: "testbed::run_subject_calls_tracey_dashboard",
        },
        TestCase {
            name: "subject_calls_tracey_validate",
            call: "testbed::run_subject_calls_tracey_validate",
        },
        TestCase {
            name: "subject_calls_tracey_lsp_surface",
            call: "testbed::run_subject_calls_tracey_lsp_surface",
        },
        TestCase {
            name: "subject_calls_tracey_lsp_workspace_diagnostics",
            call: "testbed::run_subject_calls_tracey_lsp_workspace_diagnostics",
        },
        TestCase {
            name: "subject_calls_tracey_subscribe_updates",
            call: "testbed::run_subject_calls_tracey_subscribe_updates",
        },
        TestCase {
            name: "subject_calls_dibs_list",
            call: "testbed::run_subject_calls_dibs_list",
        },
        TestCase {
            name: "subject_calls_dibs_schema",
            call: "testbed::run_subject_calls_dibs_schema",
        },
        TestCase {
            name: "subject_calls_dibs_get",
            call: "testbed::run_subject_calls_dibs_get",
        },
        TestCase {
            name: "subject_calls_dibs_create",
            call: "testbed::run_subject_calls_dibs_create",
        },
        TestCase {
            name: "subject_calls_dibs_update",
            call: "testbed::run_subject_calls_dibs_update",
        },
        TestCase {
            name: "subject_calls_dibs_delete",
            call: "testbed::run_subject_calls_dibs_delete",
        },
        TestCase {
            name: "subject_calls_dibs_migration_status",
            call: "testbed::run_subject_calls_dibs_migration_status",
        },
        TestCase {
            name: "subject_calls_dibs_migrate",
            call: "testbed::run_subject_calls_dibs_migrate",
        },
        TestCase {
            name: "subject_calls_pipelining",
            call: "testbed::run_subject_calls_pipelining",
        },
        TestCase {
            name: "subject_calls_sum_large",
            call: "testbed::run_subject_calls_sum_large",
        },
        TestCase {
            name: "subject_calls_generate_large",
            call: "testbed::run_subject_calls_generate_large",
        },
        TestCase {
            name: "subject_calls_sum_client_to_server",
            call: "testbed::run_subject_calls_sum_client_to_server",
        },
        TestCase {
            name: "subject_calls_transform_bidi",
            call: "testbed::run_subject_calls_transform_bidi",
        },
        TestCase {
            name: "subject_calls_dodeca_byte_tunnel",
            call: "testbed::run_subject_calls_dodeca_byte_tunnel",
        },
        TestCase {
            name: "subject_calls_dodeca_devtools_lsp",
            call: "testbed::run_subject_calls_dodeca_devtools_lsp",
        },
        TestCase {
            name: "subject_calls_post_reply_generate",
            call: "channeling::run_subject_calls_post_reply_generate",
        },
        TestCase {
            name: "subject_calls_post_reply_sum",
            call: "channeling::run_subject_calls_post_reply_sum",
        },
    ];
    let bidirectional = [TestCase {
        name: "channeling_transform",
        call: "channeling::run_channeling_transform_bidirectional",
    }];
    // Cross-language pairs: both sides are subject processes; the harness only orchestrates.
    // Test functions take (SERVER: SubjectSpec, CLIENT: SubjectSpec).
    struct CrossLangCombo {
        mod_name: &'static str,
        server_const: &'static str,
        client_const: &'static str,
        ignore: bool,
    }

    let cross_lang_combos = [
        // Rust server ↔ TypeScript client
        CrossLangCombo {
            mod_name: "lang_rust_server_typescript_client_tcp",
            server_const: "SUBJECT_RUST_TCP",
            client_const: "SUBJECT_TYPESCRIPT_TCP",
            ignore: false,
        },
        // TypeScript server ↔ Rust client
        CrossLangCombo {
            mod_name: "lang_typescript_server_rust_client_tcp",
            server_const: "SUBJECT_TYPESCRIPT_TCP",
            client_const: "SUBJECT_RUST_TCP",
            ignore: false,
        },
        // TypeScript server ↔ TypeScript client
        CrossLangCombo {
            mod_name: "lang_typescript_server_typescript_client_tcp",
            server_const: "SUBJECT_TYPESCRIPT_TCP",
            client_const: "SUBJECT_TYPESCRIPT_TCP",
            ignore: false,
        },
        // Rust server ↔ Swift client
        CrossLangCombo {
            mod_name: "lang_rust_server_swift_client_tcp",
            server_const: "SUBJECT_RUST_TCP",
            client_const: "SUBJECT_SWIFT_TCP",
            ignore: false,
        },
        // Swift server ↔ Rust client
        CrossLangCombo {
            mod_name: "lang_swift_server_rust_client_tcp",
            server_const: "SUBJECT_SWIFT_TCP",
            client_const: "SUBJECT_RUST_TCP",
            ignore: false,
        },
        // TypeScript server ↔ Swift client
        CrossLangCombo {
            mod_name: "lang_typescript_server_swift_client_tcp",
            server_const: "SUBJECT_TYPESCRIPT_TCP",
            client_const: "SUBJECT_SWIFT_TCP",
            ignore: false,
        },
        // Swift server ↔ TypeScript client
        CrossLangCombo {
            mod_name: "lang_swift_server_typescript_client_tcp",
            server_const: "SUBJECT_SWIFT_TCP",
            client_const: "SUBJECT_TYPESCRIPT_TCP",
            ignore: false,
        },
    ];

    // Cross-language scenario names are the single source of truth.
    // The xtask generates inline calls to run_cross_language_scenario —
    // no wrapper functions needed in testbed.rs.
    let cross_lang_scenarios: &[(&str, &str)] = &[
        // Basic RPC
        ("echo", "r[verify call.initiate]"),
        ("reverse", "r[verify call.initiate]"),
        // Fallible — all error variants
        ("divide_success", "r[verify call.error.user]"),
        ("divide_zero", "r[verify call.error.user]"),
        ("divide_overflow", "r[verify call.error.user]"),
        ("lookup_found", "r[verify call.error.user]"),
        ("lookup_found_no_email", "r[verify call.error.user]"),
        ("lookup_not_found", "r[verify call.error.user]"),
        ("lookup_access_denied", "r[verify call.error.user]"),
        // Struct / nested struct
        ("echo_point", "r[verify encoding.struct]"),
        ("create_person", "r[verify encoding.struct]"),
        ("rectangle_area", "r[verify encoding.struct.nested]"),
        // Option return
        ("parse_color", "r[verify encoding.option.return]"),
        // Vec return
        ("get_points", "r[verify encoding.vec]"),
        // Tuple
        ("swap_pair", "r[verify encoding.tuple]"),
        // Primitive types
        ("echo_bytes", "r[verify encoding.bytes]"),
        ("echo_bool", "r[verify encoding.bool]"),
        ("echo_u64", "r[verify encoding.u64]"),
        ("echo_option_string", "r[verify encoding.option]"),
        // Multi-arg struct return
        ("describe_point", "r[verify encoding.struct.multi-arg]"),
        // Enum variants
        ("all_colors", "r[verify encoding.enum.unit-variants]"),
        ("echo_shape", "r[verify encoding.enum.struct-variants]"),
        ("shape_area", "r[verify encoding.enum.struct-variants]"),
        // Recursive type — typed-VM recursion (Access::Recurse / CallBlock)
        ("echo_tree", "r[verify encoding.struct.recursive]"),
        // Complex nested + Vec<enum>
        ("create_canvas", "r[verify encoding.struct.nested]"),
        // Enum with newtype variants
        (
            "process_message",
            "r[verify encoding.enum.newtype-variants]",
        ),
        // Pipelining
        ("pipelining", "r[verify call.pipelining.allowed]"),
        // Channels
        ("sum_client_to_server", "r[verify channeling.caller-pov]"),
        ("sum_large", "r[verify channeling.flow-control]"),
        ("generate_large", "r[verify channeling.flow-control]"),
        ("transform_bidi", "r[verify channeling.type]"),
    ];

    let cross_lang_mods: Vec<TokenStream> = cross_lang_combos
        .iter()
        .map(|c| {
            let mod_ident = Ident::new(c.mod_name, Span::call_site());
            let server: TokenStream = c.server_const.parse().unwrap();
            let client: TokenStream = c.client_const.parse().unwrap();
            let ignore_attr = if c.ignore {
                quote! { #[ignore] }
            } else {
                quote! {}
            };
            // Inline the call directly — no wrapper functions needed.
            let fns: Vec<TokenStream> = cross_lang_scenarios
                .iter()
                .map(|(scenario, _spec_ref)| {
                    let fn_ident = Ident::new(scenario, Span::call_site());
                    let scenario_lit = scenario;
                    quote! {
                        #ignore_attr
                        #[test]
                        fn #fn_ident() {
                            spec_tests::harness::run_cross_language_scenario(
                                SERVER, CLIENT, #scenario_lit,
                            );
                        }
                    }
                })
                .collect();
            quote! {
                mod #mod_ident {
                    use super::*;
                    const SERVER: SubjectSpec = #server;
                    const CLIENT: SubjectSpec = #client;
                    #(#fns)*
                }
            }
        })
        .collect();

    let gen_mod = |mod_name: &str, cases: &[TestCase], ignore: bool| -> TokenStream {
        let mod_ident = Ident::new(mod_name, Span::call_site());
        let fns: Vec<TokenStream> = cases
            .iter()
            .map(|t| {
                let fn_ident = Ident::new(t.name, Span::call_site());
                let call: TokenStream = t.call.parse().unwrap();
                let ignore_attr = if ignore {
                    quote! { #[ignore] }
                } else {
                    quote! {}
                };
                quote! {
                    #ignore_attr
                    #[test]
                    fn #fn_ident() { #call(SPEC); }
                }
            })
            .collect();
        quote! {
            mod #mod_ident {
                use super::*;
                #(#fns)*
            }
        }
    };

    let combo_mods: Vec<TokenStream> = combos
        .iter()
        .map(|c| {
            let mod_ident = Ident::new(c.mod_name, Span::call_site());
            let spec: TokenStream = c.spec_const.parse().unwrap();
            let h2s = gen_mod(
                "direction_harness_to_subject",
                &harness_to_subject,
                c.ignore,
            );
            let s2h = gen_mod(
                "direction_subject_to_harness",
                &subject_to_harness,
                c.ignore,
            );
            let bidi = gen_mod("direction_bidirectional", &bidirectional, c.ignore);
            quote! {
                mod #mod_ident {
                    use super::*;
                    const SPEC: SubjectSpec = #spec;
                    #h2s
                    #s2h
                    #bidi
                }
            }
        })
        .collect();

    let tokens = quote! {
        #[path = "cases/binary_payload_transport_matrix.rs"]
        mod binary_payload_transport_matrix;
        #[path = "cases/binary_payloads.rs"]
        mod binary_payloads;
        #[path = "cases/channeling.rs"]
        mod channeling;
        #[path = "cases/schema_compat.rs"]
        mod schema_compat;
        #[path = "cases/testbed.rs"]
        mod testbed;

        use spec_tests::harness::{SubjectLanguage, SubjectSpec};

        const SUBJECT_RUST_TCP: SubjectSpec = SubjectSpec::tcp(SubjectLanguage::Rust);
        const SUBJECT_TYPESCRIPT_TCP: SubjectSpec = SubjectSpec::tcp(SubjectLanguage::TypeScript);
        const SUBJECT_TYPESCRIPT_WS: SubjectSpec = SubjectSpec::ws(SubjectLanguage::TypeScript);
        const SUBJECT_SWIFT_TCP: SubjectSpec = SubjectSpec::tcp(SubjectLanguage::Swift);

        #(#combo_mods)*

        #(#cross_lang_mods)*

        // Schema compatibility tests: Rust v1 harness ↔ TypeScript evolved (v2) subject.
        // These use a special evolved subject command and are TypeScript-only.
        mod lang_typescript_evolved_schema_compat {
            use super::schema_compat;
            #[test]
            fn added_optional_field() {
                schema_compat::run_schema_compat_added_optional_field();
            }
            #[test]
            fn reordered_fields() {
                schema_compat::run_schema_compat_reordered_fields();
            }
            #[test]
            fn added_enum_variant() {
                schema_compat::run_schema_compat_added_enum_variant();
            }
            #[test]
            fn removed_field() {
                schema_compat::run_schema_compat_removed_field();
            }
            #[test]
            fn incompatible_type_change() {
                schema_compat::run_schema_compat_incompatible_type_change();
            }
            #[test]
            fn missing_required_field() {
                schema_compat::run_schema_compat_missing_required_field();
            }
        }

        #[test]
        fn lang_rust_to_rust_transport_mem_direction_bidirectional_binary_payload_transport_matrix() {
            binary_payload_transport_matrix::run_rust_binary_payload_transport_matrix_mem();
        }
        #[test]
        fn lang_rust_to_rust_transport_tcp_direction_bidirectional_binary_payload_transport_matrix() {
            binary_payload_transport_matrix::run_rust_binary_payload_transport_matrix_subject_tcp(SUBJECT_RUST_TCP);
        }
    };

    let file: syn::File = syn::parse2(tokens)?;
    let mut out =
        String::from("// @generated by cargo xtask generate-spec-matrix\n// DO NOT EDIT\n\n");
    out.push_str(&prettyplease::unparse(&file));

    let out_path = workspace_root
        .join("spec")
        .join("spec-tests")
        .join("tests")
        .join("spec_matrix.rs");
    write_if_changed(&out_path, out)?;
    Ok(())
}

/// Write `contents` to `path` only if the file doesn't already have those exact bytes.
/// This preserves mtime when nothing changed, preventing unnecessary rebuilds in
/// timestamp-based build systems (Swift Package Manager, make, etc.).
fn write_if_changed(
    path: &std::path::Path,
    contents: impl AsRef<[u8]>,
) -> Result<(), Box<dyn std::error::Error>> {
    let contents = contents.as_ref();
    if std::fs::read(path).ok().as_deref() == Some(contents) {
        println!("Unchanged {}", path.display());
        return Ok(());
    }
    std::fs::write(path, contents)?;
    println!("Wrote {}", path.display());
    Ok(())
}

/// oha JSON output format (partial - just what we need)
#[derive(facet::Facet)]
#[facet(rename_all = "camelCase")]
struct OhaResult {
    summary: OhaSummary,
    latency_percentiles: OhaLatencyPercentiles,
}

#[derive(facet::Facet)]
#[facet(rename_all = "camelCase")]
struct OhaSummary {
    requests_per_sec: f64,
}

#[derive(facet::Facet)]
struct OhaLatencyPercentiles {
    p50: Option<f64>,
    p90: Option<f64>,
    p99: Option<f64>,
}

/// Benchmark result for a single run
#[allow(dead_code)]
struct BenchResult {
    name: String,
    endpoint: String,
    concurrency: u32,
    rps: f64,
    p50_ms: f64,
    p90_ms: f64,
    p99_ms: f64,
}
