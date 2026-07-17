#![expect(clippy::disallowed_methods)] // insta macros use unwrap internally

mod common;

use std::{
    env, fs,
    path::Path,
    process::{Command, Output},
};

use insta::{assert_snapshot, with_settings};
use tempfile::TempDir;

/// Helper function to get the path to a fixture file
fn get_fixture_path(relative_path: &str) -> String {
    let cwd = env::current_dir().expect("Failed to get current directory");
    let test_fixture_path = cwd.join("tests/fixtures").join(relative_path);
    test_fixture_path.to_string_lossy().to_string()
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
            "Metadata-Version: 2.5\n\
             Name: {distribution_name}\n\
             Version: 1.0\n\
             Import-Name: {module_name}\n"
        ),
    )
    .expect("Failed to write distribution metadata");
}

/// Build a Cribo command with deterministic test output.
fn cribo_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cribo"));
    command
        .env("RUST_LOG", "off")
        .env("CARGO_TERM_COLOR", "never")
        .env("NO_COLOR", "1");
    command
}

/// Run cribo with given arguments and return (stdout, stderr, `exit_code`)
fn run_cribo(args: &[&str]) -> (String, String, i32) {
    // Use the pre-built binary instead of cargo run for performance
    let output = cribo_command()
        .args(args)
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let exit_code = output.status.code().unwrap_or(-1);

    (stdout, stderr, exit_code)
}

/// Run Cribo with requirement generation enabled and customize its environment.
fn run_requirement_cribo(
    entry_path: &Path,
    output_path: &Path,
    configure: impl FnOnce(&mut Command),
) -> Output {
    let mut command = cribo_command();
    command
        .arg("--entry")
        .arg(entry_path)
        .arg("--output")
        .arg(output_path)
        .arg("--emit-requirements")
        .arg("--python")
        .arg(common::get_python_executable())
        .env_remove("VIRTUAL_ENV")
        .env_remove("CONDA_PREFIX");
    configure(&mut command);
    command.output().expect("Failed to execute cribo")
}

/// Assert that Cribo succeeded and emitted the expected requirement.
fn assert_requirement_output(output: &Output, output_dir: &Path, expected: &str) {
    assert!(
        output.status.success(),
        "Cribo failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(output_dir.join("requirements.txt"))
            .expect("Failed to read requirements"),
        expected
    );
}

/// Filters for normalizing paths in snapshots
fn get_cli_filters() -> Vec<(&'static str, &'static str)> {
    vec![
        // Normalize file paths - Unix/macOS
        (r"/Volumes/workplace/[^\s]+", "<WORKSPACE>"),
        (r"/home/[^/]+/[^\s]+", "<WORKSPACE>"),
        (r"/Users/[^/]+/[^\s]+", "<WORKSPACE>"),
        // Normalize file paths - Windows
        (r"\\\\?[A-Z]:\\[^\s]+", "<WORKSPACE>"),
        (r"[A-Z]:\\[^\s]+", "<WORKSPACE>"),
        (r"[A-Z]:/[^\s]+", "<WORKSPACE>"),
        // Normalize cargo paths - Unix/macOS
        (r"/Users/[^/]+/\.cargo/[^\s]+", "<CARGO>"),
        (r"/home/[^/]+/\.cargo/[^\s]+", "<CARGO>"),
        // Normalize cargo paths - Windows
        (r"\\\\?C:\\Users\\[^\\]+\\\.cargo\\[^\s]+", "<CARGO>"),
        (r"C:\\Users\\[^\\]+\\\.cargo\\[^\s]+", "<CARGO>"),
        // Normalize temporary paths - Unix/macOS
        (r"/var/folders/[^/]+/[^/]+/T/[^\s]+", "<TMP>"),
        (r"/tmp/[^\s]+", "<TMP>"),
        // Normalize temporary paths - Windows
        (r"\\\\?C:\\temp\\[^\s]+", "<TMP>"),
        (r"\\\\?C:\\Windows\\Temp\\[^\s]+", "<TMP>"),
        // Normalize GitHub Actions paths
        (r"/home/runner/work/[^\s]+", "<WORKSPACE>"),
        (r"D:\\a\\[^\s]+", "<WORKSPACE>"),
        (r"C:\\hostedtoolcache\\[^\s]+", "<WORKSPACE>"),
        // Normalize content hashes that might vary across platforms
        (r"__cribo_[a-f0-9]{6,}", "__cribo_<HASH>"),
        // Normalize timestamps if any
        (r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}", "<TIMESTAMP>"),
        // Remove any remaining cargo output (should be minimal with --quiet)
        (r"(?m)^\s*Compiling [^\n]*\n", ""),
        (r"(?m)^\s*Finished [^\n]*\n", ""),
        (r"(?m)^\s*Blocking waiting for file lock[^\n]*\n", ""),
        (r"(?m)^\s*warning: [^\n]*unused manifest key[^\n]*\n", ""),
        // Normalize OS-specific error messages (keep structure, normalize message)
        (
            r"The system cannot find the file specified\. \(os error (\d+)\)",
            "No such file or directory (os error $1)",
        ),
        // Normalize Windows executable names
        (r"cribo\.exe", "cribo"),
        // Normalize line endings
        (r"\r\n", "\n"),
        (r"\r", "\n"),
        // Normalize module paths in bundled code
        (r"# Bundle from: [^\n]+", "# Bundle from: <MODULE_PATH>"),
    ]
}

