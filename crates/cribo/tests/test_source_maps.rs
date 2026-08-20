//! Integration tests for `--sourcemap` CLI delivery modes.
//!
//! Each test drives the cribo binary end-to-end and inspects the emitted
//! bundle and/or `.map` file. See `docs/source-maps.md` for the design.

mod common;

use std::{fs, path::Path, process::Command};

use tempfile::TempDir;

/// Marker prefix of an inline source map comment.
const INLINE_MARKER: &str = "# sourceMappingURL=data:application/json;base64,";

/// Create a project from (file name, content) pairs and return its directory.
fn make_project(files: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new().expect("create temp dir");
    for (name, content) in files {
        fs::write(dir.path().join(name), content).expect("write fixture file");
    }
    dir
}

/// Create a two-module fixture project and return its directory.
fn fixture_project() -> TempDir {
    make_project(&[
        (
            "main.py",
            "from helper import greet\n\nprint(greet(\"world\"))\n",
        ),
        (
            "helper.py",
            "def greet(name):\n    message = f\"hello {name}\"\n    return message\n",
        ),
    ])
}

/// Run the cribo binary with `args`, returning (status success, stdout, stderr).
fn run_cribo(args: &[&str]) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_cribo"))
        .args(args)
        .output()
        .expect("run cribo binary");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Parse a source map JSON string and assert it is valid Source Map v3.
fn parse_map(json: &str) -> oxc_sourcemap::SourceMap<'_> {
    oxc_sourcemap::SourceMap::from_json_string(json).expect("valid Source Map v3 JSON")
}

/// Return the fixture entry path (`main.py`) as a CLI argument string.
fn entry_arg(dir: &TempDir) -> String {
    dir.path().join("main.py").to_string_lossy().into_owned()
}

/// Assert the map targets `bundle_file`, lists `helper.py`, and has mappings.
fn assert_map_covers_helper(map_json: &str, bundle_file: &str) {
    let map = parse_map(map_json);
    assert_eq!(map.get_file(), Some(bundle_file));
    assert!(
        map.get_sources()
            .any(|source| source.ends_with("helper.py")),
        "map sources must include helper.py: {:?}",
        map.get_sources().collect::<Vec<_>>()
    );
    assert!(map.get_tokens().count() > 0, "map must contain mappings");
}

#[test]
fn linked_mode_writes_map_and_comment() {
    let dir = fixture_project();
    let out = dir.path().join("bundle.py");
    let (ok, _, stderr) = run_cribo(&[
        "--entry",
        &entry_arg(&dir),
        "--output",
        &out.to_string_lossy(),
        "--sourcemap=linked",
    ]);
    assert!(ok, "bundling must succeed: {stderr}");

    let bundle = fs::read_to_string(&out).expect("read bundle");
    assert!(
        bundle
            .trim_end()
            .ends_with("# sourceMappingURL=bundle.py.map"),
        "linked mode must append the sourceMappingURL comment"
    );

    let map_json = fs::read_to_string(dir.path().join("bundle.py.map")).expect("read map file");
    assert_map_covers_helper(&map_json, "bundle.py");
    // Linked mode embeds sourcesContent by default.
    assert!(map_json.contains("sourcesContent"));
}

#[test]
fn bare_sourcemap_flag_defaults_to_linked() {
    let dir = fixture_project();
    let out = dir.path().join("bundle.py");
    let (ok, _, stderr) = run_cribo(&[
        "--entry",
        &entry_arg(&dir),
        "--output",
        &out.to_string_lossy(),
        "--sourcemap",
    ]);
    assert!(ok, "bundling must succeed: {stderr}");
    assert!(dir.path().join("bundle.py.map").exists());
    let bundle = fs::read_to_string(&out).expect("read bundle");
    assert!(bundle.contains("# sourceMappingURL=bundle.py.map"));
}

#[test]
fn external_mode_writes_map_without_comment() {
    let dir = fixture_project();
    let out = dir.path().join("bundle.py");
    let (ok, _, stderr) = run_cribo(&[
        "--entry",
        &entry_arg(&dir),
        "--output",
        &out.to_string_lossy(),
        "--sourcemap=external",
    ]);
    assert!(ok, "bundling must succeed: {stderr}");

    let bundle = fs::read_to_string(&out).expect("read bundle");
    assert!(
        !bundle
            .lines()
            .any(|line| line.starts_with("# sourceMappingURL=")),
        "external mode must not reference the map from the bundle"
    );
    let map_json = fs::read_to_string(dir.path().join("bundle.py.map")).expect("read map file");
    assert_map_covers_helper(&map_json, "bundle.py");
}

