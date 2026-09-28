//! Headless authentication input must fail locally and predictably before any
//! network request when stdin or flag selection is invalid.

mod common;
use common::suno_in;

fn assert_invalid(out: std::process::Output) {
    assert_eq!(out.status.code(), Some(3));
    assert!(out.stdout.is_empty());

    let json: serde_json::Value =
        serde_json::from_slice(&out.stderr).expect("stderr should be a JSON error envelope");
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "invalid_input");
}

#[test]
fn cookie_stdin_rejects_empty_input_offline() {
    let tmp = tempfile::tempdir().unwrap();
    let out = suno_in(tmp.path())
        .args(["auth", "--cookie-stdin"])
        .write_stdin(" \n\t")
        .output()
        .unwrap();
    assert_invalid(out);
}

#[test]
fn jwt_stdin_rejects_empty_input_offline() {
    let tmp = tempfile::tempdir().unwrap();
    let out = suno_in(tmp.path())
        .args(["auth", "--jwt-stdin"])
        .write_stdin("")
        .output()
        .unwrap();
    assert_invalid(out);
}

#[test]
fn cookie_stdin_rejects_oversized_input_offline() {
    let tmp = tempfile::tempdir().unwrap();
    let out = suno_in(tmp.path())
        .args(["auth", "--cookie-stdin"])
        .write_stdin(vec![b'x'; 65_537])
        .output()
        .unwrap();
    assert_invalid(out);
}

#[test]
fn stdin_auth_flags_conflict() {
    let tmp = tempfile::tempdir().unwrap();
    let out = suno_in(tmp.path())
        .args(["auth", "--cookie-stdin", "--jwt-stdin"])
        .write_stdin("unused")
        .output()
        .unwrap();
    assert_invalid(out);
}

#[test]
fn direct_and_stdin_cookie_flags_conflict() {
    let tmp = tempfile::tempdir().unwrap();
    let out = suno_in(tmp.path())
        .args(["auth", "--cookie", "unused", "--cookie-stdin"])
        .write_stdin("unused")
        .output()
        .unwrap();
    assert_invalid(out);
}
