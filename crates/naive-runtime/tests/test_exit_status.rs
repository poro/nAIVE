//! Exercise the actual CLI so discovery and engine failures cannot report green.

use std::path::PathBuf;
use std::process::{Command, Output};

struct Project(PathBuf);

impl Project {
    fn new(config: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("naive-cli-{}-{id}", std::process::id()));
        std::fs::create_dir_all(root.join("tests")).unwrap();
        let project = Self(root);
        project.write(
            "naive.yaml",
            &format!("name: Regression\nversion: '1'\n{config}"),
        );
        project
    }

    fn write(&self, path: &str, content: &str) {
        std::fs::write(self.0.join(path), content).unwrap();
    }

    fn test(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_naive"))
            .current_dir(&self.0)
            .arg("test")
            .args(args)
            .output()
            .unwrap()
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn assert_failure(output: Output, expected: &str) {
    assert!(!output.status.success(), "CLI unexpectedly passed");
    let message = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        message.contains(expected),
        "expected {expected:?}: {message}"
    );
}

#[test]
fn missing_explicit_and_configured_tests_exit_nonzero() {
    let project = Project::new("test:\n  files: [tests/missing.lua]\n");
    assert_failure(project.test(&[]), "Configured test file not found");
    assert_failure(project.test(&["tests/missing.lua"]), "Test file not found");
}

#[test]
fn missing_or_empty_test_directories_exit_nonzero() {
    let project = Project::new("test:\n  directory: absent\n");
    assert_failure(project.test(&[]), "Test directory not found");
    let project = Project::new("");
    assert_failure(project.test(&[]), "No test_*.lua files found");
    let project = Project::new("test:\n  files: []\n");
    assert_failure(project.test(&[]), "Configured test file list is empty");
}

#[test]
fn configured_test_paths_cannot_escape_the_project() {
    let project = Project::new("test:\n  files: [../test_escape.lua]\n");
    assert_failure(project.test(&[]), "contain no '..'");
    assert_failure(project.test(&["../test_escape.lua"]), "contain no '..'");
}

#[test]
fn game_script_failures_exit_nonzero_and_clean_scripts_pass() {
    let project = Project::new("");
    project.write("scene.yaml", "name: Regression\nentities:\n  - id: actor\n    components:\n      transform: {}\n      script: { source: game.lua }\n");
    project.write(
        "tests/test_game.lua",
        "function test_game()\n  pcall(scene.load, 'scene.yaml')\n  wait_frames(2)\nend\n",
    );
    for (source, expected) in [
        ("function broken(", "Failed to load script"),
        ("function init() error('init failure') end", "init failure"),
        (
            "function update(dt) error('update failure') end",
            "update failure",
        ),
    ] {
        project.write("game.lua", source);
        assert_failure(project.test(&[]), expected);
    }
    project.write(
        "game.lua",
        "function update(dt) game.frames = (game.frames or 0) + 1 end",
    );
    let result = project.test(&[]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
}

#[test]
fn discovery_and_test_body_loops_fail_with_a_bounded_exit() {
    let project = Project::new("");
    for source in [
        "while true do end",
        "function test_loop() while true do end end",
        "function test_loop() while true do pcall(function() while true do end end) end end",
    ] {
        project.write("tests/test_loop.lua", source);
        let started = std::time::Instant::now();
        assert_failure(project.test(&["--timeout-seconds", "2"]), "limit");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }
}

#[test]
fn suite_deadline_kills_rust_waits_and_results_survive_lua_print_output() {
    let project = Project::new("");
    project.write(
        "tests/test_wait.lua",
        "function test_wait() wait_frames(1000000000000) end",
    );
    let started = std::time::Instant::now();
    assert_failure(project.test(&["--timeout-seconds", "1"]), "timed out");
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    project.write(
        "tests/test_wait.lua",
        "print('not worker JSON'); function test_ok() log.info('kept test output') end",
    );
    let output = project.test(&[]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("kept test output"));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout)
            .matches("OK test_ok")
            .count(),
        1
    );
    project.write(
        "tests/test_wait.lua",
        "function test_output() print(string.rep('x', 2 * 1024 * 1024)) end",
    );
    assert_failure(project.test(&[]), "output exceeded");
    assert_failure(project.test(&["--timeout-seconds", "0"]), "between 1 and");
}