#[test]
fn test_stdout_flag_help() {
    let (stdout, _, exit_code) = run_cribo(&["--help"]);

    // Should succeed
    assert_eq!(exit_code, 0);

    // Check help contains stdout flag
    assert!(stdout.contains("--stdout"));
    assert!(stdout.contains("Output bundled code to stdout instead of a file"));
}

#[test]
fn test_stdout_conflicts_with_output() {
    let (_, stderr, exit_code) = run_cribo(&[
        "--entry",
        "nonexistent.py",
        "--output",
        "output.py",
        "--stdout",
    ]);

    // Should fail
    assert_ne!(exit_code, 0);

    with_settings!({
        filters => get_cli_filters(),
    }, {
        assert_snapshot!("stdout_conflicts_with_output_stderr", stderr);
    });
}

#[test]
fn test_missing_output_and_stdout_flags() {
    let (_, stderr, exit_code) = run_cribo(&["--entry", "nonexistent.py"]);

    // Should fail
    assert_ne!(exit_code, 0);

    with_settings!({
        filters => get_cli_filters(),
    }, {
        assert_snapshot!("missing_output_and_stdout_stderr", stderr);
    });
}

#[test]
fn test_stdout_bundling_functionality() {
    let (stdout, stderr, exit_code) = run_cribo(&[
        "--entry",
        &get_fixture_path("simple_project/main.py"),
        "--stdout",
    ]);

    // Should succeed
    assert_eq!(exit_code, 0, "Command failed with stderr: {stderr}");

    with_settings!({
        filters => get_cli_filters(),
    }, {
        assert_snapshot!("stdout_bundling_output", stdout);
        assert_snapshot!("stdout_bundling_stderr", stderr);
    });

    // Ensure no log messages in stdout
    assert!(!stdout.contains("INFO"));
    assert!(!stdout.contains("WARN"));
    assert!(!stdout.contains("ERROR"));
}

#[test]
fn test_stdout_with_verbose_separation() {
    let (stdout, stderr, exit_code) = run_cribo(&[
        "--entry",
        &get_fixture_path("simple_project/main.py"),
        "--stdout",
        "-v",
    ]);

    // Should succeed
    assert_eq!(exit_code, 0);

    with_settings!({
        filters => get_cli_filters(),
    }, {
        assert_snapshot!("stdout_verbose_output", stdout);
        assert_snapshot!("stdout_verbose_stderr", stderr);
    });

    // Stdout should only contain Python code
    assert!(!stdout.contains("INFO"));
    assert!(!stdout.contains("Starting Cribo"));
}

#[test]
fn test_stdout_with_requirements() {
    let (stdout, stderr, exit_code) = run_cribo(&[
        "--entry",
        &get_fixture_path("simple_project/main.py"),
        "--stdout",
        "--emit-requirements",
    ]);

    // Should succeed
    assert_eq!(exit_code, 0);

    with_settings!({
        filters => get_cli_filters(),
    }, {
        assert_snapshot!("stdout_requirements_output", stdout);
        assert_snapshot!("stdout_requirements_stderr", stderr);
    });
}

#[test]
fn test_requirements_use_cwd_fallback_virtualenv_for_external_entry() {
    let sandbox = TempDir::new().expect("Failed to create temporary directory");
    let launch_dir = sandbox.path().join("launch");
    let entry_dir = sandbox.path().join("external");
    let output_dir = sandbox.path().join("output");
    fs::create_dir_all(&launch_dir).expect("Failed to create launch directory");
    fs::create_dir_all(&entry_dir).expect("Failed to create entry directory");
    fs::create_dir_all(&output_dir).expect("Failed to create output directory");

    let environment = launch_dir.join(".venv");
    let site_packages = if cfg!(windows) {
        environment.join("Lib").join("site-packages")
    } else {
        environment
            .join("lib")
            .join("python3.12")
            .join("site-packages")
    };
    fs::create_dir_all(site_packages.join("cwd_only_module"))
        .expect("Failed to create virtualenv package");
    fs::create_dir_all(environment.join(if cfg!(windows) { "Scripts" } else { "bin" }))
        .expect("Failed to create virtualenv executable directory");
    fs::write(
        site_packages.join("cwd_only_module").join("__init__.py"),
        "",
    )
    .expect("Failed to write virtualenv package");

    let distribution_metadata = site_packages.join("cwd_only_distribution-1.0.dist-info");
    fs::create_dir_all(&distribution_metadata).expect("Failed to create distribution metadata");
    fs::write(
        distribution_metadata.join("METADATA"),
        "Metadata-Version: 2.5\n\
         Name: cwd-only-distribution\n\
         Version: 1.0\n\
         Import-Name: cwd_only_module\n",
    )
    .expect("Failed to write distribution metadata");

    let entry_path = entry_dir.join("main.py");
    fs::write(
        &entry_path,
        "import cwd_only_module\nprint(cwd_only_module.__name__)\n",
    )
    .expect("Failed to write entry module");
    let output_path = output_dir.join("bundle.py");

    let output = run_requirement_cribo(&entry_path, &output_path, |command| {
        command.current_dir(&launch_dir).env_remove("PYTHONPATH");
    });
    assert_requirement_output(&output, &output_dir, "cwd-only-distribution");
}

