//! Current Suno file preparation, separate from playback and byte transfer.
use super::{SunoClient, types::Clip};
use crate::{
    cli::{DownloadFormat, DownloadSource},
    errors::CliError,
};
use serde::Deserialize;
use std::time::Duration;

#[derive(Debug, Deserialize)]
struct PreparedDownload {
    #[serde(default)]
    ok: bool,
    status: Option<String>,
    download_url: Option<String>,
    detail: Option<String>,
    reason: Option<String>,
}

impl SunoClient {
    pub async fn download_source(
        &self,
        source: DownloadSource,
        format: DownloadFormat,
    ) -> Result<DownloadSource, CliError> {
        if source == DownloadSource::Playback
            && !matches!(format, DownloadFormat::Mp3 | DownloadFormat::Mp4)
        {
            return Err(CliError::InvalidInput(
                "playback supports MP3 and MP4 only; use auto, studio, or library for WAV/M4A"
                    .into(),
            ));
        }
        if source != DownloadSource::Auto {
            return Ok(source);
        }
        if format == DownloadFormat::Mp4 {
            return Ok(DownloadSource::Library);
        }
        let billing = self.billing_info().await?;
        Ok(
            if billing
                .accessible_features
                .iter()
                .any(|f| f.name == "studio")
            {
                DownloadSource::Studio
            } else {
                DownloadSource::Library
            },
        )
    }

    pub async fn prepare_download(
        &self,
        clip: &Clip,
        format: DownloadFormat,
        source: DownloadSource,
    ) -> Result<String, CliError> {
        if clip.status != "complete" {
            return Err(CliError::Download(format!(
                "clip {} is {}; wait for completion with `suno status {} --wait`",
                clip.id, clip.status, clip.id
            )));
        }
        let source = self.download_source(source, format).await?;
        if source == DownloadSource::Playback {
            let url = match format {
                DownloadFormat::Mp4 => clip.video_url.as_deref(),
                _ => clip.audio_url.as_deref(),
            }
            .filter(|url| !url.is_empty())
            .ok_or_else(|| {
                CliError::Download(format!(
                    "clip {} has no {} playback URL",
                    clip.id,
                    format.extension()
                ))
            })?;
            let parsed = reqwest::Url::parse(url)
                .map_err(|_| CliError::Download("invalid playback URL".into()))?;
            if parsed.scheme() != "https" || parsed.host_str().is_none() {
                return Err(CliError::Download("playback URL must use HTTPS".into()));
            }
            return Ok(url.to_owned());
        }
        if source == DownloadSource::Library && clip.is_download_unlocked != Some(true) {
            // Authorize once per invocation, outside all preparation/transfer
            // retries. A later invocation re-fetches the unlock flag first.
            let receipt: serde_json::Value = self
                .with_auth_retry(|| async {
                    let resp = self
                        .post("/api/download/authorize")
                        .json(&serde_json::json!({"item_id": clip.id, "item_type": "clip"}))
                        .send()
                        .await?;
                    Ok(self.check_response(resp).await?.json().await?)
                })
                .await?;
            if receipt["ok"] != true {
                return Err(CliError::Download(format!(
                    "download authorization was not confirmed for {}; check `suno credits` and retry this download",
                    clip.id
                )));
            }
        }
        self.prepared_download_url(&clip.id, format, source).await
    }

    async fn library_wav_url(&self, id: &str) -> Result<String, CliError> {
        let poll = async {
            let mut conversion_requested = false;
            loop {
                let value: serde_json::Value = self
                    .with_auth_retry(|| async {
                        let response = self.get(&format!("/api/gen/{id}/wav_file/")).send().await?;
                        Ok(self.check_response(response).await?.json().await?)
                    })
                    .await?;
                if let Some(url) = value["wav_file_url"].as_str().filter(|u| !u.is_empty()) {
                    if !url.starts_with("https://") {
                        return Err(CliError::Download("WAV download URL must use HTTPS".into()));
                    }
                    return Ok(url.to_string());
                }
                if !conversion_requested {
                    self.with_auth_retry(|| async {
                        let response = self
                            .post(&format!("/api/gen/{id}/convert_wav/"))
                            .send()
                            .await?;
                        self.check_response(response).await?;
                        Ok(())
                    })
                    .await?;
                    conversion_requested = true;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(180), poll)
            .await
            .map_err(|_| {
                CliError::Download(format!(
                    "WAV preparation timed out for {id}; retry the download"
                ))
            })?
    }

    /// Re-prepare an expiring URL without repeating authorization.
    pub async fn prepared_download_url(
        &self,
        id: &str,
        format: DownloadFormat,
        source: DownloadSource,
    ) -> Result<String, CliError> {
        if source == DownloadSource::Playback {
            return Err(CliError::InvalidInput(
                "playback URLs come from clip metadata, not signed preparation".into(),
            ));
        }
        if source == DownloadSource::Library && format == DownloadFormat::Wav {
            return self.library_wav_url(id).await;
        }
        let path = match source {
            DownloadSource::Studio => format!(
                "/api/studio/clip/{id}/download?format={}",
                format.extension()
            ),
            _ => format!("/api/download/clip/{id}?format={}", format.extension()),
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
        let poll = async {
            loop {
                let mut retry_delay = Duration::from_secs(2);
                let prepared = self
                    .with_auth_retry(|| async {
                        let resp = self.get(&path).send().await?;
                        Ok(self
                            .check_response(resp)
                            .await?
                            .json::<PreparedDownload>()
                            .await?)
                    })
                    .await;
                match prepared {
                    Ok(p) if p.ok && p.status.as_deref() == Some("ready") => {
                        let url = p.download_url.filter(|u| !u.is_empty()).ok_or_else(|| {
                            CliError::Download("ready download has no URL".into())
                        })?;
                        let parsed = reqwest::Url::parse(&url).map_err(|_| {
                            CliError::Download("invalid prepared download URL".into())
                        })?;
                        if parsed.scheme() != "https" {
                            return Err(CliError::Download(
                                "prepared download URL must use HTTPS".into(),
                            ));
                        }
                        return Ok(url);
                    }
                    Ok(p)
                        if p.status.as_deref() == Some("processing")
                            || p.status.as_deref() == Some("rate_limited")
                            || p.reason.as_deref() == Some("rate_limited") => {}
                    Ok(p) => {
                        return Err(CliError::Download(p.detail.or(p.reason).unwrap_or_else(
                            || format!("download preparation failed ({:?})", p.status),
                        )));
                    }
                    Err(e) if e.retryable_read() => {
                        retry_delay = e.retry_delay().unwrap_or(retry_delay);
                    }
                    Err(e) => return Err(e),
                }
                tokio::time::sleep(retry_delay).await;
            }
        };
        tokio::time::timeout_at(deadline, poll).await.map_err(|_| {
            CliError::Download(format!(
                "preparation timed out for {id}; retry `suno download {id}`"
            ))
        })?
    }
}
