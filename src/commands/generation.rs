//! Generation lifecycle and recovery output shared by create and status.
use crate::{
    api::SunoClient,
    api::types::GenerateRequest,
    cli::{DownloadFormat, DownloadSource},
    config, download,
    errors::CliError,
    output::{self, OutputFormat},
};

pub(crate) fn generation_recovery(error: CliError, ids: &[String]) -> CliError {
    CliError::Recovery {
        source: Box::new(CliError::Diagnostic {
            source: Box::new(error),
            details: serde_json::json!({"ids":ids,"next_action":{"argv":crate::commands::jobs::resume_argv(ids)}}),
        }),
        suggestion: format!(
            "Resume the existing clips with `suno status {} --wait --download ./songs/`; saved IDs are in `suno jobs`. Do not submit another generation.",
            ids.join(" ")
        ),
    }
}

pub(crate) fn preview_request(req: &GenerateRequest, fmt: OutputFormat) -> Result<(), CliError> {
    let data = serde_json::json!({"dry_run":true, "submitted":false, "model_verified_live":false,
        "request":req, "next_action":"Run the same command without --dry-run to validate against the live catalogue and submit"});
    match fmt {
        OutputFormat::Json => output::json::success(data)?,
        OutputFormat::Table => println!("{}", serde_json::to_string_pretty(&data).unwrap()),
    }
    Ok(())
}

/// Generate, wait, optionally download with lyrics embedding.
/// Poll timing comes from config (`poll_timeout_secs`, `poll_interval_secs`).
pub(crate) async fn handle_generation(
    c: &SunoClient,
    clips: Vec<crate::api::types::Clip>,
    wait: bool,
    download_dir: Option<(&str, DownloadSource)>,
    fmt: OutputFormat,
    quiet: bool,
    cfg: &config::AppConfig,
) -> Result<(), CliError> {
    let ids: Vec<String> = clips.iter().map(|c| c.id.clone()).collect();
    let should_wait = wait || download_dir.is_some();
    let mut clips = if should_wait
        && clips
            .iter()
            .any(|c| !matches!(c.status.as_str(), "complete" | "error"))
    {
        if !quiet {
            eprintln!("Waiting for generation to complete...");
        }
        c.poll_clips(&ids, cfg.poll_timeout_secs, cfg.poll_interval_secs)
            .await
            .map_err(|e| generation_recovery(e, &ids))?
    } else {
        clips
    };
    let mut failure = None;
    if let Some((dir, requested_source)) = download_dir {
        let source = c
            .download_source(requested_source, DownloadFormat::Mp3)
            .await
            .map_err(|e| generation_recovery(e, &ids))?;
        for clip in &mut clips {
            if clip.status != "complete" {
                continue;
            }
            let result: Result<(), CliError> = async {
                let path =
                    download::download_clip(c, clip, dir, DownloadFormat::Mp3, source, quiet)
                        .await?;
                clip.local_path = Some(path.clone());
                let aligned = if clip.metadata.make_instrumental {
                    None
                } else {
                    c.aligned_lyrics(&clip.id).await.ok()
                };
                download::embed_lyrics_in_mp3(
                    &path,
                    &clip.title,
                    clip.metadata.prompt.as_deref(),
                    aligned.as_deref(),
                )?;
                if !quiet {
                    eprintln!("Downloaded: {path}");
                }
                Ok(())
            }
            .await;
            if let Err(e) = result {
                clip.download_error = Some(e.to_string());
                if failure.is_none() {
                    failure = Some(e);
                }
            }
        }
    }
    for clip in &clips {
        if clip.status == "error" && failure.is_none() {
            let reason = clip
                .metadata
                .error_message
                .as_deref()
                .or(clip.metadata.error_type.as_deref())
                .unwrap_or("unknown error");
            failure = Some(CliError::GenerationFailed(format!(
                "{} ({reason})",
                clip.id
            )));
        }
    }
    if let Some(e) = failure {
        return Err(CliError::Diagnostic {
            source: Box::new(generation_recovery(e, &ids)),
            details: serde_json::json!({"ids":ids,"clips":clips,"next_action":{"argv":crate::commands::jobs::resume_argv(&ids)}}),
        });
    }
    match fmt {
        OutputFormat::Json => output::json::success(&clips)?,
        OutputFormat::Table => output::table::clips(&clips),
    }
    Ok(())
}
