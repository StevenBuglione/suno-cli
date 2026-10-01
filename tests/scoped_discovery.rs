//! Scoped discovery is an offline projection of the executable Clap tree.

#![allow(dead_code)] // cli.rs is imported for parser introspection, not execution.

#[path = "../src/cli.rs"]
mod cli;
mod common;

use std::{collections::BTreeSet, path::Path};

use clap::{Command, CommandFactory, Parser};
use common::{suno_in, write_config_in};
use serde_json::Value;

fn run_json(home: &Path, args: &[&str]) -> (Value, Vec<u8>) {
    let out = suno_in(home).args(args).output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stderr.is_empty(),
        "{args:?} wrote stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value = serde_json::from_slice(&out.stdout).expect("discovery must emit JSON");
    (value, out.stdout)
}

fn command_keys(value: &Value) -> BTreeSet<String> {
    value["commands"]
        .as_object()
        .expect("commands object")
        .keys()
        .cloned()
        .collect()
}

fn public_leaf_paths(root: &Command, prefix: &str, paths: &mut BTreeSet<String>) {
    for command in root
        .get_subcommands()
        .filter(|command| !command.is_hide_set())
    {
        if command.get_name() == "help" {
            continue;
        }
        let path = format!("{prefix}{}", command.get_name());
        if command.get_subcommands().next().is_some() {
            public_leaf_paths(command, &format!("{path} "), paths);
        } else {
            paths.insert(path);
        }
    }
}

#[test]
fn full_manifest_covers_every_public_clap_leaf_in_both_directions() {
    let home = tempfile::tempdir().unwrap();
    let (manifest, _) = run_json(home.path(), &["agent-info"]);

    let mut root = cli::Cli::command();
    root.build();
    let mut clap_paths = BTreeSet::new();
    public_leaf_paths(&root, "", &mut clap_paths);
    assert_eq!(command_keys(&manifest), clap_paths);

    for (path, command) in manifest["commands"].as_object().unwrap() {
        assert!(command["description"].is_string(), "{path}: description");
        assert!(command["args"].is_array(), "{path}: args");
        assert!(command["options"].is_array(), "{path}: options");
        assert!(command["effect"].is_string(), "{path}: effect");
        assert!(command["idempotent"].is_boolean(), "{path}: idempotent");
        assert!(command["examples"].is_array(), "{path}: examples");
    }
}

#[test]
fn exact_and_group_scopes_are_projections_of_the_full_manifest() {
    let home = tempfile::tempdir().unwrap();
    let (full, _) = run_json(home.path(), &["agent-info"]);
    let (generate, _) = run_json(home.path(), &["agent-info", "--command", "generate"]);
    let (config, _) = run_json(home.path(), &["agent-info", "--command", "  config  "]);

    assert_eq!(command_keys(&generate), BTreeSet::from(["generate".into()]));
    assert_eq!(
        generate["commands"]["generate"],
        full["commands"]["generate"]
    );
    assert_eq!(
        command_keys(&config),
        BTreeSet::from([
            "config check".into(),
            "config path".into(),
            "config set".into(),
            "config show".into(),
        ])
    );
    for path in command_keys(&config) {
        assert_eq!(config["commands"][&path], full["commands"][&path]);
    }

    let mut full_metadata = full.clone();
    let mut scoped_metadata = config.clone();
    full_metadata.as_object_mut().unwrap().remove("commands");
    scoped_metadata.as_object_mut().unwrap().remove("commands");
    assert_eq!(scoped_metadata, full_metadata);
}

#[test]
fn info_without_an_id_is_the_discovery_alias() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(
        run_json(home.path(), &["info"]).0,
        run_json(home.path(), &["agent-info"]).0
    );
    assert_eq!(
        run_json(home.path(), &["info", "--command", "download"]).0,
        run_json(home.path(), &["agent-info", "--command", "download"]).0
    );
}

#[test]
fn unknown_empty_and_alias_filters_are_input_errors() {
    for path in ["", "unknown", "con", "configx", "ls", "contract"] {
        let home = tempfile::tempdir().unwrap();
        let out = suno_in(home.path())
            .args(["agent-info", "--command", path])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(3), "filter {path:?}");
        assert!(out.stdout.is_empty(), "filter {path:?} wrote stdout");
        let error: Value = serde_json::from_slice(&out.stderr)
            .unwrap_or_else(|_| panic!("filter {path:?} did not emit JSON stderr"));
        assert_eq!(error["error"]["code"], "invalid_input", "filter {path:?}");
    }
}

#[test]
fn discovery_is_compact_and_does_not_load_malformed_config() {
    let home = tempfile::tempdir().unwrap();
    write_config_in(home.path(), "{{invalid toml");

    let (scoped, bytes) = run_json(
        home.path(),
        &["--json", "--quiet", "agent-info", "--command", "generate"],
    );
    assert_eq!(command_keys(&scoped), BTreeSet::from(["generate".into()]));
    assert_eq!(bytes.iter().filter(|byte| **byte == b'\n').count(), 1);
    assert_eq!(bytes.last(), Some(&b'\n'));
    assert!(!bytes.windows(2).any(|window| window == b"\n "));
}

#[test]
fn syntax_includes_arity_defaults_enums_aliases_shorts_and_conflicts() {
    let home = tempfile::tempdir().unwrap();
    let (info, _) = run_json(home.path(), &["agent-info"]);

    let options = |command: &str| info["commands"][command]["options"].as_array().unwrap();
    let option = |command: &str, name: &str| {
        options(command)
            .iter()
            .find(|option| option["name"] == name)
            .unwrap_or_else(|| panic!("missing {command} {name}"))
    };
    assert_eq!(option("generate", "--title")["short"], "-t");
    assert_eq!(
        option("generate", "--lyrics")["conflicts"],
        serde_json::json!(["--lyrics-file"])
    );
    assert_eq!(option("remaster", "--model")["default"], "v6");
    assert!(
        option("remaster", "--model")["values"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("v6"))
    );
    assert_eq!(
        option("generate", "--token-provider")["aliases"],
        serde_json::json!(["--captcha-provider"])
    );
    assert_eq!(
        info["commands"]["status"]["args"][0]["arity"],
        serde_json::json!({"min": 1, "max": null})
    );
    assert_eq!(
        info["commands"]["list"]["aliases"],
        serde_json::json!(["ls"])
    );
    for flag in ["--json", "--quiet", "--headless", "--no-browser"] {
        assert!(info["global_flags"][flag].is_object(), "missing {flag}");
    }
}

#[test]
fn every_advertised_example_parses_without_executing_it() {
    let home = tempfile::tempdir().unwrap();
    let (manifest, _) = run_json(home.path(), &["agent-info"]);

    for (path, command) in manifest["commands"].as_object().unwrap() {
        let examples = command["examples"]
            .as_array()
            .unwrap_or_else(|| panic!("{path} examples"));
        assert!(!examples.is_empty(), "{path} needs an example");
        for example in examples {
            let argv = std::iter::once("suno").chain(
                example
                    .as_array()
                    .expect("example argv")
                    .iter()
                    .map(|arg| arg.as_str().expect("example string")),
            );
            assert!(
                cli::Cli::try_parse_from(argv).is_ok(),
                "{path} has an example that does not parse: {example}"
            );
        }
    }
}
