//! Non-destructive song edits captured from Suno's current public web bundle.
use super::{SunoClient, types::Clip};
use crate::errors::CliError;
use serde_json::{Value, json};

impl SunoClient {
    pub async fn crop(&self, id: &str, request: &Value) -> Result<String, CliError> {
        self.with_auth_retry(|| async {
            let response = self
                .post(&format!("/api/edit/crop/{id}/"))
                .json(request)
                .send()
                .await?;
            let body: Value = self.check_response(response).await?.json().await?;
            body["action_clip_id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| {
                    CliError::GenerationFailed(
                        "crop returned no action ID; inspect the library before submitting again"
                            .into(),
                    )
                })
        })
        .await
    }

    pub async fn edit_action(&self, id: &str) -> Result<Value, CliError> {
        self.with_auth_retry(|| async {
            let response = self.get(&format!("/api/edit/action/{id}/")).send().await?;
            Ok(self.check_response(response).await?.json().await?)
        })
        .await
    }

    pub async fn poll_edit_action(
        &self,
        id: &str,
        timeout: u64,
        interval: u64,
    ) -> Result<Vec<Clip>, CliError> {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout);
        loop {
            let body = tokio::time::timeout_at(deadline, self.edit_action(id))
                .await
                .map_err(|_| {
                    CliError::GenerationFailed(format!(
                        "edit timed out; resume `suno edit-status {id} --wait`"
                    ))
                })??;
            match body["status"].as_str() {
                Some("complete") => {
                    return self
                        .poll_clips(
                            &[id.to_owned()],
                            deadline
                                .saturating_duration_since(tokio::time::Instant::now())
                                .as_secs(),
                            interval,
                        )
                        .await;
                }
                Some("error") => {
                    return Err(CliError::GenerationFailed(
                        body["error_message"]
                            .as_str()
                            .unwrap_or("Suno edit worker failed")
                            .into(),
                    ));
                }
                _ => {}
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(CliError::GenerationFailed(format!(
                    "edit timed out; resume `suno edit-status {id} --wait`"
                )));
            }
            tokio::time::sleep(
                std::time::Duration::from_secs(interval.max(1))
                    .min(deadline.saturating_duration_since(tokio::time::Instant::now())),
            )
            .await;
        }
    }

    pub async fn transform(&self, operation: &str, request: &Value) -> Result<Clip, CliError> {
        let path = match operation {
            "speed" => "/api/clips/adjust-speed/",
            "reverse" => "/api/clips/reverse-clip/",
            _ => return Err(CliError::InvalidInput("unknown audio transform".into())),
        };
        self.with_auth_retry(|| async {
            let response = self.post(path).json(request).send().await?;
            Ok(self.check_response(response).await?.json().await?)
        })
        .await
    }
}

pub fn crop_request(start: f64, end: f64, remove: bool, title: &str) -> Result<Value, CliError> {
    super::cover::validate_range(Some(start), Some(end), None)?;
    Ok(
        json!({"crop_start_s":start,"crop_end_s":end,"is_crop_remove":remove,"title":title,"ui_surface":"song_actions"}),
    )
}

pub fn speed_request(
    id: &str,
    multiplier: f64,
    keep_pitch: bool,
    title: &str,
) -> Result<Value, CliError> {
    if !multiplier.is_finite() || multiplier <= 0.0 {
        return Err(CliError::InvalidInput(
            "--multiplier must be finite and greater than zero".into(),
        ));
    }
    Ok(json!({"clip_id":id,"speed_multiplier":multiplier,"keep_pitch":keep_pitch,"title":title}))
}