#[test]
fn inline_mode_embeds_map_and_writes_no_file() {
    let dir = fixture_project();
    let out = dir.path().join("bundle.py");
    let (ok, _, stderr) = run_cribo(&[
        "--entry",
        &entry_arg(&dir),
        "--output",
        &out.to_string_lossy(),
        "--sourcemap=inline",
    ]);
    assert!(ok, "bundling must succeed: {stderr}");
    assert!(
        !dir.path().join("bundle.py.map").exists(),
        "inline mode must not write a map file"
    );

    let bundle = fs::read_to_string(&out).expect("read bundle");
    let map_json = decode_inline_map(&bundle);
    assert_map_covers_helper(&map_json, "bundle.py");
    // Inline mode omits sourcesContent by default.
    assert!(!map_json.contains("sourcesContent"));
}

#[test]
fn stdout_with_bare_sourcemap_selects_inline() {
    let dir = fixture_project();
    let (ok, stdout, stderr) = run_cribo(&["--entry", &entry_arg(&dir), "--stdout", "--sourcemap"]);
    assert!(ok, "bundling must succeed: {stderr}");
    let map_json = decode_inline_map(&stdout);
    assert_map_covers_helper(&map_json, "<stdout>");
}

#[test]
fn stdout_with_linked_sourcemap_errors() {
    let dir = fixture_project();
    let (ok, _, stderr) = run_cribo(&[
        "--entry",
        &entry_arg(&dir),
        "--stdout",
        "--sourcemap=linked",
    ]);
    assert!(!ok, "linked + stdout must be rejected");
    assert!(
        stderr.contains("--sourcemap=inline"),
        "error must suggest inline mode: {stderr}"
    );
}

#[test]
fn stdout_with_external_sourcemap_errors() {
    let dir = fixture_project();
    let (ok, _, stderr) = run_cribo(&[
        "--entry",
        &entry_arg(&dir),
        "--stdout",
        "--sourcemap=external",
    ]);
    assert!(!ok, "external + stdout must be rejected");
    assert!(stderr.contains("--sourcemap=inline"));
}

#[test]
fn no_sourcemap_by_default() {
    let dir = fixture_project();
    let out = dir.path().join("bundle.py");
    let (ok, _, stderr) = run_cribo(&[
        "--entry",
        &entry_arg(&dir),
        "--output",
        &out.to_string_lossy(),
    ]);
    assert!(ok, "bundling must succeed: {stderr}");
    assert!(!dir.path().join("bundle.py.map").exists());
    let bundle = fs::read_to_string(&out).expect("read bundle");
    assert!(!bundle.contains("sourceMappingURL"));
}

#[test]
fn config_file_sourcemap_key_is_honored() {
    let dir = fixture_project();
    fs::write(dir.path().join("cribo.toml"), "sourcemap = \"external\"\n").expect("write config");
    let out = dir.path().join("bundle.py");
    let (ok, _, stderr) = run_cribo(&[
        "--entry",
        &entry_arg(&dir),
        "--output",
        &out.to_string_lossy(),
        "--config",
        &dir.path().join("cribo.toml").to_string_lossy(),
    ]);
    assert!(ok, "bundling must succeed: {stderr}");
    assert!(
        dir.path().join("bundle.py.map").exists(),
        "config-file sourcemap key must enable map emission"
    );
    let bundle = fs::read_to_string(&out).expect("read bundle");
    assert!(
        !bundle
            .lines()
            .any(|line| line.starts_with("# sourceMappingURL=")),
        "external mode: no comment"
    );
}

/// Extract and decode the inline source map data URL from bundle text.
fn decode_inline_map(bundle: &str) -> String {
    let marker_pos = bundle
        .rfind(INLINE_MARKER)
        .expect("inline source map comment present");
    let payload = bundle[marker_pos + INLINE_MARKER.len()..].trim_end();
    let bytes = base64_simd::STANDARD
        .decode_to_vec(payload.as_bytes())
        .expect("valid base64 payload");
    String::from_utf8(bytes).expect("valid UTF-8 source map")
}

/// The map file sits next to the bundle even when the output path is nested.
#[test]
fn linked_map_lands_next_to_nested_output() {
    let dir = fixture_project();
    let nested = dir.path().join("dist").join("app.py");
    fs::create_dir_all(nested.parent().expect("parent")).expect("mkdir dist");
    let (ok, _, stderr) = run_cribo(&[
        "--entry",
        &entry_arg(&dir),
        "--output",
        &nested.to_string_lossy(),
        "--sourcemap=linked",
    ]);
    assert!(ok, "bundling must succeed: {stderr}");
    let map_path = dir.path().join("dist").join("app.py.map");
    assert!(map_path.exists(), "map must sit next to the nested output");
    let bundle = fs::read_to_string(&nested).expect("read bundle");
    assert!(bundle.contains("# sourceMappingURL=app.py.map"));

    // Source paths must be relative to the map's directory.
    let map_json = fs::read_to_string(&map_path).expect("read map");
    let map = parse_map(&map_json);
    assert!(
        map.get_sources()
            .all(|source| Path::new(source).is_relative()),
        "sources must be relative paths: {:?}",
        map.get_sources().collect::<Vec<_>>()
    );
}

