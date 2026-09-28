//! An offline, transparent musical brief. Presets are examples, not measured optima.
use crate::{cli::PromptArgs, errors::CliError, output::OutputFormat};
use serde_json::{Value, json};
use std::io::Write;

const PRESETS: &str = include_str!("../../assets/prompt-presets.json");
const DIMENSIONS: &[&str] = &[
    "genre",
    "mood",
    "bpm",
    "meter",
    "beat_unit",
    "groove",
    "key",
    "voice",
    "delivery",
    "instruments",
    "arrangement",
    "production",
    "exclude",
];

fn build(args: &PromptArgs) -> Result<Value, CliError> {
    let presets: Value = serde_json::from_str(PRESETS)?;
    if args.list_presets {
        return Ok(json!({"presets": presets, "guide": "suno guide prompting"}));
    }
    let mut brief = if let Some(name) = &args.preset {
        presets.get(name).cloned().ok_or_else(|| {
            CliError::InvalidInput(format!(
                "unknown preset '{name}'; use suno prompt --list-presets"
            ))
        })?
    } else {
        json!({})
    };
    for (key, value) in [
        ("genre", &args.genre),
        ("mood", &args.mood),
        ("meter", &args.meter),
        ("beat_unit", &args.beat_unit),
        ("groove", &args.groove),
        ("key", &args.key),
        ("voice", &args.voice),
        ("delivery", &args.delivery),
        ("instruments", &args.instruments),
        ("arrangement", &args.arrangement),
        ("production", &args.production),
        ("exclude", &args.exclude),
    ] {
        if let Some(value) = value {
            if value.trim().is_empty() {
                return Err(CliError::InvalidInput(format!(
                    "--{} cannot be blank",
                    key.replace('_', "-")
                )));
            }
            brief[key] = json!(value.trim());
        }
    }
    if let Some(bpm) = args.bpm {
        brief["bpm"] = json!(bpm);
    }
    if args.instrumental {
        brief.as_object_mut().unwrap().remove("voice");
        if args.delivery.is_none() {
            brief.as_object_mut().unwrap().remove("delivery");
        }
    }
    if let Some(meter) = brief["meter"].as_str() {
        let valid = meter.split_once('/').is_some_and(|(top, bottom)| {
            top.parse::<u32>().is_ok_and(|v| (1..=32).contains(&v))
                && bottom
                    .parse::<u32>()
                    .is_ok_and(|v| [1, 2, 4, 8, 16, 32, 64].contains(&v))
        });
        if !valid {
            return Err(CliError::InvalidInput(
                "--meter must be a time signature such as 4/4, 3/4, or 6/8".into(),
            ));
        }
    }
    let mut warnings = vec!["BPM, key, meter, voice, and section directions are requests. Verify the rendered audio before marking them as achieved.".to_string()];
    if brief["bpm"].is_number() && brief["beat_unit"].is_null() {
        warnings.push("BPM has no beat unit: choose quarter, dotted-quarter, or eighth to avoid half/double-time ambiguity.".into());
    }
    if !brief["beat_unit"].is_null() && brief["bpm"].is_null() {
        warnings.push("A beat unit was supplied without a BPM target.".into());
    }
    if args.preset.is_some() {
        warnings.push("Preset directions are original examples. Edit the cast, performance, and instrumentation for this song.".into());
    }
    let mut parts = Vec::new();
    for key in ["genre", "mood"] {
        if let Some(s) = brief[key].as_str() {
            parts.push(s.to_owned());
        }
    }
    if let Some(bpm) = brief["bpm"].as_u64() {
        parts.push(match brief["beat_unit"].as_str() {
            Some(unit) => format!("{bpm} BPM ({unit} pulse)"),
            None => format!("{bpm} BPM"),
        });
    }
    for (key, label) in [
        ("meter", "meter"),
        ("groove", "groove"),
        ("key", "tonality"),
        ("voice", "voice"),
        ("delivery", "delivery"),
        ("instruments", "instruments"),
        ("arrangement", "arrangement"),
        ("production", "production"),
    ] {
        if let Some(s) = brief[key].as_str() {
            parts.push(format!("{label}: {s}"));
        }
    }
    if args.instrumental {
        parts.push("instrumental, no vocals".into());
    }
    let tags = parts.join(". ");
    let exclude = brief["exclude"].as_str().unwrap_or("");
    for (label, text) in [("style prompt", tags.as_str()), ("exclusions", exclude)] {
        if text.encode_utf16().count() > 1000 {
            return Err(CliError::InvalidInput(format!(
                "{label} exceeds the current 1000 UTF-16 unit limit; shorten the directions"
            )));
        }
    }
    let missing: Vec<_> = DIMENSIONS
        .iter()
        .filter(|&&k| brief[k].is_null() && !(k == "voice" && args.instrumental))
        .collect();
    let mut argv: Vec<String> = vec![
        "suno".into(),
        "generate".into(),
        "--model".into(),
        "v6".into(),
        "--request-id".into(),
        uuid::Uuid::new_v4().to_string(),
    ];
    if !tags.is_empty() {
        argv.extend(["--tags".into(), tags.clone()]);
    }
    if !exclude.is_empty() {
        argv.extend(["--exclude".into(), exclude.to_owned()]);
    }
    if let Some(title) = &args.title {
        argv.extend(["--title".into(), title.clone()]);
    }
    if let Some(vocal) = &args.vocal {
        argv.extend([
            "--vocal".into(),
            match vocal {
                crate::cli::VocalGender::Male => "male",
                crate::cli::VocalGender::Female => "female",
            }
            .into(),
        ]);
    }
    if args.instrumental {
        argv.push("--instrumental".into());
    }
    let ready = if let Some(file) = &args.lyrics_file {
        // Check the actual path so a generated command never points at a guessed filename.
        let lyrics = std::fs::read_to_string(file).map_err(|e| {
            CliError::InvalidInput(format!("cannot read lyrics file '{file}': {e}"))
        })?;
        if !crate::commands::write::placeholder_lines(&lyrics).is_empty() {
            warnings.push(
                "Lyrics still contain <...> placeholders; finish them before generation.".into(),
            );
        }
        let absolute = std::fs::canonicalize(file)?;
        argv.extend([
            "--lyrics-file".into(),
            absolute.to_string_lossy().into_owned(),
        ]);
        true
    } else {
        args.instrumental
    };
    argv.push("--wait".into());
    let preview = {
        let mut a = argv.clone();
        a.push("--dry-run".into());
        a
    };
    Ok(json!({
        "brief": brief, "style_prompt": tags, "negative_tags": exclude,
        "instrumental": args.instrumental, "missing_directions": missing, "warnings": warnings,
        "controls": {"weirdness": null, "style_influence": null, "note": "No slider preset is imposed. Read suno guide prompting before choosing overrides."},
        "next_action": if ready { json!({"argv": argv, "preview_argv": preview, "note": "Keep this request ID when retrying the same song. Audition both results before a larger batch."}) } else { json!({"argv": ["suno", "guide", "songwriting"], "note": "Write the lyrics, then rerun prompt with --lyrics-file; or use --instrumental for music without vocals."}) },
        "guide": "suno guide prompting"
    }))
}

pub fn run(args: PromptArgs, fmt: OutputFormat) -> Result<(), CliError> {
    let result = build(&args)?;
    match fmt {
        OutputFormat::Json => crate::output::json::success(result),
        OutputFormat::Table => {
            let mut stdout = std::io::stdout().lock();
            if args.list_presets {
                writeln!(
                    stdout,
                    "{}",
                    serde_json::to_string_pretty(&result["presets"])?
                )?;
            } else {
                writeln!(
                    stdout,
                    "Style: {}\nExclude: {}",
                    result["style_prompt"].as_str().unwrap_or_default(),
                    result["negative_tags"].as_str().unwrap_or_default()
                )?;
                writeln!(
                    stdout,
                    "\nNext action (argv): {}",
                    result["next_action"]["argv"]
                )?;
                for warning in result["warnings"].as_array().unwrap() {
                    writeln!(stdout, "{}", warning.as_str().unwrap())?;
                }
            }
            Ok(())
        }
    }
}
