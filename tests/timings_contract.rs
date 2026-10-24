mod common;

use common::suno_in;
use serde_json::Value;

fn check_report(report: &Value, stage: &str) {
    assert!(report["total_ms"].as_f64().unwrap() >= 0.0);
    let stages = report["stages"].as_array().unwrap();
    assert!(stages.iter().any(|entry| entry["stage"] == stage));
    for entry in stages {
        assert!(entry["duration_ms"].as_f64().unwrap() >= 0.0);
        assert!(entry["start_ms"].as_f64().unwrap() >= 0.0);
        assert!(entry["start_ms"].as_f64().unwrap() <= report["total_ms"].as_f64().unwrap());
        assert_eq!(entry.as_object().unwrap().len(), 3);
    }
    for pair in stages.windows(2) {
        assert!(pair[0]["start_ms"].as_f64().unwrap() <= pair[1]["start_ms"].as_f64().unwrap());
    }
}

#[test]
fn timings_are_opt_in_and_do_not_change_stdout() {
    let home = tempfile::tempdir().unwrap();
    let plain = suno_in(home.path())
        .args(["jobs", "--json"])
        .output()
        .unwrap();
    assert!(plain.status.success());
    assert!(plain.stderr.is_empty());
    for args in [
        vec!["--timings", "jobs", "--json"],
        vec!["jobs", "--timings", "--json", "--quiet"],
    ] {
        let timed = suno_in(home.path()).args(args).output().unwrap();
        assert!(timed.status.success());
        assert_eq!(timed.stdout, plain.stdout);
        let stderr: Value = serde_json::from_slice(&timed.stderr).unwrap();
        check_report(&stderr["timings"], "command.run");
        check_report(&stderr["timings"], "browser.shutdown");
    }
}

#[test]
fn early_auth_error_keeps_one_json_envelope_and_exit_code() {
    let home = tempfile::tempdir().unwrap();
    let plain = suno_in(home.path())
        .args(["credits", "--json"])
        .output()
        .unwrap();
    let timed = suno_in(home.path())
        .args(["credits", "--json", "--timings"])
        .output()
        .unwrap();
    assert!(!timed.status.success());
    assert_eq!(timed.status.code(), plain.status.code());
    assert_eq!(timed.stdout, plain.stdout);
    let mut report: Value = serde_json::from_slice(&timed.stderr).unwrap();
    check_report(&report["timings"], "auth.load");
    check_report(&report["timings"], "command.run");
    report.as_object_mut().unwrap().remove("timings");
    assert_eq!(
        report,
        serde_json::from_slice::<Value>(&plain.stderr).unwrap()
    );
}

#[test]
fn timings_do_not_record_input_or_paths() {
    let home = tempfile::tempdir().unwrap();
    let output = suno_in(home.path())
        .args([
            "describe",
            "--prompt",
            "PRIVATE_TIMING_SENTINEL",
            "--dry-run",
            "--timings",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stderr.contains("PRIVATE_TIMING_SENTINEL"));
    assert!(!stderr.contains(home.path().to_str().unwrap()));
    let report: Value = serde_json::from_str(&stderr).unwrap();
    check_report(&report["timings"], "config.load");
}
