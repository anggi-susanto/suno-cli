//! Validate requests against the account's current model catalogue.
use super::{
    SunoClient,
    types::{GenerateRequest, MaxLengths},
};
use crate::errors::CliError;

pub fn validate_lengths(req: &GenerateRequest, limits: &MaxLengths) -> Result<(), CliError> {
    let prompt_limit = if req.metadata.create_mode == "inspiration" {
        limits.gpt_description_prompt
    } else {
        limits.prompt
    };
    for (name, value, limit) in [
        ("title", req.title.as_str(), limits.title),
        ("tags", req.tags.as_str(), limits.tags),
        ("exclude", req.negative_tags.as_str(), limits.negative_tags),
        ("prompt/lyrics", req.prompt.as_str(), prompt_limit),
    ] {
        // Web forms measure UTF-16 code units, including two units for emoji.
        let count = value.encode_utf16().count();
        if limit > 0 && count > limit as usize {
            return Err(CliError::InvalidInput(format!(
                "{name} is {count} UTF-16 units; model limit is {limit}"
            )));
        }
    }
    if req.metadata.create_mode == "inspiration" && req.prompt.trim().is_empty() {
        return Err(CliError::InvalidInput(
            "description must not be empty".into(),
        ));
    }
    Ok(())
}

pub fn validate_offline(req: &GenerateRequest) -> Result<(), CliError> {
    validate_lengths(
        req,
        &MaxLengths {
            title: 100,
            prompt: 5000,
            tags: 1000,
            negative_tags: 1000,
            gpt_description_prompt: 3000,
        },
    )
}

impl SunoClient {
    pub async fn validate_generation(&self, req: &GenerateRequest) -> Result<(), CliError> {
        let _timing = crate::timings::span("generation.validate");
        let billing = self.billing_info().await?;
        if req.metadata.create_mode == "remaster" {
            // can_use is false on the current remaster catalogue even for
            // eligible subscribers; membership is the authoritative mapping.
            if billing
                .remaster_model_types
                .iter()
                .any(|m| m.external_key == req.mv)
            {
                return Ok(());
            }
        } else if let Some(model) = billing.models.iter().find(|m| m.external_key == req.mv) {
            if !model.can_use {
                return Err(CliError::Config(format!(
                    "{} is unavailable on this account; run `suno models`",
                    model.name
                )));
            }
            return validate_lengths(req, &model.max_lengths);
        }
        Err(CliError::InvalidInput(format!(
            "model '{}' is absent from Suno's current catalogue; run `suno models` and select an available model",
            req.mv
        )))
    }
}
