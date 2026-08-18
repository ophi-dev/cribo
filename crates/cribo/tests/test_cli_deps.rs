//! End-to-end tests for the `cribo deps` subcommand: third-party dependency
//! detection for a Python file or directory.

mod common;

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use tempfile::TempDir;

/// Build a Cribo command with deterministic test output.
fn cribo_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cribo"));
    command
        .env("RUST_LOG", "off")
        .env("CARGO_TERM_COLOR", "never")
        .env("NO_COLOR", "1");
    command
}

/// Create an importable package and its owning distribution metadata.
fn write_test_distribution(
    root: &Path,
    module_name: &str,
    distribution_name: &str,
    module_body: &str,
) {
    let package_dir = root.join(module_name);
    fs::create_dir_all(&package_dir).expect("Failed to create test package");
    fs::write(package_dir.join("__init__.py"), module_body).expect("Failed to write test package");

    let metadata_dir = root.join(format!("{distribution_name}-1.0.dist-info"));
    fs::create_dir_all(&metadata_dir).expect("Failed to create distribution metadata");
    fs::write(
        metadata_dir.join("METADATA"),
        format!(
            "Metadata-Version: 2.5\nName: {distribution_name}\nVersion: 1.0\nImport-Name: \
             {module_name}\n"
        ),
    )
    .expect("Failed to write distribution metadata");
}

/// Sandbox for deps tests: a project directory and a fake virtualenv whose
/// site-packages carries the given distributions.
struct DepsSandbox {
    _sandbox: TempDir,
    project_dir: PathBuf,
    environment: PathBuf,
    site_packages: PathBuf,
}

impl DepsSandbox {
    fn new() -> Self {
        let sandbox = TempDir::new().expect("Failed to create temporary directory");
        let project_dir = sandbox.path().join("project");
        fs::create_dir_all(&project_dir).expect("Failed to create project directory");

        let environment = sandbox.path().join("venv-env");
        let site_packages = if cfg!(windows) {
            environment.join("Lib").join("site-packages")
        } else {
            environment
                .join("lib")
                .join("python3.12")
                .join("site-packages")
        };
        fs::create_dir_all(&site_packages).expect("Failed to create site-packages");
        fs::create_dir_all(environment.join(if cfg!(windows) { "Scripts" } else { "bin" }))
            .expect("Failed to create virtualenv executable directory");

        Self {
            _sandbox: sandbox,
            project_dir,
            environment,
            site_packages,
        }
    }

    /// Write a project file, creating parent directories as needed.
    fn write_file(&self, relative_path: &str, content: &str) -> PathBuf {
        let path = self.project_dir.join(relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("Failed to create parent directory");
        }
        fs::write(&path, content).expect("Failed to write project file");
        path
    }

    /// Install a fake distribution into the sandbox virtualenv.
    fn install(&self, module_name: &str, distribution_name: &str) {
        write_test_distribution(&self.site_packages, module_name, distribution_name, "");
    }

    /// Run `cribo deps` with the given extra arguments against an entry path.
    fn run_deps(&self, entry: &Path, extra_args: &[&str]) -> Output {
        let mut command = cribo_command();
        command
            .arg("deps")
            .arg("--entry")
            .arg(entry)
            .arg("--python")
            .arg(common::get_python_executable())
            .args(extra_args)
            .env("VIRTUAL_ENV", &self.environment)
            .env_remove("PYTHONPATH")
            .env_remove("CONDA_PREFIX")
            .current_dir(&self.project_dir);
        command.output().expect("Failed to execute cribo deps")
    }
}

