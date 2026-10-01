//! Contract tests for the offline `suno prompt` brief builder and its
//! hand-off to `generate --dry-run`.

mod common;
use common::suno_in;
use serde_json::Value;

fn prompt_json(home: &std::path::Path, args: &[&str]) -> Value {
    let out = suno_in(home)
        .arg("prompt")
        .args(args)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "prompt failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("prompt output should be JSON")
}

#[test]
fn every_documented_preset_is_accepted_without_slider_defaults() {
    let tmp = tempfile::tempdir().unwrap();
    for preset in ["comic-folk", "work-song", "solo-lament", "electronic"] {
        let value = prompt_json(tmp.path(), &["--preset", preset]);
        assert_eq!(value["status"], "success", "preset {preset}");
        assert_eq!(value["data"]["controls"]["weirdness"], Value::Null);
        assert_eq!(value["data"]["controls"]["style_influence"], Value::Null);
    }
}

#[test]
fn explicit_rhythm_and_voice_overrides_are_preserved_exactly() {
    let tmp = tempfile::tempdir().unwrap();
    let value = prompt_json(
        tmp.path(),
        &[
            "--preset",
            "comic-folk",
            "--bpm",
            "87",
            "--meter",
            "7/8",
            "--beat-unit",
            "eighth",
            "--voice",
            "one weathered adult contralto, close and unison-free",
        ],
    );
    let data = &value["data"];
    assert_eq!(data["brief"]["bpm"], 87);
    assert_eq!(data["brief"]["meter"], "7/8");
    assert_eq!(data["brief"]["beat_unit"], "eighth");
    assert_eq!(
        data["brief"]["voice"],
        "one weathered adult contralto, close and unison-free"
    );
    let style = data["style_prompt"].as_str().unwrap();
    assert!(style.contains("87 BPM (eighth pulse)"), "{style}");
    assert!(style.contains("meter: 7/8"), "{style}");
    assert!(
        style.contains("voice: one weathered adult contralto, close and unison-free"),
        "{style}"
    );
}

#[test]
fn exclusions_stay_separate_and_omitted_dimensions_are_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let excluded = "country twang, breathy crooning";
    let value = prompt_json(
        tmp.path(),
        &["--genre", "chamber folk", "--exclude", excluded],
    );
    let data = &value["data"];
    assert_eq!(data["negative_tags"], excluded);
    assert!(!data["style_prompt"].as_str().unwrap().contains(excluded));
    let missing = data["missing_directions"].as_array().unwrap();
    for direction in ["mood", "bpm", "meter", "voice", "delivery"] {
        assert!(
            missing.iter().any(|item| item == direction),
            "missing_directions should include {direction}: {missing:?}"
        );
    }
    assert!(!missing.iter().any(|item| item == "genre"));
    assert!(!missing.iter().any(|item| item == "exclude"));
}

#[test]
fn emitted_preview_argv_round_trips_into_offline_generate() {
    let tmp = tempfile::tempdir().unwrap();
    let lyrics_path = tmp.path().join("finished lyrics.txt");
    let lyrics = "[Verse]\nThe river keeps the lantern's name.\n\n[Chorus]\nCarry it home again.\n";
    std::fs::write(&lyrics_path, lyrics).unwrap();
    let title = "Lantern Name";
    let excluded = "children's choir, glossy pop";
    let value = prompt_json(
        tmp.path(),
        &[
            "--genre",
            "modal river work song",
            "--bpm",
            "82",
            "--meter",
            "4/4",
            "--beat-unit",
            "quarter",
            "--voice",
            "one adult low caller and mixed adult responses",
            "--delivery",
            "projected calls and rough unison answers",
            "--exclude",
            excluded,
            "--title",
            title,
            "--lyrics-file",
            lyrics_path.to_str().unwrap(),
        ],
    );
    let data = &value["data"];
    let style = data["style_prompt"].as_str().unwrap().to_owned();
    let argv: Vec<String> = data["next_action"]["preview_argv"]
        .as_array()
        .expect("finished lyrics should produce preview argv")
        .iter()
        .map(|arg| arg.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(argv.first().map(String::as_str), Some("suno"));

    let out = suno_in(tmp.path())
        .args(&argv[1..])
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "generated preview command failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let preview: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(preview["data"]["dry_run"], true);
    assert_eq!(preview["data"]["submitted"], false);
    assert_eq!(preview["data"]["request"]["title"], title);
    assert_eq!(preview["data"]["request"]["prompt"], lyrics);
    assert_eq!(preview["data"]["request"]["tags"], style);
    assert_eq!(preview["data"]["request"]["negative_tags"], excluded);
    assert_eq!(
        preview["data"]["request"]["metadata"]["control_sliders"],
        Value::Null
    );
}

#[test]
fn excessive_utf16_and_invalid_meter_are_bad_input() {
    let tmp = tempfile::tempdir().unwrap();
    let too_long = "🎵".repeat(501); // 1002 UTF-16 units.
    suno_in(tmp.path())
        .args(["prompt", "--genre", &too_long, "--json"])
        .assert()
        .code(3);
    suno_in(tmp.path())
        .args(["prompt", "--meter", "four-four", "--json"])
        .assert()
        .code(3);
}

#[test]
fn force_does_not_bypass_placeholders_but_allow_placeholders_does() {
    let tmp = tempfile::tempdir().unwrap();
    let lyrics_path = tmp.path().join("unfinished.txt");
    std::fs::write(&lyrics_path, "[Verse]\n<finish this line>\n").unwrap();
    let path = lyrics_path.to_str().unwrap();

    suno_in(tmp.path())
        .args([
            "generate",
            "--lyrics-file",
            path,
            "--force",
            "--dry-run",
            "--json",
        ])
        .assert()
        .code(3);
    suno_in(tmp.path())
        .args([
            "generate",
            "--lyrics-file",
            path,
            "--allow-placeholders",
            "--dry-run",
            "--json",
        ])
        .assert()
        .code(0);
}
