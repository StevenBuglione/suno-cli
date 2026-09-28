//! Offline and browser-free behavior that agents can rely on.

mod common;
use common::suno_in;

fn json_stdout(out: &std::process::Output) -> serde_json::Value {
    assert!(
        out.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("stdout should be a JSON envelope")
}

fn assert_invalid_before_auth(out: std::process::Output) {
    assert_eq!(out.status.code(), Some(3));
    assert!(out.stdout.is_empty());
    let json: serde_json::Value =
        serde_json::from_slice(&out.stderr).expect("stderr should be a JSON error envelope");
    assert_eq!(json["error"]["code"], "invalid_input");
}

#[test]
fn generate_dry_run_is_offline_v6_preview_and_writes_no_receipt() {
    let tmp = tempfile::tempdir().unwrap();
    let out = suno_in(tmp.path())
        .args([
            "--headless",
            "generate",
            "--title",
            "Offline preview",
            "--tags",
            "pop, bright synths",
            "--lyrics",
            "[Verse]\nA local-only preview",
            "--model",
            "v6",
            "--weirdness",
            "25",
            "--style-influence",
            "75",
            "--audio-influence",
            "40",
            "--max-mode",
            "--dry-run",
            "--json",
        ])
        .output()
        .unwrap();
    let json = json_stdout(&out);
    let data = &json["data"];
    assert_eq!(data["dry_run"], true);
    assert_eq!(data["submitted"], false);
    assert_eq!(data["request"]["mv"], "chirp-hawk");
    assert_eq!(data["request"]["token"], serde_json::Value::Null);
    assert_eq!(data["request"]["metadata"]["is_max_mode"], true);
    assert_eq!(
        data["request"]["metadata"]["control_sliders"]["weirdness_constraint"],
        0.25
    );
    assert_eq!(
        data["request"]["metadata"]["control_sliders"]["style_weight"],
        0.75
    );
    assert_eq!(
        data["request"]["metadata"]["control_sliders"]["audio_weight"],
        0.4
    );
    assert!(!tmp.path().join("data/jobs").exists());
}

#[test]
fn describe_dry_run_is_offline_and_writes_no_receipt() {
    let tmp = tempfile::tempdir().unwrap();
    let out = suno_in(tmp.path())
        .args([
            "--headless",
            "describe",
            "--prompt",
            "quiet piano under distant rain",
            "--model",
            "v6",
            "--weirdness",
            "10",
            "--style-influence",
            "60",
            "--max-mode",
            "--dry-run",
            "--json",
        ])
        .output()
        .unwrap();
    let json = json_stdout(&out);
    assert_eq!(json["data"]["dry_run"], true);
    assert_eq!(json["data"]["submitted"], false);
    assert_eq!(json["data"]["request"]["mv"], "chirp-hawk");
    assert_eq!(
        json["data"]["request"]["metadata"]["create_mode"],
        "inspiration"
    );
    assert_eq!(json["data"]["request"]["token"], serde_json::Value::Null);
    assert!(!tmp.path().join("data/jobs").exists());
}

#[test]
fn no_browser_auth_without_credentials_is_a_local_setup_error() {
    let tmp = tempfile::tempdir().unwrap();
    let out = suno_in(tmp.path())
        .args(["--no-browser", "auth", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let json: serde_json::Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(json["error"]["code"], "auth_missing");
}

#[test]
fn oversized_unicode_description_is_rejected_before_auth() {
    let tmp = tempfile::tempdir().unwrap();
    // Each emoji is two UTF-16 code units. The offline v6 description limit
    // is 3,000 units, so 1,501 emoji must fail even though chars().count()
    // would report only 1,501.
    let prompt = "🎵".repeat(1501);
    let out = suno_in(tmp.path())
        .args(["--no-browser", "describe", "--prompt", &prompt, "--json"])
        .output()
        .unwrap();
    assert_invalid_before_auth(out);
    assert!(!tmp.path().join("data/jobs").exists());
}

#[test]
fn non_finite_and_out_of_range_sliders_are_rejected_before_auth() {
    for (flag, value) in [("--weirdness", "NaN"), ("--style-influence", "101")] {
        let tmp = tempfile::tempdir().unwrap();
        let out = suno_in(tmp.path())
            .args([
                "--no-browser",
                "describe",
                "--prompt",
                "offline validation",
                flag,
                value,
                "--json",
            ])
            .output()
            .unwrap();
        assert_invalid_before_auth(out);
        assert!(!tmp.path().join("data/jobs").exists());
    }
}

#[test]
fn empty_jobs_is_a_successful_no_results_envelope() {
    let tmp = tempfile::tempdir().unwrap();
    let out = suno_in(tmp.path())
        .args(["jobs", "--json"])
        .output()
        .unwrap();
    let json = json_stdout(&out);
    assert_eq!(json["status"], "no_results");
    assert_eq!(json["data"], serde_json::json!([]));
}

#[test]
fn status_download_help_documents_that_it_waits() {
    let tmp = tempfile::tempdir().unwrap();
    let out = suno_in(tmp.path())
        .args(["status", "--help"])
        .output()
        .unwrap();
    let json = json_stdout(&out);
    let usage = json["data"]["usage"].as_str().unwrap().to_ascii_lowercase();
    assert!(usage.contains("--download"));
    assert!(usage.contains("implies --wait"));
}

#[test]
fn jobs_lists_valid_receipts_even_when_another_receipt_is_corrupt() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data/jobs");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("broken.json"), "{broken").unwrap();
    std::fs::write(
        dir.join("valid.json"),
        r#"{"state":"submitted","ids":["a"],"updated_at":"2026-09-28","transaction_id":"valid"}"#,
    )
    .unwrap();
    let output = common::suno_in(tmp.path())
        .args(["jobs", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let jobs = value["data"].as_array().unwrap();
    assert_eq!(jobs.len(), 2);
    assert!(jobs.iter().any(|j| j["state"] == "unreadable"));
    assert!(jobs.iter().any(|j| j["ids"][0] == "a"));
}
