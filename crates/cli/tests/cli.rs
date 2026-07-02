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
fn doctor_prints_four_report_lines() {
    let assert = Command::cargo_bin("ryuzi").unwrap().arg("doctor").assert();
    let output = assert.get_output().clone();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        4,
        "doctor must print exactly 4 lines, got: {stdout}"
    );
    assert!(lines[0].starts_with("git:    "), "line 1: {}", lines[0]);
    assert!(lines[1].starts_with("claude: "), "line 2: {}", lines[1]);
    assert!(lines[2].starts_with("auth:   "), "line 3: {}", lines[2]);
    assert!(lines[3].starts_with("doctor: "), "line 4: {}", lines[3]);
    // Exit code must agree with the verdict line (environment-tolerant: claude may
    // legitimately be missing on CI runners).
    let code = output.status.code().unwrap();
    if lines[3] == "doctor: PASS" {
        assert_eq!(code, 0);
    } else {
        assert_eq!(code, 1);
    }
}