#[test]
fn sources_content_can_be_forced_on_for_inline() {
    let dir = fixture_project();
    let out = dir.path().join("bundle.py");
    let (ok, _, stderr) = run_cribo(&[
        "--entry",
        &entry_arg(&dir),
        "--output",
        &out.to_string_lossy(),
        "--sourcemap=inline",
        "--sources-content=true",
    ]);
    assert!(ok, "bundling must succeed: {stderr}");
    let bundle = fs::read_to_string(&out).expect("read bundle");
    let map_json = decode_inline_map(&bundle);
    assert!(
        map_json.contains("sourcesContent"),
        "--sources-content=true must force embedding for inline maps"
    );
    let map = parse_map(&map_json);
    assert!(
        map.get_source_contents()
            .flatten()
            .any(|content| content.contains("def greet")),
        "embedded content must carry the original helper source"
    );
}

#[test]
fn sources_content_can_be_forced_off_for_linked() {
    let dir = fixture_project();
    let out = dir.path().join("bundle.py");
    let (ok, _, stderr) = run_cribo(&[
        "--entry",
        &entry_arg(&dir),
        "--output",
        &out.to_string_lossy(),
        "--sourcemap=linked",
        "--sources-content=false",
    ]);
    assert!(ok, "bundling must succeed: {stderr}");
    let map_json = fs::read_to_string(dir.path().join("bundle.py.map")).expect("read map");
    assert!(
        !map_json.contains("sourcesContent"),
        "--sources-content=false must strip embedding for linked maps"
    );
}

#[test]
fn config_file_sources_content_key_is_honored() {
    let dir = fixture_project();
    fs::write(
        dir.path().join("cribo.toml"),
        "sourcemap = \"external\"\nsources-content = false\n",
    )
    .expect("write config");
    let out = dir.path().join("bundle.py");
    let (ok, _, stderr) = run_cribo(&[
        "--entry",
        &entry_arg(&dir),
        "--output",
        &out.to_string_lossy(),
        "--config",
        &dir.path().join("cribo.toml").to_string_lossy(),
    ]);
    assert!(ok, "bundling must succeed: {stderr}");
    let map_json = fs::read_to_string(dir.path().join("bundle.py.map")).expect("read map");
    assert!(!map_json.contains("sourcesContent"));
}

// ---------------------------------------------------------------------------
// Runtime traceback remapping (injected prologue)
// ---------------------------------------------------------------------------

/// Create a fixture whose entry crashes two calls deep inside helper.py.
fn crash_project() -> TempDir {
    make_project(&[
        ("main.py", "from helper import boom\n\nboom()\n"),
        (
            "helper.py",
            "def boom():\n    inner()\n\ndef inner():\n    raise ValueError(\"kaboom\")\n",
        ),
    ])
}

/// Bundle the crash project with the given sourcemap argument; return bundle path.
fn bundle_crash_project(dir: &TempDir, sourcemap_arg: &str) -> std::path::PathBuf {
    let out = dir.path().join("bundle.py");
    let (ok, _, stderr) = run_cribo(&[
        "--entry",
        &entry_arg(dir),
        "--output",
        &out.to_string_lossy(),
        sourcemap_arg,
    ]);
    assert!(ok, "bundling must succeed: {stderr}");
    out
}

