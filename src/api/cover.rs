//! Cover builder from Suno's public web bundle, retrieved 2026-09-30.
use super::types::{Clip, ControlSliders, GenerateRequest};
use crate::errors::CliError;

#[derive(Default)]
pub struct CoverOptions {
    pub title: Option<String>,
    pub tags: Option<String>,
    pub lyrics: Option<String>,
    pub exclude: Option<String>,
    pub instrumental: bool,
    pub persona: Option<String>,
    pub vocal: Option<String>,
    pub start: Option<f64>,
    pub end: Option<f64>,
    pub sliders: Option<ControlSliders>,
    pub request_id: Option<uuid::Uuid>,
}

pub fn validate_range(
    start: Option<f64>,
    end: Option<f64>,
    duration: Option<f64>,
) -> Result<(), CliError> {
    for (name, value) in [("start", start), ("end", end)] {
        if let Some(value) = value
            && (!value.is_finite() || value < 0.0)
        {
            return Err(CliError::InvalidInput(format!(
                "--{name} must be a finite non-negative timestamp"
            )));
        }
    }
    if let Some(end) = end
        && end <= start.unwrap_or(0.0)
    {
        return Err(CliError::InvalidInput("--end must be after --start".into()));
    }
    if let Some(duration) = duration.filter(|d| d.is_finite() && *d > 0.0)
        && (start.is_some_and(|s| s >= duration) || end.is_some_and(|e| e > duration))
    {
        return Err(CliError::InvalidInput(format!(
            "reference timestamps exceed source duration {duration}s"
        )));
    }
    Ok(())
}

pub fn request(
    source: &Clip,
    model: &str,
    options: CoverOptions,
) -> Result<GenerateRequest, CliError> {
    if source.status != "complete" {
        return Err(CliError::InvalidInput(
            "the source song must be complete before covering it".into(),
        ));
    }
    validate_range(options.start, options.end, source.metadata.duration)?;
    let mut req = GenerateRequest::new(model, "custom");
    req.task = Some("cover".into());
    req.cover_clip_id = Some(source.id.clone());
    req.cover_start_s = options.start;
    req.cover_end_s = options.end;
    req.title = options.title.unwrap_or_else(|| source.title.clone());
    req.tags = options
        .tags
        .unwrap_or_else(|| source.metadata.tags.clone().unwrap_or_default());
    let custom_lyrics = options.lyrics.is_some();
    req.prompt = options
        .lyrics
        .unwrap_or_else(|| source.metadata.prompt.clone().unwrap_or_default());
    req.make_instrumental = options.instrumental
        || (!custom_lyrics && source.metadata.make_instrumental)
        || req.prompt.trim().is_empty();
    if options.instrumental {
        req.prompt.clear();
    }
    req.negative_tags = options.exclude.unwrap_or_default();
    req.persona_id = options.persona;
    req.metadata.vocal_gender = options.vocal;
    req.metadata.is_remix = true;
    req.metadata.control_sliders = options.sliders;
    if let Some(id) = options.request_id {
        req.transaction_uuid = id.to_string();
    }
    super::models::validate_offline(&req)?;
    Ok(req)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::clip_reference;
    fn source() -> Clip {
        serde_json::from_value(serde_json::json!({
            "id":"aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee", "title":"Original", "status":"complete", "model_name":"chirp-hawk",
            "audio_url":null,"video_url":null,"image_url":null,"created_at":"2026-09-30T00:00:00Z",
            "metadata":{"prompt":"[Verse]\nKeep this melody", "tags":"pop", "duration":120}
        })).unwrap()
    }
    #[test]
    fn cover_preserves_source_and_uses_the_reference_task() {
        let req = request(&source(), "chirp-hawk", CoverOptions::default()).unwrap();
        let value = serde_json::to_value(req).unwrap();
        assert_eq!(value["task"], "cover");
        assert_eq!(value["metadata"]["create_mode"], "custom");
        assert_eq!(value["metadata"]["is_remix"], true);
        assert_eq!(value["prompt"], "[Verse]\nKeep this melody");
        assert_eq!(value["title"], "Original");
        assert_eq!(value["cover_clip_id"], source().id);
    }
    #[test]
    fn instrumental_cover_removes_inherited_lyrics() {
        let req = request(
            &source(),
            "chirp-hawk",
            CoverOptions {
                instrumental: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(req.prompt.is_empty());
        assert!(req.make_instrumental);
    }
    #[test]
    fn invalid_ranges_and_unfinished_sources_are_rejected() {
        for (start, end) in [
            (Some(f64::NAN), None),
            (Some(-1.0), None),
            (Some(30.0), Some(20.0)),
            (None, Some(121.0)),
        ] {
            assert!(
                request(
                    &source(),
                    "chirp-hawk",
                    CoverOptions {
                        start,
                        end,
                        ..Default::default()
                    }
                )
                .is_err()
            );
        }
        let mut clip = source();
        clip.status = "streaming".into();
        assert!(request(&clip, "chirp-hawk", CoverOptions::default()).is_err());
    }
    #[test]
    fn references_accept_only_suno_song_urls_or_uuids() {
        let id = source().id;
        assert_eq!(
            clip_reference(&format!("https://suno.com/song/{id}?share=1")).unwrap(),
            id
        );
        assert!(clip_reference(&format!("https://evil-suno.com/song/{id}")).is_err());
        assert!(clip_reference("../billing/info").is_err());
    }
}
