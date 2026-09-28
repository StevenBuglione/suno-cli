//! Distribution-aware update contract. Package-manager and unknown-source full
//! updates are offline instructions-only operations. `--check` is the only
//! managed-source path that consults GitHub Releases.

mod common;
use common::{skip_live, suno};

fn update_json(source: &str, extra: &[&str]) -> (Option<i32>, Vec<u8>, Vec<u8>) {
    let mut args = vec!["update"];
    args.extend_from_slice(extra);
    let out = suno()
        .env("SUNO_INSTALL_SOURCE", source)
        .args(&args)
        .output()
        .unwrap();
    (out.status.code(), out.stdout, out.stderr)
}

#[test]
fn homebrew_full_update_returns_offline_owner_instructions() {
    let bin = assert_cmd::cargo::cargo_bin("suno");
    let before = std::fs::metadata(&bin).unwrap().modified().unwrap();
    let (code, stdout, _) = update_json("homebrew", &[]);
    assert_eq!(code, Some(0));

    let json: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(json["status"], "success");
    assert_eq!(json["data"]["status"], "managed_install");
    assert_eq!(json["data"]["latest_version"], serde_json::Value::Null);
    assert_eq!(json["data"]["requires_skill_reinstall"], false);
    assert_eq!(json["data"]["install_source"], "homebrew");
    assert_eq!(json["data"]["update_mode"], "package_manager");
    assert_eq!(
        json["data"]["upgrade_command"],
        "brew upgrade paperfoot/tap/suno"
    );

    let after = std::fs::metadata(&bin).unwrap().modified().unwrap();
    assert_eq!(before, after, "update must not touch a brew-owned binary");
}

#[test]
fn cargo_update_returns_offline_owner_instructions() {
    let (code, stdout, _) = update_json("cargo", &[]);
    assert_eq!(code, Some(0));
    let json: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(json["data"]["status"], "managed_install");
    assert_eq!(json["data"]["latest_version"], serde_json::Value::Null);
    assert_eq!(json["data"]["requires_skill_reinstall"], false);
    assert_eq!(json["data"]["install_source"], "cargo");
    assert_eq!(json["data"]["update_mode"], "package_manager");
    assert_eq!(
        json["data"]["upgrade_command"],
        "cargo install --locked --force suno"
    );
}

#[test]
fn brew_is_accepted_as_homebrew_alias() {
    let (code, stdout, _) = update_json("brew", &[]);
    assert_eq!(code, Some(0));
    let json: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(json["data"]["install_source"], "homebrew");
}

#[test]
fn invalid_install_source_exits_2() {
    let (code, stdout, stderr) = update_json("spaceship", &["--check"]);
    assert_eq!(code, Some(2));
    assert!(stdout.is_empty());
    let json: serde_json::Value = serde_json::from_slice(&stderr).unwrap();
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "config_error");
}

#[test]
fn unknown_full_update_returns_honest_instructions_only_result() {
    let bin = assert_cmd::cargo::cargo_bin("suno");
    let before = std::fs::metadata(&bin).unwrap().modified().unwrap();
    let (code, stdout, stderr) = update_json("unknown", &[]);
    assert_eq!(code, Some(0));
    assert!(stderr.is_empty());

    let json: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(json["status"], "success");
    assert_eq!(json["data"]["status"], "not_checked");
    assert_eq!(json["data"]["latest_version"], serde_json::Value::Null);
    assert_eq!(json["data"]["install_source"], "unknown");
    assert_eq!(json["data"]["update_mode"], "instructions_only");
    assert_eq!(json["data"]["upgrade_command"], serde_json::Value::Null);

    let after = std::fs::metadata(&bin).unwrap().modified().unwrap();
    assert_eq!(
        before, after,
        "unknown-source update must not touch the binary"
    );
}

#[test]
fn homebrew_check_queries_latest_but_never_self_replaces() {
    if skip_live() {
        return;
    }
    let (code, stdout, _) = update_json("homebrew", &["--check"]);
    assert_eq!(code, Some(0));
    let json: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert!(json["data"]["latest_version"].is_string());
    assert_eq!(json["data"]["update_mode"], "package_manager");
    assert!(matches!(
        json["data"]["status"].as_str(),
        Some("up_to_date" | "update_available" | "unsupported_platform")
    ));
}

#[test]
fn unknown_check_queries_latest_but_stays_instructions_only() {
    if skip_live() {
        return;
    }
    let (code, stdout, _) = update_json("unknown", &["--check"]);
    assert_eq!(code, Some(0));
    let json: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert!(json["data"]["latest_version"].is_string());
    assert_eq!(json["data"]["update_mode"], "instructions_only");
}

#[test]
fn standalone_check_reaches_github_live_without_replacing() {
    if skip_live() {
        return;
    }
    let bin = assert_cmd::cargo::cargo_bin("suno");
    let before = std::fs::metadata(&bin).unwrap().modified().unwrap();
    let (code, stdout, _) = update_json("standalone", &["--check"]);
    assert_eq!(code, Some(0));
    let json: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    let status = json["data"]["status"].as_str().unwrap();
    assert!(
        status == "up_to_date" || status == "update_available" || status == "unsupported_platform",
        "unexpected status: {status}"
    );
    let after = std::fs::metadata(&bin).unwrap().modified().unwrap();
    assert_eq!(before, after, "check mode must not replace the binary");
}
