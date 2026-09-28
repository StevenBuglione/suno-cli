//! Durable receipts contain identifiers, never credentials or lyric payloads.
use crate::{
    config,
    errors::CliError,
    output::{self, OutputFormat},
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub fn resume_argv(ids: &[String]) -> Vec<String> {
    let mut argv = vec!["suno".into(), "status".into()];
    argv.extend_from_slice(ids);
    argv.push("--wait".into());
    argv
}

/// Reserve storage before submission. Keep an ambiguous submission receipt if
/// the connection drops after Suno may have accepted it.
fn fingerprint(req: &crate::api::types::GenerateRequest) -> Result<String, CliError> {
    use sha2::{Digest, Sha256};
    let mut value = serde_json::to_value(req)?;
    value.as_object_mut().unwrap().remove("token");
    value.as_object_mut().unwrap().remove("token_provider");
    value.as_object_mut().unwrap().remove("transaction_uuid");
    value["metadata"]
        .as_object_mut()
        .unwrap()
        .remove("create_session_token");
    fn canonicalize(value: &mut Value) {
        match value {
            Value::Object(map) => {
                for v in map.values_mut() {
                    canonicalize(v);
                }
                map.sort_keys();
            }
            Value::Array(values) => {
                for v in values {
                    canonicalize(v);
                }
            }
            _ => {}
        }
    }
    canonicalize(&mut value);
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(&value)?)))
}

pub fn existing(req: &crate::api::types::GenerateRequest) -> Result<Option<Vec<String>>, CliError> {
    let path = config::data_dir()
        .join("jobs")
        .join(format!("{}.json", req.transaction_uuid));
    if !path.exists() {
        return Ok(None);
    }
    let receipt: Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
    if receipt["request_sha256"] != fingerprint(req)? {
        return Err(CliError::InvalidInput(
            "--request-id was already used for a different request; choose a new UUID".into(),
        ));
    }
    let ids: Vec<String> = serde_json::from_value(receipt["ids"].clone())?;
    if receipt["state"] == "submitted" && !ids.is_empty() {
        return Ok(Some(ids));
    }
    Err(CliError::InvalidInput(format!(
        "request {} has state {}; inspect `suno jobs` and `suno list` before using a new request ID",
        req.transaction_uuid, receipt["state"]
    )))
}

pub fn prepare(req: &crate::api::types::GenerateRequest) -> Result<PathBuf, CliError> {
    let dir = config::data_dir().join("jobs");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", req.transaction_uuid));
    let receipt = json!({"transaction_id":req.transaction_uuid, "request_sha256":fingerprint(req)?,
        "updated_at":chrono::Utc::now().to_rfc3339(), "state":"submitting", "ids":[],
        "next_action":{"argv":["suno", "list"], "note":"Check the library before resubmitting an uncertain request"}});
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new_in(&dir)?;
    file.write_all(serde_json::to_string_pretty(&receipt)?.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist_noclobber(&path).map_err(|e| {
        CliError::Config(format!(
            "cannot reserve request receipt {}: {}",
            path.display(),
            e.error
        ))
    })?;
    Ok(path)
}

pub fn save(path: &Path, transaction: &str, ids: &[String], state: &str) -> Result<(), CliError> {
    let previous: Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let receipt = json!({
        "request_sha256": previous["request_sha256"],
        "transaction_id": transaction,
        "updated_at": chrono::Utc::now().to_rfc3339(),
        "state": state,
        "ids": ids,
        "next_action": if ids.is_empty() { json!({"argv":["suno", "list"], "note":"Check the library before resubmitting an uncertain request"}) }
            else { json!({"argv": resume_argv(ids)}) },
    });
    let temp = path.with_extension("json.tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    let mut file = options.open(&temp)?;
    file.write_all(serde_json::to_string_pretty(&receipt)?.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(temp, path)?;
    Ok(())
}

pub fn run(limit: u32, fmt: OutputFormat) -> Result<(), CliError> {
    let dir = config::data_dir().join("jobs");
    let mut jobs: Vec<Value> = Vec::new();
    if dir.exists() {
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "json") {
                let mut receipt: Value = match std::fs::read_to_string(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|s| serde_json::from_str::<Value>(&s).map_err(|e| e.to_string()))
                {
                    Ok(v) if v.is_object() => v,
                    _ => {
                        json!({"state":"unreadable", "transaction_id":path.file_stem().unwrap_or_default().to_string_lossy(),
                        "ids":[], "next_action":{"argv":["suno","list"],"note":"Receipt is unreadable; inspect the library before resubmitting"}})
                    }
                };
                receipt["path"] = json!(path);
                jobs.push(receipt);
            }
        }
    }
    jobs.sort_by(|a, b| b["updated_at"].as_str().cmp(&a["updated_at"].as_str()));
    jobs.truncate(limit as usize);
    match fmt {
        OutputFormat::Json => output::json::with_status(
            if jobs.is_empty() {
                "no_results"
            } else {
                "success"
            },
            jobs,
        )?,
        OutputFormat::Table => {
            for job in jobs {
                println!(
                    "{}  {}  {}",
                    job["updated_at"].as_str().unwrap_or(""),
                    job["state"].as_str().unwrap_or(""),
                    job["path"].as_str().unwrap_or("")
                );
                println!("  {}", job["next_action"]["argv"]);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::GenerateRequest;

    #[test]
    #[allow(clippy::result_large_err)] // figment Jail requires this error type
    fn receipt_reserves_once_preserves_ids_and_never_stores_secrets() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("SUNO_DATA_DIR", jail.directory().display().to_string());
            let mut req = GenerateRequest::new("chirp-hawk", "custom");
            req.prompt = "private lyrics".into();
            req.token = Some("private captcha".into());
            assert!(existing(&req).unwrap().is_none());
            let path = prepare(&req).unwrap();
            assert!(prepare(&req).is_err());
            assert!(existing(&req).is_err()); // outcome unknown: do not replay
            let ids = vec!["a".into(), "b".into()];
            save(&path, &req.transaction_uuid, &ids, "submitted").unwrap();
            let saved = std::fs::read_to_string(path).unwrap();
            assert!(!saved.contains("private"));
            assert_eq!(existing(&req).unwrap(), Some(ids.clone()));
            req.token = None;
            req.metadata.create_session_token = uuid::Uuid::new_v4().to_string();
            assert_eq!(existing(&req).unwrap(), Some(ids));
            req.prompt = "changed lyrics".into();
            assert_eq!(existing(&req).unwrap_err().exit_code(), 3);
            Ok(())
        });
    }
}
