use super::SunoClient;
use super::types::{Clip, GenerateResponse, RemasterRequest};
use crate::errors::CliError;

/// Resolve remaster payload fields from the current Web contract
/// (2026-09-11):
/// - `chirp-bass` (v4.5+) omits variation; an explicit value is an error
/// - `chirp-halibut` / `chirp-flounder` / `chirp-carp` send variation
///   (default `normal`)
/// - `style_profile` is v6 (`chirp-halibut`) only (default `boost`)
pub fn remaster_fields(
    model_key: &str,
    variation: Option<&str>,
    style_profile: Option<&str>,
) -> Result<(Option<String>, Option<String>), CliError> {
    let variation_category = match model_key {
        "chirp-bass" => {
            if variation.is_some() {
                return Err(CliError::InvalidInput(
                    "--variation is not supported by the v4.5+ remaster model".into(),
                ));
            }
            None
        }
        "chirp-halibut" | "chirp-flounder" | "chirp-carp" => {
            Some(variation.unwrap_or("normal").to_string())
        }
        other => {
            return Err(CliError::InvalidInput(format!(
                "unsupported remaster model `{other}` — refusing to guess its variation_category contract"
            )));
        }
    };

    let style = if model_key == "chirp-halibut" {
        Some(style_profile.unwrap_or("boost").to_string())
    } else if style_profile.is_some() {
        return Err(CliError::InvalidInput(
            "--style-profile is supported only by the v6 remaster model (chirp-halibut)".into(),
        ));
    } else {
        None
    };

    Ok((variation_category, style))
}

impl SunoClient {
    /// Remaster a clip. Posts to the current web remaster route
    /// `POST /api/generate/upsample` (captured 2026-09-11). The older
    /// v2-web `create_mode: remaster` guess is no longer used.
    pub async fn remaster(
        &self,
        clip_id: &str,
        remaster_model_key: &str,
        variation: Option<&str>,
        style_profile: Option<&str>,
    ) -> Result<Vec<Clip>, CliError> {
        let (variation_category, style_profile) =
            remaster_fields(remaster_model_key, variation, style_profile)?;
        let req = RemasterRequest {
            clip_id: clip_id.to_string(),
            model_name: remaster_model_key.to_string(),
            variation_category,
            style_profile,
        };
        self.with_auth_retry(|| async {
            let resp = self
                .post("/api/generate/upsample")
                .json(&req)
                .send()
                .await?;
            let resp = self.check_response(resp).await?;
            let result: GenerateResponse = resp.json().await?;
            if let Some(status) = result.status.as_deref()
                && status.eq_ignore_ascii_case("error")
            {
                return Err(CliError::GenerationFailed(format!(
                    "Suno rejected the remaster (status: {status})"
                )));
            }
            if result.clips.is_empty() {
                return Err(CliError::GenerationFailed(
                    "Suno returned no clips — the remaster was not created".into(),
                ));
            }
            Ok(result.clips)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::remaster_fields;

    #[test]
    fn remaster_variation_encoding_is_model_specific() {
        let (v, s) = remaster_fields("chirp-halibut", None, None).unwrap();
        assert_eq!(v.as_deref(), Some("normal"));
        assert_eq!(s.as_deref(), Some("boost"));

        let (v, s) = remaster_fields("chirp-halibut", Some("high"), Some("clarity")).unwrap();
        assert_eq!(v.as_deref(), Some("high"));
        assert_eq!(s.as_deref(), Some("clarity"));

        let (v, s) = remaster_fields("chirp-flounder", None, None).unwrap();
        assert_eq!(v.as_deref(), Some("normal"));
        assert!(s.is_none());

        let (v, s) = remaster_fields("chirp-bass", None, None).unwrap();
        assert!(v.is_none());
        assert!(s.is_none());

        assert!(remaster_fields("chirp-bass", Some("high"), None).is_err());
        assert!(remaster_fields("chirp-flounder", None, Some("clarity")).is_err());
        assert!(remaster_fields("chirp-future", None, None).is_err());
    }
}