/// Run a Python file; returns (status success, stdout, stderr).
fn run_python(bundle: &Path, envs: &[(&str, &str)]) -> (bool, String, String) {
    let mut command = Command::new(common::get_python_executable());
    command.arg(bundle);
    // The env-gating tests require a clean slate; an explicit entry below wins.
    command.env_remove("CRIBO_SOURCE_MAPS");
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command.output().expect("run python");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Assert stderr shows the remapped traceback pointing at original files.
fn assert_remapped(stderr: &str) {
    assert!(
        stderr.contains("helper.py\", line 5, in inner"),
        "traceback must point at helper.py:5: {stderr}"
    );
    assert!(
        stderr.contains("raise ValueError(\"kaboom\")"),
        "traceback must show the original source line: {stderr}"
    );
    assert!(
        stderr.contains("main.py\", line 3, in <module>"),
        "traceback must point at main.py:3: {stderr}"
    );
    assert!(
        !stderr.contains("bundle.py\", line"),
        "no frame should remain on bundle coordinates: {stderr}"
    );
}

/// Assert stderr is one single standard (non-remapped) traceback with no
/// runtime noise.
fn assert_standard_traceback(stderr: &str) {
    assert_eq!(
        stderr.matches("Traceback (most recent call last):").count(),
        1,
        "exactly one traceback expected: {stderr}"
    );
    assert!(
        stderr.contains("bundle.py\", line"),
        "standard traceback must show bundle coordinates: {stderr}"
    );
    assert!(
        !stderr.contains("helper.py\", line 5"),
        "no remapping must happen: {stderr}"
    );
}

#[test]
fn runtime_remaps_linked_crash() {
    let dir = crash_project();
    let bundle = bundle_crash_project(&dir, "--sourcemap=linked");
    let (ok, _, stderr) = run_python(&bundle, &[]);
    assert!(!ok, "the crashing bundle must exit non-zero");
    assert_remapped(&stderr);
}

#[test]
fn runtime_disabled_when_linked_map_missing() {
    let dir = crash_project();
    let bundle = bundle_crash_project(&dir, "--sourcemap=linked");
    fs::remove_file(dir.path().join("bundle.py.map")).expect("delete map");
    let (ok, _, stderr) = run_python(&bundle, &[]);
    assert!(!ok);
    assert_standard_traceback(&stderr);
}

#[test]
fn runtime_remaps_inline_crash() {
    let dir = crash_project();
    let bundle = bundle_crash_project(&dir, "--sourcemap=inline");
    let (ok, _, stderr) = run_python(&bundle, &[]);
    assert!(!ok);
    assert_remapped(&stderr);
}

#[test]
fn runtime_kill_switch_disables_remapping() {
    let dir = crash_project();
    let bundle = bundle_crash_project(&dir, "--sourcemap=inline");
    let (ok, _, stderr) = run_python(&bundle, &[("CRIBO_SOURCE_MAPS", "0")]);
    assert!(!ok);
    assert_standard_traceback(&stderr);
}

#[test]
fn runtime_external_mode_is_env_gated() {
    let dir = crash_project();
    let bundle = bundle_crash_project(&dir, "--sourcemap=external");

    // Without the env var the runtime stays dormant.
    let (ok, _, stderr) = run_python(&bundle, &[]);
    assert!(!ok);
    assert_standard_traceback(&stderr);

    // CRIBO_SOURCE_MAPS=1 activates it against the sibling map.
    let (ok, _, stderr) = run_python(&bundle, &[("CRIBO_SOURCE_MAPS", "1")]);
    assert!(!ok);
    assert_remapped(&stderr);

    // The env var can also point directly at a relocated map file.
    let moved = dir.path().join("elsewhere.map");
    fs::rename(dir.path().join("bundle.py.map"), &moved).expect("move map");
    let (ok, _, stderr) = run_python(
        &bundle,
        &[("CRIBO_SOURCE_MAPS", moved.to_string_lossy().as_ref())],
    );
    assert!(!ok);
    assert_remapped(&stderr);

    // With the map moved away, =1 finds nothing and stays silent.
    let (ok, _, stderr) = run_python(&bundle, &[("CRIBO_SOURCE_MAPS", "1")]);
    assert!(!ok);
    assert_standard_traceback(&stderr);
}

/// Drive the pure-Python unit tests for the runtime internals (VLQ machine,
/// JSON scanner, backward EOF scan, base64 chunk alignment, json fallback).
#[test]
fn python_runtime_unit_tests() {
    use cow_utils::CowUtils as _;

    const TEMPLATE: &str = include_str!("../src/python/sourcemap_runtime.py");
    let dir = TempDir::new().expect("create temp dir");
    let runtime_path = dir.path().join("runtime.py");
    fs::write(
        &runtime_path,
        TEMPLATE
            .cow_replace("__CRIBO_SOURCEMAP_MODE__", "external")
            .as_ref(),
    )
    .expect("write substituted runtime");

    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("python")
        .join("test_sourcemap_runtime.py");
    let output = Command::new(common::get_python_executable())
        .arg(&script)
        .arg(&runtime_path)
        .output()
        .expect("run python unit tests");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "python runtime unit tests failed:\n{stdout}\n{stderr}"
    );
    // The harness discovers its tests and reports the count itself; assert the
    // sentinel plus a sanity floor instead of duplicating the exact count here.
    assert!(stdout.contains("RUNTIME TESTS PASSED"), "{stdout}");
    assert!(
        stdout.matches("PASS test_").count() >= 10,
        "expected a healthy number of runtime unit tests: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// Full hook coverage, duress conditions, and laziness
// ---------------------------------------------------------------------------

#[test]
fn runtime_remaps_thread_crash() {
    let dir = make_project(&[
        (
            "main.py",
            "import threading\nfrom helper import boom\n\nworker = \
             threading.Thread(target=boom)\nworker.start()\nworker.join()\nprint(\"done\")\n",
        ),
        (
            "helper.py",
            "def boom():\n    inner()\n\ndef inner():\n    raise ValueError(\"thread kaboom\")\n",
        ),
    ]);
    let bundle = bundle_crash_project(&dir, "--sourcemap=linked");
    // An uncaught exception in a non-main thread does not fail the process.
    let (ok, stdout, stderr) = run_python(&bundle, &[]);
    assert!(ok, "main thread must finish normally: {stderr}");
    assert!(stdout.contains("done"));
    assert!(
        stderr.contains("Exception in thread"),
        "threading hook must announce the thread: {stderr}"
    );
    assert!(
        stderr.contains("helper.py\", line 5, in inner"),
        "thread traceback must be remapped: {stderr}"
    );
    assert!(stderr.contains("thread kaboom"));
}

#[test]
fn runtime_remaps_unraisable_error() {
    let dir = make_project(&[
        (
            "main.py",
            "from helper import make\n\nobj = make()\ndel obj\nprint(\"done\")\n",
        ),
        (
            "helper.py",
            "class Cursed:\n    def __del__(self):\n        raise RuntimeError(\"del \
             failed\")\n\ndef make():\n    return Cursed()\n",
        ),
    ]);
    let bundle = bundle_crash_project(&dir, "--sourcemap=linked");
    let (ok, stdout, stderr) = run_python(&bundle, &[]);
    assert!(ok, "unraisable errors must not fail the process: {stderr}");
    assert!(stdout.contains("done"));
    assert!(
        stderr.contains("Exception ignored in"),
        "unraisable hook must keep the standard preamble: {stderr}"
    );
    assert!(
        stderr.contains("helper.py\", line 3, in __del__"),
        "unraisable traceback must be remapped: {stderr}"
    );
}

#[test]
fn runtime_survives_recursion_error() {
    let dir = make_project(&[
        ("main.py", "from helper import spiral\n\nspiral()\n"),
        ("helper.py", "def spiral():\n    spiral()\n"),
    ]);
    let bundle = bundle_crash_project(&dir, "--sourcemap=linked");
    let (ok, _, stderr) = run_python(&bundle, &[]);
    assert!(!ok);
    assert!(stderr.contains("RecursionError"), "{stderr}");
    assert!(
        stderr.contains("helper.py\", line 2, in spiral"),
        "recursion frames must be remapped: {stderr}"
    );
    assert!(
        stderr.contains("[Previous line repeated"),
        "repeated recursion frames must be collapsed: {stderr}"
    );
    // Collapsing keeps the output small even for ~1000 recorded frames.
    assert!(
        stderr.lines().count() < 60,
        "collapsed traceback expected, got {} lines",
        stderr.lines().count()
    );
}

#[cfg(unix)]
#[test]
fn runtime_survives_memory_pressure() {
    let dir = make_project(&[
        (
            "main.py",
            "import resource\nfrom helper import hoard\n\n_soft, hard = \
             resource.getrlimit(resource.RLIMIT_AS)\ntarget = 512 * 1024 * 1024\nif hard != \
             resource.RLIM_INFINITY:\n    target = min(target, hard)\n\
             resource.setrlimit(resource.RLIMIT_AS, (target, hard))\nhoard()\n",
        ),
        (
            "helper.py",
            "def hoard():\n    blocks = []\n    try:\n        while True:\n            \
             blocks.append(bytearray(16 * 1024 * 1024))\n    except MemoryError:\n        \
             blocks.clear()\n        raise MemoryError(\"exhausted\") from None\n",
        ),
    ]);
    let bundle = bundle_crash_project(&dir, "--sourcemap=linked");
    let (ok, _, stderr) = run_python(&bundle, &[]);
    assert!(!ok);
    assert!(stderr.contains("MemoryError"), "{stderr}");
    // The runtime must never produce a secondary error, whichever path it took.
    assert!(
        !stderr.contains("Error in sys.excepthook"),
        "the hook must not raise: {stderr}"
    );
    assert!(
        stderr.contains("helper.py\", line 8, in hoard"),
        "MemoryError under an address-space limit should still remap: {stderr}"
    );
}

#[cfg(unix)]
#[test]
fn runtime_falls_back_cleanly_on_fd_exhaustion() {
    let dir = make_project(&[
        (
            "main.py",
            "from helper import consume_fds_and_boom\n\nconsume_fds_and_boom()\n",
        ),
        (
            "helper.py",
            "import resource\n\ndef consume_fds_and_boom():\n    _soft, hard = \
             resource.getrlimit(resource.RLIMIT_NOFILE)\n    \
             resource.setrlimit(resource.RLIMIT_NOFILE, (16, hard))\n    holders = []\n    \
             try:\n        while True:\n            holders.append(open(\"/dev/null\", \
             \"rb\"))\n    except OSError:\n        pass\n    raise ValueError(\"fd exhausted \
             kaboom\")\n",
        ),
    ]);
    let bundle = bundle_crash_project(&dir, "--sourcemap=linked");
    let (ok, _, stderr) = run_python(&bundle, &[]);
    assert!(!ok);
    // The map cannot be opened, so the runtime must fall back to the default
    // traceback without any secondary noise.
    assert!(stderr.contains("fd exhausted kaboom"), "{stderr}");
    assert_eq!(
        stderr.matches("Traceback (most recent call last):").count(),
        1,
        "exactly one traceback expected: {stderr}"
    );
    assert!(
        !stderr.contains("Error in sys.excepthook"),
        "the hook must not raise: {stderr}"
    );
    assert!(
        !stderr.contains("helper.py\", line"),
        "with the map unreadable, frames stay on bundle coordinates: {stderr}"
    );
}

#[test]
fn runtime_tolerates_broken_map_on_happy_path() {
    let dir = fixture_project(); // non-throwing project
    let out = dir.path().join("bundle.py");
    let (ok, _, stderr) = run_cribo(&[
        "--entry",
        &entry_arg(&dir),
        "--output",
        &out.to_string_lossy(),
        "--sourcemap=linked",
    ]);
    assert!(ok, "bundling must succeed: {stderr}");

    // Replace the map with garbage (and drop read permission on Unix, though
    // that is a no-op when running as root). The runtime is fail-open, so this
    // cannot *prove* the map is never touched — laziness itself is enforced by
    // the runtime design (all map access lives behind the hook path). What it
    // proves is that a broken or unreadable map never disturbs a successful run.
    let map_path = dir.path().join("bundle.py.map");
    fs::write(&map_path, "NOT JSON {{{").expect("overwrite map");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&map_path, fs::Permissions::from_mode(0o000))
            .expect("make map unreadable");
    }

    let (ok, stdout, stderr) = run_python(&out, &[]);
    assert!(ok, "happy-path run must succeed: {stderr}");
    assert!(stdout.contains("hello world"));
    assert!(
        stderr.is_empty(),
        "a broken map must not disturb a successful run: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Precedence, environment configuration, and hook-chaining behavior
// ---------------------------------------------------------------------------

#[test]
fn cli_sourcemap_flag_overrides_config_file() {
    let dir = fixture_project();
    fs::write(dir.path().join("cribo.toml"), "sourcemap = \"external\"\n").expect("write config");
    let out = dir.path().join("bundle.py");
    let (ok, _, stderr) = run_cribo(&[
        "--entry",
        &entry_arg(&dir),
        "--output",
        &out.to_string_lossy(),
        "--config",
        &dir.path().join("cribo.toml").to_string_lossy(),
        "--sourcemap=linked",
    ]);
    assert!(ok, "bundling must succeed: {stderr}");
    let bundle = fs::read_to_string(&out).expect("read bundle");
    assert!(
        bundle.contains("# sourceMappingURL=bundle.py.map"),
        "CLI --sourcemap=linked must override the config file's external mode"
    );
}

#[test]
fn env_var_enables_sourcemap_generation() {
    let dir = fixture_project();
    let out = dir.path().join("bundle.py");
    let output = Command::new(env!("CARGO_BIN_EXE_cribo"))
        .args([
            "--entry",
            &entry_arg(&dir),
            "--output",
            &out.to_string_lossy(),
        ])
        .env("CRIBO_SOURCEMAP", "external")
        .env("CRIBO_SOURCES_CONTENT", "false")
        .output()
        .expect("run cribo binary");
    assert!(
        output.status.success(),
        "bundling must succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let map_json = fs::read_to_string(dir.path().join("bundle.py.map"))
        .expect("CRIBO_SOURCEMAP env var must enable map emission");
    assert_map_covers_helper(&map_json, "bundle.py");
    assert!(
        !map_json.contains("sourcesContent"),
        "CRIBO_SOURCES_CONTENT=false must strip embedding"
    );
}

#[test]
fn runtime_keeps_thread_sys_exit_silent() {
    let dir = make_project(&[
        (
            "main.py",
            "import sys\nimport threading\n\nworker = threading.Thread(target=lambda: \
             sys.exit(3))\nworker.start()\nworker.join()\nprint(\"done\")\n",
        ),
        ("helper.py", "unused = True\n"),
    ]);
    let out = dir.path().join("bundle.py");
    let (ok, _, stderr) = run_cribo(&[
        "--entry",
        &entry_arg(&dir),
        "--output",
        &out.to_string_lossy(),
        "--sourcemap=linked",
    ]);
    assert!(ok, "bundling must succeed: {stderr}");
    let (ok, stdout, stderr) = run_python(&out, &[]);
    assert!(
        ok,
        "sys.exit in a worker thread must not fail the process: {stderr}"
    );
    assert!(stdout.contains("done"));
    assert!(
        stderr.is_empty(),
        "SystemExit in a thread must stay silent, as with the default hook: {stderr}"
    );
}

#[test]
fn runtime_notifies_preinstalled_custom_excepthook() {
    // A custom excepthook installed before the bundle's prologue (via
    // sitecustomize) must still observe the exception after a successful remap.
    let dir = crash_project();
    fs::write(
        dir.path().join("sitecustomize.py"),
        "import sys\n\n_original = sys.excepthook\n\n\ndef reporting_hook(exc_type, exc_value, \
         tb):\n    print(\"REPORTER SAW:\", exc_type.__name__, file=sys.stderr)\n\n\nsys.excepthook \
         = reporting_hook\n",
    )
    .expect("write sitecustomize");
    let bundle = bundle_crash_project(&dir, "--sourcemap=linked");

    let mut command = Command::new(common::get_python_executable());
    command.arg(&bundle);
    command.env_remove("CRIBO_SOURCE_MAPS");
    // Make usercustomize importable so the custom hook installs before the bundle.
    command.env("PYTHONPATH", dir.path());
    let output = command.output().expect("run python");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert_remapped(&stderr);
    assert!(
        stderr.contains("REPORTER SAW: ValueError"),
        "the preinstalled custom hook must still be notified after a remap: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Second review round: shadowing, docstring, tracebacklimit, notes, handlers
// ---------------------------------------------------------------------------

#[test]
fn runtime_survives_shadowing_threading_module() {
    // A project file named threading.py next to the bundle must not be able
    // to break (or be imported by) the runtime's own stdlib imports.
    let dir = crash_project();
    let bundle = bundle_crash_project(&dir, "--sourcemap=linked");
    fs::write(
        dir.path().join("threading.py"),
        "raise RuntimeError(\"shadow module imported\")\n",
    )
    .expect("write shadowing module");

    let (ok, _, stderr) = run_python(&bundle, &[]);
    assert!(!ok, "the crash must still surface");
    assert!(
        !stderr.contains("shadow module imported"),
        "the runtime must not import the adjacent threading.py: {stderr}"
    );
    assert_remapped(&stderr);
}

#[test]
fn bundle_docstring_survives_runtime_injection() {
    let dir = make_project(&[
        ("main.py", "\"\"\"Entry doc.\"\"\"\n\nprint(__doc__)\n"),
        ("helper.py", "unused = True\n"),
    ]);
    let out = dir.path().join("bundle.py");
    let (ok, _, stderr) = run_cribo(&[
        "--entry",
        &entry_arg(&dir),
        "--output",
        &out.to_string_lossy(),
        "--sourcemap=inline",
    ]);
    assert!(ok, "bundling must succeed: {stderr}");
    let (ok, stdout, stderr) = run_python(&out, &[]);
    assert!(ok, "bundle must run: {stderr}");
    assert!(
        stdout.contains("Entry doc."),
        "the injected runtime must not displace the bundle docstring: {stdout}"
    );
}

#[test]
fn runtime_honors_tracebacklimit() {
    let dir = make_project(&[
        (
            "main.py",
            // Set the limit on the real sys module: the bundler's stdlib
            // import proxy forwards attribute reads but not writes, so a
            // plain `sys.tracebacklimit = 0` would only decorate the proxy
            // (true for bundles with or without source maps).
            "from helper import boom\n\n__import__(\"sys\").tracebacklimit = 0\nboom()\n",
        ),
        (
            "helper.py",
            "def boom():\n    raise ValueError(\"limited kaboom\")\n",
        ),
    ]);
    let bundle = bundle_crash_project(&dir, "--sourcemap=linked");
    let (ok, _, stderr) = run_python(&bundle, &[]);
    assert!(!ok);
    assert!(stderr.contains("ValueError: limited kaboom"), "{stderr}");
    assert!(
        !stderr.contains("Traceback (most recent call last):"),
        "tracebacklimit = 0 must suppress the frame listing: {stderr}"
    );
    assert!(
        !stderr.contains("File \""),
        "tracebacklimit = 0 must suppress all frames: {stderr}"
    );
}

#[test]
fn runtime_renders_exception_notes() {
    let dir = make_project(&[
        ("main.py", "from helper import boom\n\nboom()\n"),
        (
            "helper.py",
            "def boom():\n    error = ValueError(\"kaboom\")\n    error.add_note(\"NOTE: check \
             the flux capacitor\")\n    raise error\n",
        ),
    ]);
    let bundle = bundle_crash_project(&dir, "--sourcemap=linked");
    let (ok, _, stderr) = run_python(&bundle, &[]);
    assert!(!ok);
    assert!(
        stderr.contains("helper.py\", line 4, in boom"),
        "traceback must be remapped: {stderr}"
    );
    assert!(
        stderr.contains("NOTE: check the flux capacitor"),
        "__notes__ must survive remapped rendering: {stderr}"
    );
}

#[test]
fn map_covers_elif_and_except_headers() {
    let dir = make_project(&[
        (
            "main.py",
            "from helper import classify\n\nprint(classify(2))\n",
        ),
        (
            "helper.py",
            "def classify(value):\n    if value == 0:\n        return \"zero\"\n    elif value == \
             1:\n        return \"one\"\n    elif value == 2:\n        return \"two\"\n    \
             try:\n        return int(value)\n    except ValueError:\n        return \"other\"\n",
        ),
    ]);
    let out = dir.path().join("bundle.py");
    let (ok, _, stderr) = run_cribo(&[
        "--entry",
        &entry_arg(&dir),
        "--output",
        &out.to_string_lossy(),
        "--sourcemap=linked",
    ]);
    assert!(ok, "bundling must succeed: {stderr}");
    let map_json = fs::read_to_string(dir.path().join("bundle.py.map")).expect("read map");
    let map = parse_map(&map_json);

    let helper_id = (0..map.get_sources().count() as u32)
        .find(|id| {
            map.get_source(*id)
                .is_some_and(|s| s.ends_with("helper.py"))
        })
        .expect("helper.py in sources");
    let mapped_helper_lines: Vec<u32> = map
        .get_tokens()
        .filter(|token| token.get_source_id() == Some(helper_id))
        .map(|token| token.get_src_line())
        .collect();
    // 0-based original lines: 3 and 5 are the `elif` headers, 9 is `except ValueError:`.
    for header_line in [3, 5, 9] {
        assert!(
            mapped_helper_lines.contains(&header_line),
            "helper.py 0-based line {header_line} (a clause header) must be mapped; mapped \
             lines: {mapped_helper_lines:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Third review round: specialized formatting, long chains, header coverage
// ---------------------------------------------------------------------------

#[test]
fn runtime_keeps_name_error_suggestions() {
    let dir = make_project(&[
        ("main.py", "from helper import go\n\ngo()\n"),
        (
            "helper.py",
            "def go():\n    valuable = 1\n    return valuabl\n",
        ),
    ]);
    let bundle = bundle_crash_project(&dir, "--sourcemap=linked");
    let (ok, _, stderr) = run_python(&bundle, &[]);
    assert!(!ok);
    assert!(
        stderr.contains("helper.py\", line 3, in go"),
        "traceback must be remapped: {stderr}"
    );
    assert!(stderr.contains("NameError"), "{stderr}");
    assert!(
        stderr.contains("Did you mean"),
        "interpreter suggestions must survive remapped rendering: {stderr}"
    );
}

#[test]
fn runtime_renders_long_exception_chains_fully() {
    // 20 chained causes exceed the previous traversal cap; the innermost
    // (root) exception and its traceback must still be rendered.
    let dir = make_project(&[
        ("main.py", "from helper import cascade\n\ncascade()\n"),
        (
            "helper.py",
            "def cascade():\n    try:\n        raise ValueError(\"root kaboom\")\n    except \
             ValueError as error:\n        current = error\n        for depth in range(20):\n            \
             try:\n                raise RuntimeError(\"layer %d\" % depth) from \
             current\n            except RuntimeError as next_error:\n                current = \
             next_error\n        raise current\n",
        ),
    ]);
    let bundle = bundle_crash_project(&dir, "--sourcemap=linked");
    let (ok, _, stderr) = run_python(&bundle, &[]);
    assert!(!ok);
    assert!(
        stderr.contains("ValueError: root kaboom"),
        "the root cause of a 20-deep chain must be rendered: {stderr}"
    );
    assert!(stderr.contains("layer 19"), "{stderr}");
    assert!(
        stderr.contains("helper.py\", line 3"),
        "the root cause frame must be remapped: {stderr}"
    );
}

#[test]
fn map_covers_match_case_headers_and_decorators() {
    let dir = make_project(&[
        ("main.py", "from helper import run\n\nprint(run(1))\n"),
        (
            "helper.py",
            "def trace(func):\n    return func\n\n\n@trace\n@trace\ndef run(value):\n    match \
             value:\n        case 0:\n            return \"zero\"\n        case _ if value > \
             0:\n            return \"positive\"\n        case _:\n            return \
             \"negative\"\n",
        ),
    ]);
    let out = dir.path().join("bundle.py");
    let (ok, _, stderr) = run_cribo(&[
        "--entry",
        &entry_arg(&dir),
        "--output",
        &out.to_string_lossy(),
        "--sourcemap=linked",
    ]);
    assert!(ok, "bundling must succeed: {stderr}");
    let map_json = fs::read_to_string(dir.path().join("bundle.py.map")).expect("read map");
    let map = parse_map(&map_json);

    let helper_id = (0..map.get_sources().count() as u32)
        .find(|id| {
            map.get_source(*id)
                .is_some_and(|s| s.ends_with("helper.py"))
        })
        .expect("helper.py in sources");
    let mapped_helper_lines: Vec<u32> = map
        .get_tokens()
        .filter(|token| token.get_source_id() == Some(helper_id))
        .map(|token| token.get_src_line())
        .collect();
    // 0-based original lines: 4 and 5 are the two decorators; 8, 10, and 12
    // are the `case` headers.
    for header_line in [4, 5, 8, 10, 12] {
        assert!(
            mapped_helper_lines.contains(&header_line),
            "helper.py 0-based line {header_line} (decorator or case header) must be mapped; \
             mapped lines: {mapped_helper_lines:?}"
        );
    }
}
