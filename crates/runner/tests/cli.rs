use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn version_flag_prints_bare_semver() {
    Command::cargo_bin("ryuzi")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"^\d+\.\d+\.\d+\n$").unwrap());
    Command::cargo_bin("ryuzi")
        .unwrap()
        .arg("-v")
        .assert()
        .success();
}

#[test]
fn unknown_command_exits_1_with_hint() {
    Command::cargo_bin("ryuzi")
        .unwrap()
        .arg("bogus")
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "unknown command: bogus - run `ryuzi --help`",
        ));
}

#[test]
// No-args always prints help: the TUI was removed with the CLI product.
fn help_flag_and_bare_help_and_no_args_print_usage() {
    for args in [vec!["--help"], vec!["-h"], vec!["help"], vec![]] {
        Command::cargo_bin("ryuzi")
            .unwrap()
            .args(&args)
            .assert()
            .success()
            .stdout(predicate::str::contains("USAGE").and(predicate::str::contains("doctor")));
    }
}

#[test]
fn doctor_prints_three_report_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let assert = Command::cargo_bin("ryuzi")
        .unwrap()
        .arg("doctor")
        .env("XDG_DATA_HOME", tmp.path())
        .env("HOME", tmp.path())
        .assert();
    let output = assert.get_output().clone();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        3,
        "doctor must print exactly 3 lines, got: {stdout}"
    );
    assert!(lines[0].starts_with("git:    "), "line 1: {}", lines[0]);
    assert!(lines[1].starts_with("settings: "), "line 2: {}", lines[1]);
    assert!(lines[2].starts_with("doctor: "), "line 3: {}", lines[2]);
    // Exit code must agree with the verdict line (environment-tolerant:
    // a fresh DB always has missing settings, so FAIL is expected here).
    let code = output.status.code().unwrap();
    if lines[2] == "doctor: PASS" {
        assert_eq!(code, 0);
    } else {
        assert_eq!(code, 1);
    }
}

#[test]
fn help_lists_the_start_command() {
    Command::cargo_bin("ryuzi")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("start"));
}