/// Assert success and return stdout.
fn stdout_of(output: &Output) -> String {
    assert!(
        output.status.success(),
        "cribo deps failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// A single entry file whose used imports resolve to distribution names through
/// virtualenv site-packages metadata; stdlib and first-party imports are excluded.
#[test]
fn test_deps_basic_detection() {
    let sandbox = DepsSandbox::new();
    sandbox.install("fancy_http", "fancy-http-client");
    sandbox.install("fancy_yaml", "fancy-yaml");
    sandbox.write_file(
        "helper.py",
        "import fancy_yaml\n\nDATA = fancy_yaml.__name__\n",
    );
    let entry = sandbox.write_file(
        "main.py",
        "import os\nimport fancy_http\nimport helper\n\nprint(os.name, fancy_http.__name__, \
         helper.DATA)\n",
    );

    let output = sandbox.run_deps(&entry, &[]);
    assert_eq!(stdout_of(&output), "fancy-http-client\nfancy-yaml\n");
}

/// Tree-shaking drops imports only referenced by unused code; --no-tree-shake
/// keeps them.
#[test]
fn test_deps_obeys_tree_shaking() {
    let sandbox = DepsSandbox::new();
    sandbox.install("used_pkg", "used-distribution");
    sandbox.install("unused_pkg", "unused-distribution");
    let entry = sandbox.write_file(
        "main.py",
        "import used_pkg\nimport unused_pkg\n\nprint(used_pkg.__name__)\n",
    );

    let shaken = sandbox.run_deps(&entry, &[]);
    assert_eq!(stdout_of(&shaken), "used-distribution\n");

    let unshaken = sandbox.run_deps(&entry, &["--no-tree-shake"]);
    assert_eq!(
        stdout_of(&unshaken),
        "unused-distribution\nused-distribution\n"
    );
}

/// TYPE_CHECKING-only imports are included by default and skipped with
/// --exclude-type-checking.
#[test]
fn test_deps_exclude_type_checking() {
    let sandbox = DepsSandbox::new();
    sandbox.install("runtime_pkg", "runtime-distribution");
    sandbox.install("typing_pkg", "typing-distribution");
    let entry = sandbox.write_file(
        "main.py",
        "from typing import TYPE_CHECKING\n\nimport runtime_pkg\n\nif TYPE_CHECKING:\n    import \
         typing_pkg\n\nprint(runtime_pkg.__name__)\n",
    );

    let default_run = sandbox.run_deps(&entry, &[]);
    assert_eq!(
        stdout_of(&default_run),
        "runtime-distribution\ntyping-distribution\n"
    );

    let excluded = sandbox.run_deps(&entry, &["--exclude-type-checking"]);
    assert_eq!(stdout_of(&excluded), "runtime-distribution\n");
}

/// Conditional imports (try/except, if blocks) are included by default and
/// skipped with `--exclude-conditional`; `TYPE_CHECKING` imports are unaffected.
#[test]
fn test_deps_exclude_conditional() {
    let sandbox = DepsSandbox::new();
    sandbox.install("runtime_pkg", "runtime-distribution");
    sandbox.install("optional_pkg", "optional-distribution");
    sandbox.install("typing_pkg", "typing-distribution");
    let entry = sandbox.write_file(
        "main.py",
        "from typing import TYPE_CHECKING\n\nimport runtime_pkg\n\nif TYPE_CHECKING:\n    import \
         typing_pkg\n\ntry:\n    import optional_pkg\nexcept ImportError:\n    optional_pkg = \
         None\n\nprint(runtime_pkg.__name__)\n",
    );

    let default_run = sandbox.run_deps(&entry, &[]);
    assert_eq!(
        stdout_of(&default_run),
        "optional-distribution\nruntime-distribution\ntyping-distribution\n"
    );

    let excluded = sandbox.run_deps(&entry, &["--exclude-conditional"]);
    assert_eq!(
        stdout_of(&excluded),
        "runtime-distribution\ntyping-distribution\n"
    );
}

/// JSON format reports per-import classification and requirement mapping.
#[test]
fn test_deps_json_format() {
    let sandbox = DepsSandbox::new();
    sandbox.install("runtime_pkg", "runtime-distribution");
    sandbox.install("typing_pkg", "typing-distribution");
    let entry = sandbox.write_file(
        "main.py",
        "from typing import TYPE_CHECKING\n\nimport runtime_pkg\n\nif TYPE_CHECKING:\n    import \
         typing_pkg\n\nprint(runtime_pkg.__name__)\n",
    );

    let output = sandbox.run_deps(&entry, &["--format", "json", "--exclude-type-checking"]);
    let report: serde_json::Value =
        serde_json::from_str(&stdout_of(&output)).expect("stdout must be valid JSON");

    assert_eq!(
        report["requirements"],
        serde_json::Value::from(vec!["runtime-distribution"])
    );

    let imports = report["imports"]
        .as_array()
        .expect("imports must be an array");
    assert_eq!(imports.len(), 2);

    let runtime = &imports[0];
    assert_eq!(runtime["module"], "runtime_pkg");
    assert_eq!(runtime["requirement"], "runtime-distribution");
    assert_eq!(runtime["type_checking_only"], false);
    assert_eq!(runtime["conditional"], false);
    assert_eq!(runtime["included"], true);
    assert_eq!(
        runtime["imported_by"],
        serde_json::Value::from(vec!["main"])
    );

    let typing = &imports[1];
    assert_eq!(typing["module"], "typing_pkg");
    assert_eq!(typing["requirement"], serde_json::Value::Null);
    assert_eq!(typing["type_checking_only"], true);
    assert_eq!(typing["included"], false);
}

/// --output writes the report to a file instead of stdout.
#[test]
fn test_deps_output_file() {
    let sandbox = DepsSandbox::new();
    sandbox.install("filed_pkg", "filed-distribution");
    let entry = sandbox.write_file("main.py", "import filed_pkg\n\nprint(filed_pkg.__name__)\n");
    let requirements_path = sandbox.project_dir.join("requirements.txt");

    let output = sandbox.run_deps(
        &entry,
        &[
            "--output",
            requirements_path
                .to_str()
                .expect("requirements path must be valid UTF-8"),
        ],
    );
    assert_eq!(stdout_of(&output), "");
    assert_eq!(
        fs::read_to_string(&requirements_path).expect("requirements file must exist"),
        "filed-distribution\n"
    );
}

/// A directory entry without __init__.py/__main__.py scans every Python source:
/// top-level scripts, packages, and package submodules never imported by their
/// package (coverage sweep).
#[test]
fn test_deps_directory_scan() {
    let sandbox = DepsSandbox::new();
    sandbox.install("script_pkg", "script-distribution");
    sandbox.install("package_pkg", "package-distribution");
    sandbox.install("orphan_pkg", "orphan-distribution");

    sandbox.write_file(
        "src/script.py",
        "import script_pkg\n\nprint(script_pkg.__name__)\n",
    );
    sandbox.write_file("src/mypkg/__init__.py", "from . import used\n");
    sandbox.write_file(
        "src/mypkg/used.py",
        "import package_pkg\n\nprint(package_pkg.__name__)\n",
    );
    // Not imported by the package: only the coverage sweep reaches it
    sandbox.write_file(
        "src/mypkg/orphan.py",
        "import orphan_pkg\n\nprint(orphan_pkg.__name__)\n",
    );
    // Directories that must be ignored while scanning
    sandbox.write_file("src/__pycache__/junk.py", "import junk_pkg\n");
    sandbox.write_file("src/.hidden/junk.py", "import junk_pkg\n");

    let output = sandbox.run_deps(&sandbox.project_dir.join("src"), &[]);
    assert_eq!(
        stdout_of(&output),
        "orphan-distribution\npackage-distribution\nscript-distribution\n"
    );
}

/// A directory with __main__.py is analyzed through the regular entry machinery.
#[test]
fn test_deps_directory_with_main() {
    let sandbox = DepsSandbox::new();
    sandbox.install("runnable_pkg", "runnable-distribution");
    sandbox.write_file(
        "app/__main__.py",
        "import runnable_pkg\n\nprint(runnable_pkg.__name__)\n",
    );

    let output = sandbox.run_deps(&sandbox.project_dir.join("app"), &[]);
    assert_eq!(stdout_of(&output), "runnable-distribution\n");
}

/// A directory symlink pointing at an ancestor must not send the scan into
/// infinite recursion.
#[cfg(unix)]
#[test]
fn test_deps_directory_scan_survives_symlink_cycle() {
    let sandbox = DepsSandbox::new();
    sandbox.install("looped_pkg", "looped-distribution");
    sandbox.write_file(
        "src/main.py",
        "import looped_pkg\n\nprint(looped_pkg.__name__)\n",
    );
    // src/loop -> src creates a directory cycle
    std::os::unix::fs::symlink(
        sandbox.project_dir.join("src"),
        sandbox.project_dir.join("src").join("loop"),
    )
    .expect("Failed to create cyclic symlink");

    let output = sandbox.run_deps(&sandbox.project_dir.join("src"), &[]);
    assert_eq!(stdout_of(&output), "looped-distribution\n");
}

/// In directory-scan mode a broken file is skipped with a warning instead of
/// hiding every other file's dependencies.
#[test]
fn test_deps_directory_scan_tolerates_broken_file() {
    let sandbox = DepsSandbox::new();
    sandbox.install("good_pkg", "good-distribution");
    sandbox.write_file(
        "src/good.py",
        "import good_pkg\n\nprint(good_pkg.__name__)\n",
    );
    sandbox.write_file("src/broken.py", "def broken(:\n");

    let output = sandbox.run_deps(&sandbox.project_dir.join("src"), &[]);
    assert_eq!(stdout_of(&output), "good-distribution\n");
}

/// For a single-file entry, analysis failures are fatal.
#[test]
fn test_deps_single_file_failure_is_fatal() {
    let sandbox = DepsSandbox::new();
    let entry = sandbox.write_file("broken.py", "def broken(:\n");

    let output = sandbox.run_deps(&entry, &[]);
    assert!(
        !output.status.success(),
        "a broken single-file entry must fail"
    );
}

/// requirements.module-map config overrides metadata-based resolution.
#[test]
fn test_deps_respects_module_map_config() {
    let sandbox = DepsSandbox::new();
    sandbox.install("mapped_pkg", "wrong-distribution");
    let entry = sandbox.write_file(
        "main.py",
        "import mapped_pkg\n\nprint(mapped_pkg.__name__)\n",
    );
    let config_path = sandbox.write_file(
        "cribo-map.toml",
        "[requirements]\nmodule-map = { mapped_pkg = \"right-distribution>=2\" }\n",
    );

    let output = sandbox.run_deps(
        &entry,
        &[
            "--config",
            config_path
                .to_str()
                .expect("config path must be valid UTF-8"),
        ],
    );
    assert_eq!(stdout_of(&output), "right-distribution>=2\n");
}

/// PYTHONPATH participates in classification: modules found there with adjacent
/// dist-info are third-party requirements.
#[test]
fn test_deps_follows_pythonpath() {
    let sandbox = DepsSandbox::new();
    let vendored_root = sandbox.project_dir.join("vendored");
    fs::create_dir_all(&vendored_root).expect("Failed to create vendored root");
    write_test_distribution(
        &vendored_root,
        "vendored_pkg",
        "vendored-distribution",
        "VALUE = 1\n",
    );
    let entry = sandbox.write_file(
        "main.py",
        "import vendored_pkg\n\nprint(vendored_pkg.VALUE)\n",
    );

    let mut command = cribo_command();
    command
        .arg("deps")
        .arg("--entry")
        .arg(&entry)
        .arg("--python")
        .arg(common::get_python_executable())
        .env("PYTHONPATH", &vendored_root)
        .env_remove("VIRTUAL_ENV")
        .env_remove("CONDA_PREFIX")
        .current_dir(&sandbox.project_dir);
    let output = command.output().expect("Failed to execute cribo deps");
    assert_eq!(stdout_of(&output), "vendored-distribution\n");
}

/// The legacy flat CLI keeps working alongside the new subcommand.
#[test]
fn test_bundle_cli_still_works_without_subcommand() {
    let sandbox = DepsSandbox::new();
    let entry = sandbox.write_file("main.py", "print('hello')\n");

    let mut command = cribo_command();
    command
        .arg("--entry")
        .arg(&entry)
        .arg("--stdout")
        .env_remove("PYTHONPATH")
        .env_remove("VIRTUAL_ENV")
        .env_remove("CONDA_PREFIX")
        .current_dir(&sandbox.project_dir);
    let output = command.output().expect("Failed to execute cribo");
    let stdout = stdout_of(&output);
    assert!(stdout.contains("print('hello')") || stdout.contains("print(\"hello\")"));
}

/// Missing --entry without a subcommand is still an error.
#[test]
fn test_bundle_cli_missing_entry_errors() {
    let output = cribo_command()
        .arg("--stdout")
        .output()
        .expect("Failed to execute cribo");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--entry"),
        "error must mention --entry, got: {stderr}"
    );
}
