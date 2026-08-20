//! Integration tests for `--sourcemap` CLI delivery modes.
//!
//! Each test drives the cribo binary end-to-end and inspects the emitted
//! bundle and/or `.map` file. See `docs/source-maps.md` for the design.

mod common;

use std::{fs, path::Path, process::Command};

use tempfile::TempDir;

/// Marker prefix of an inline source map comment.
const INLINE_MARKER: &str = "# sourceMappingURL=data:application/json;base64,";

/// Create a two-module fixture project and return its directory.
fn fixture_project() -> TempDir {
    let dir = TempDir::new().expect("create temp dir");
    fs::write(
        dir.path().join("main.py"),
        "from helper import greet\n\nprint(greet(\"world\"))\n",
    )
    .expect("write main.py");
    fs::write(
        dir.path().join("helper.py"),
        "def greet(name):\n    message = f\"hello {name}\"\n    return message\n",
    )
    .expect("write helper.py");
    dir
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

fn entry_arg(dir: &TempDir) -> String {
    dir.path().join("main.py").to_string_lossy().into_owned()
}

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
    let dir = TempDir::new().expect("create temp dir");
    fs::write(
        dir.path().join("main.py"),
        "from helper import boom\n\nboom()\n",
    )
    .expect("write main.py");
    fs::write(
        dir.path().join("helper.py"),
        "def boom():\n    inner()\n\ndef inner():\n    raise ValueError(\"kaboom\")\n",
    )
    .expect("write helper.py");
    dir
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
    assert!(stdout.contains("ALL 12 RUNTIME TESTS PASSED"), "{stdout}");
}

// ---------------------------------------------------------------------------
// Full hook coverage, duress conditions, and laziness
// ---------------------------------------------------------------------------

/// Create a project from (file name, content) pairs and return its directory.
fn make_project(files: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new().expect("create temp dir");
    for (name, content) in files {
        fs::write(dir.path().join(name), content).expect("write fixture file");
    }
    dir
}

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
             resource.getrlimit(resource.RLIMIT_AS)\n\
             resource.setrlimit(resource.RLIMIT_AS, (512 * 1024 * 1024, hard))\nhoard()\n",
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
fn runtime_is_lazy_on_happy_path() {
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

    // Replace the map with something that would fail loudly on ANY access:
    // garbage content and, on Unix, no read permission at all.
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
        "no map access may happen on the happy path: {stderr}"
    );
}
