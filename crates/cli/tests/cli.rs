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
