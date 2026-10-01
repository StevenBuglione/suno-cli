mod common;
use common::suno_in;
use serde_json::{Value, json};

const ID: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";

fn source(home: &std::path::Path) -> String {
    let path = home.join("source.json");
    std::fs::write(
        &path,
        json!({"version":"1","status":"success","data":{
            "id":ID,"title":"Original","status":"complete","model_name":"chirp-hawk",
            "created_at":"2026-09-30T00:00:00Z","audio_url":null,"video_url":null,"image_url":null,
            "metadata":{"tags":"pop","prompt":"[Verse]\nOriginal words","duration":120}
        }})
        .to_string(),
    )
    .unwrap();
    path.to_string_lossy().into_owned()
}

fn preview(operation: &str, additional: &[&str]) -> Value {
    let temp = tempfile::tempdir().unwrap();
    let file = source(temp.path());
    let mut args = vec![operation, ID, "--source-file", &file, "--dry-run"];
    args.extend_from_slice(additional);
    let output = suno_in(temp.path()).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert!(!temp.path().join("data/jobs").exists());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["submitted"], false);
    value["data"]["request"].clone()
}

#[test]
fn cover_keeps_lyrics_and_exposes_all_three_sliders() {
    let req = preview(
        "cover",
        &[
            "--tags",
            "jazz",
            "--exclude",
            "distortion",
            "--title",
            "Jazz cover",
            "--weirdness",
            "20",
            "--style-influence",
            "80",
            "--audio-influence",
            "65",
            "--vocal",
            "female",
        ],
    );
    assert_eq!(req["task"], "cover");
    assert_eq!(req["prompt"], "[Verse]\nOriginal words");
    assert_eq!(req["cover_clip_id"], ID);
    assert_eq!(req["tags"], "jazz");
    assert_eq!(req["negative_tags"], "distortion");
    assert_eq!(req["metadata"]["vocal_gender"], "female");
    assert_eq!(
        req["metadata"]["control_sliders"],
        json!({"weirdness_constraint":0.2,"style_weight":0.8,"audio_weight":0.65})
    );
}

#[test]
fn reuse_creates_a_new_song_without_audio_reference() {
    let req = preview("reuse", &[]);
    assert_eq!(req["prompt"], "[Verse]\nOriginal words");
    assert_eq!(req["tags"], "pop");
    assert!(req.get("task").is_none());
    assert!(req["cover_clip_id"].is_null());
    assert!(req["continue_clip_id"].is_null());
}

#[test]
fn replace_carries_source_context_and_new_section_lyrics() {
    let req = preview(
        "replace",
        &[
            "--start",
            "30",
            "--end",
            "45",
            "--lyrics",
            "[Chorus]\nNew words",
        ],
    );
    assert_eq!(req["task"], "infill");
    assert_eq!(req["continue_clip_id"], ID);
    assert_eq!(req["prompt"], "[Verse]\nOriginal words");
    assert_eq!(req["metadata"]["infill_lyrics"], "[Chorus]\nNew words");
    assert_eq!(req["continued_aligned_prompt"], "[Chorus]\nNew words");
    assert_eq!(req["infill_context_end_s"], 120.0);
    assert!(req.get("make_instrumental").is_none());
}

#[test]
fn vocal_and_accompaniment_tasks_use_different_source_fields() {
    let vocals = preview("add-vocals", &["--lyrics", "[Verse]\nNew vocals"]);
    assert_eq!(vocals["task"], "overpainting");
    assert_eq!(vocals["overpainting_clip_id"], ID);
    let music = preview("add-instrumental", &["--tags", "acoustic guitar"]);
    assert_eq!(music["task"], "underpainting");
    assert_eq!(music["underpainting_clip_id"], ID);
}

#[test]
fn bad_ranges_and_placeholders_fail_before_authentication() {
    for args in [
        vec!["cover", ID, "--start", "NaN"],
        vec!["replace", ID, "--start", "10", "--end", "5"],
        vec!["cover", ID, "--lyrics", "<unfinished verse>"],
        vec!["extend", ID, "--at", "-1"],
        vec!["speed", ID, "--multiplier", "NaN"],
    ] {
        let temp = tempfile::tempdir().unwrap();
        let output = suno_in(temp.path()).args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(3));
        assert!(!temp.path().join("data/jobs").exists());
    }
}

#[test]
fn offline_source_mismatch_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let file = source(temp.path());
    let output = suno_in(temp.path())
        .args([
            "cover",
            "aaaaaaaa-bbbb-4ccc-8ddd-ffffffffffff",
            "--source-file",
            &file,
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&output.stderr).contains("does not match"));
}

#[test]
fn remaster_uses_the_dedicated_upsample_contract() {
    let temp = tempfile::tempdir().unwrap();
    let output = suno_in(temp.path())
        .args([
            "remaster",
            ID,
            "--variation",
            "high",
            "--style-profile",
            "clarity",
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["data"]["endpoint"], "/api/generate/upsample");
    assert_eq!(
        json["data"]["request"],
        json!({"clip_id":ID,"model_name":"chirp-halibut","variation_category":"high","style_profile":"clarity"})
    );
}

#[test]
fn interactive_login_respects_browser_policy_without_launching() {
    for flag in ["--headless", "--no-browser"] {
        let temp = tempfile::tempdir().unwrap();
        let output = suno_in(temp.path())
            .args([flag, "auth", "--browser-login"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!temp.path().join("config/auth.json").exists());
    }
}

#[test]
fn current_voice_personas_select_reference_tasks() {
    let cover = preview(
        "cover",
        &["--persona", "aaaaaaaa-bbbb-4ccc-8ddd-ffffffffffff"],
    );
    assert_eq!(cover["task"], "vox_cover");
    assert_eq!(cover["override_fields"], json!(["prompt", "tags"]));
    let reuse = preview(
        "reuse",
        &["--persona", "aaaaaaaa-bbbb-4ccc-8ddd-ffffffffffff"],
    );
    assert_eq!(reuse["task"], "vox");
}
