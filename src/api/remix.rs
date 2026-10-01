//! Reference tasks from Suno's public web request builder (2026-09-30).
use super::{
    cover::validate_range,
    types::{Clip, GenerateRequest},
};
use crate::errors::CliError;

pub fn request(
    source: &Clip,
    mut req: GenerateRequest,
    operation: &str,
    start: Option<f64>,
    end: Option<f64>,
) -> Result<GenerateRequest, CliError> {
    if source.status != "complete" {
        return Err(CliError::InvalidInput(
            "the source song must be complete".into(),
        ));
    }
    validate_range(start, end, source.metadata.duration)?;
    match operation {
        "reuse" => {}
        "add-vocals" => {
            if req.prompt.trim().is_empty() {
                return Err(CliError::InvalidInput(
                    "add-vocals requires --lyrics or --lyrics-file".into(),
                ));
            }
            req.task = Some("overpainting".into());
            req.overpainting_clip_id = Some(source.id.clone());
            req.make_instrumental = false;
        }
        "add-instrumental" => {
            req.task = Some("underpainting".into());
            req.underpainting_clip_id = Some(source.id.clone());
        }
        "replace" => {
            let (start, end) = start.zip(end).ok_or_else(|| {
                CliError::InvalidInput("replace requires --start and --end".into())
            })?;
            let duration = source
                .metadata
                .duration
                .filter(|d| d.is_finite() && *d > 0.0)
                .ok_or_else(|| {
                    CliError::InvalidInput("source song is missing its duration".into())
                })?;
            req.task = Some("infill".into());
            req.continue_clip_id = Some(source.id.clone());
            req.continued_aligned_prompt = Some(req.prompt.clone());
            req.metadata.infill_lyrics = Some(req.prompt.clone());
            req.prompt = source.metadata.prompt.clone().unwrap_or_default();
            req.infill_start_s = Some(start);
            req.infill_end_s = Some(end);
            req.infill_dur_s = Some(end - start);
            req.infill_context_start_s = Some(0.0);
            req.infill_context_end_s = Some(duration);
        }
        _ => return Err(CliError::InvalidInput("unknown remix operation".into())),
    }
    req.metadata.is_remix = operation != "reuse";
    super::models::validate_offline(&req)?;
    Ok(req)
}