#[test]
fn test_requirements_follow_pythonpath_precedence() {
    let sandbox = TempDir::new().expect("Failed to create temporary directory");
    let entry_dir = sandbox.path().join("entry");
    let first_root = sandbox.path().join("first");
    let second_root = sandbox.path().join("second");
    let output_dir = sandbox.path().join("output");
    fs::create_dir_all(&entry_dir).expect("Failed to create entry directory");
    fs::create_dir_all(&output_dir).expect("Failed to create output directory");

    write_test_distribution(
        &first_root,
        "precedence_module",
        "first-distribution",
        "SOURCE = 'first'\n",
    );
    write_test_distribution(
        &second_root,
        "precedence_module",
        "second-distribution",
        "SOURCE = 'second'\n",
    );

    let entry_path = entry_dir.join("main.py");
    fs::write(
        &entry_path,
        "import precedence_module\nprint(precedence_module.SOURCE)\n",
    )
    .expect("Failed to write entry module");
    let output_path = output_dir.join("bundle.py");
    let pythonpath =
        env::join_paths([&first_root, &second_root]).expect("Failed to construct PYTHONPATH");

    let output = run_requirement_cribo(&entry_path, &output_path, |command| {
        command.env("PYTHONPATH", pythonpath);
    });
    assert_requirement_output(&output, &output_dir, "first-distribution");
}

#[test]
fn test_stdout_mode_preserves_bundled_structure() {
    let (stdout, _, exit_code) = run_cribo(&[
        "--entry",
        &get_fixture_path("simple_project/main.py"),
        "--stdout",
    ]);

    // Should succeed
    assert_eq!(exit_code, 0);

    // The bundled structure assertions will be in the snapshot itself
    with_settings!({
        filters => get_cli_filters(),
    }, {
        assert_snapshot!("stdout_bundled_structure", stdout);
    });
}

#[test]
fn test_stdout_error_handling() {
    let (stdout, stderr, exit_code) = run_cribo(&["--entry", "nonexistent_file.py", "--stdout"]);

    // Should fail
    assert_ne!(exit_code, 0);

    with_settings!({
        filters => get_cli_filters(),
    }, {
        assert_snapshot!("stdout_error_stdout", stdout);
        assert_snapshot!("stdout_error_stderr", stderr);
    });

    // Stdout should be empty or minimal
    assert!(stdout.is_empty() || stdout.len() < 100);
}

#[test]
fn test_directory_entry_with_main_py() {
    let (stdout, _, exit_code) = run_cribo(&[
        "--entry",
        &get_fixture_path("directory_entry_main"),
        "--stdout",
    ]);

    // Should succeed
    assert_eq!(exit_code, 0);

    with_settings!({
        filters => get_cli_filters(),
    }, {
        assert_snapshot!("directory_entry_main_stdout", stdout);
    });

    // Should contain code from __init__.py, not __main__.py (prefers __init__.py)
    assert!(stdout.contains("This is __init__.py"));
    assert!(!stdout.contains("Running from __main__.py"));
}

#[test]
fn test_directory_entry_with_init_py_only() {
    let (stdout, _, exit_code) = run_cribo(&[
        "--entry",
        &get_fixture_path("directory_entry_init"),
        "--stdout",
    ]);

    // Should succeed
    assert_eq!(exit_code, 0);

    with_settings!({
        filters => get_cli_filters(),
    }, {
        assert_snapshot!("directory_entry_init_stdout", stdout);
    });

    // Should contain code from __init__.py
    assert!(stdout.contains("Running from __init__.py as fallback"));
}

#[test]
fn test_directory_entry_empty_fails() {
    let (_, stderr, exit_code) = run_cribo(&[
        "--entry",
        &get_fixture_path("directory_entry_empty"),
        "--stdout",
    ]);

    // Should fail
    assert_ne!(exit_code, 0);

    with_settings!({
        filters => get_cli_filters(),
    }, {
        assert_snapshot!("directory_entry_empty_stderr", stderr);
    });

    // Should contain appropriate error message (checks __init__.py first)
    assert!(stderr.contains("does not contain __init__.py or __main__.py"));
}
