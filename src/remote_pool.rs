//! Explicit, loopback-only remote download transport. Never calls provider URLs.
use crate::{api::types::Clip, cli::DownloadFormat, errors::CliError};
use futures_util::StreamExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    time::{Duration, Instant},
};
use tokio::io::AsyncWriteExt;

const LIMIT: u64 = 256 * 1024 * 1024;

pub(crate) fn validate_format(format: DownloadFormat) -> Result<(), CliError> {
    if !matches!(format, DownloadFormat::Mp3 | DownloadFormat::Wav) {
        return Err(CliError::InvalidInput(
            "remote-pool supports MP3/WAV only; no fallback or transcoding".into(),
        ));
    }
    Ok(())
}

pub(crate) struct RemotePool {
    base: String,
    token: String,
    http: reqwest::Client,
}
#[derive(Deserialize)]
struct Job {
    job_id: String,
    status: String,
    bytes: Option<u64>,
    sha256: Option<String>,
}
impl RemotePool {
    pub(crate) fn from_env() -> Result<Self, CliError> {
        let base = std::env::var("SUNO_REMOTE_POOL_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8787".into());
        let token_path = std::env::var_os("SUNO_REMOTE_POOL_TOKEN_FILE")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|h| Path::new(&h).join(".suno/remote-downloader/worker-token"))
            })
            .ok_or_else(|| CliError::Config("remote-pool token file not configured".into()))?;
        let token = std::fs::read_to_string(token_path).map_err(|_| CliError::Config("cannot read remote-pool token file; start the local service or set SUNO_REMOTE_POOL_TOKEN_FILE".into()))?;
        Self::new(base, token)
    }
    fn new(base: String, token: String) -> Result<Self, CliError> {
        let url = reqwest::Url::parse(&base)
            .map_err(|_| CliError::Config("invalid remote-pool URL".into()))?;
        if url.scheme() != "http"
            || !matches!(url.host_str(), Some("127.0.0.1" | "[::1]"))
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err(CliError::Config(
                "remote-pool URL must be an HTTP loopback origin (127.0.0.1 or [::1])".into(),
            ));
        }
        let token = token.trim().to_string();
        if token.is_empty() || token.len() > 4096 || token.contains(['\r', '\n']) {
            return Err(CliError::Config("invalid remote-pool token file".into()));
        }
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| CliError::Http(e.without_url()))?;
        Ok(Self {
            base: base.trim_end_matches('/').into(),
            token,
            http,
        })
    }
    pub(crate) async fn preflight(&self) -> Result<(), CliError> {
        let response = self
            .http
            .get(format!("{}/admin/health", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| CliError::Http(e.without_url()))?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(CliError::Config("remote-pool admin API unavailable; safely restart an updated service after active work is reconciled (no fallback)".into()));
        }
        Ok(())
    }
    async fn job(&self, request: reqwest::RequestBuilder) -> Result<Job, CliError> {
        let response = request
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| CliError::Http(e.without_url()))?;
        if !response.status().is_success() {
            return Err(CliError::Download(format!(
                "remote-pool admin API returned {}; check service version/auth (no fallback)",
                response.status()
            )));
        }
        let mut data = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| CliError::Http(e.without_url()))?;
            if data.len() + chunk.len() > 32768 {
                return Err(CliError::Download("remote-pool status too large".into()));
            }
            data.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&data)
            .map_err(|_| CliError::Download("invalid remote-pool status".into()))
    }
    pub(crate) async fn download(
        &self,
        clip: &Clip,
        output: &str,
        format: DownloadFormat,
    ) -> Result<String, CliError> {
        let _timing = crate::timings::span("download.remote_pool");
        validate_format(format)?;
        if clip.status != "complete" {
            return Err(CliError::Download(
                "remote-pool requires a complete clip".into(),
            ));
        }
        uuid::Uuid::parse_str(&clip.id)
            .map_err(|_| CliError::InvalidInput("remote-pool requires a clip UUID".into()))?;
        let mut job = self
            .job(
                self.http
                    .post(format!("{}/admin/jobs", self.base))
                    .json(&serde_json::json!({"content_id":clip.id,"format":format.extension()})),
            )
            .await?;
        if !job.job_id.starts_with("job_")
            || job.job_id.len() != 36
            || !job.job_id[4..].bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(CliError::Download("invalid remote-pool job ID".into()));
        }
        let endpoint = format!("{}/admin/jobs/{}", self.base, job.job_id);
        let expected_job_id = job.job_id.clone();
        let start = Instant::now();
        loop {
            match job.status.as_str() {
                "received" => break,
                "failed" | "cancelled" | "outcome_unknown" => {
                    return Err(CliError::Download(format!(
                        "remote-pool job {} ended {}; reconcile the existing job, do not automatically retry",
                        job.job_id, job.status
                    )));
                }
                _ => {}
            }
            if start.elapsed() >= Duration::from_secs(1800) {
                return Err(CliError::Download(format!(
                    "remote-pool job {} timed out; inspect existing job before retrying",
                    job.job_id
                )));
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
            job = self.job(self.http.get(&endpoint)).await?;
            if job.job_id != expected_job_id {
                return Err(CliError::Download(
                    "remote-pool status job identity mismatch".into(),
                ));
            }
        }
        let bytes = job
            .bytes
            .filter(|n| *n > 0 && *n <= LIMIT)
            .ok_or_else(|| CliError::Download("invalid remote-pool file size".into()))?;
        let digest = job
            .sha256
            .ok_or_else(|| CliError::Download("missing remote-pool digest".into()))?;
        tokio::fs::create_dir_all(output).await?;
        let path = Path::new(output).join(crate::download::clip_filename(
            &clip.title,
            &clip.id,
            format.extension(),
        ));
        let part = path.with_extension(format!("{}.part", uuid::Uuid::new_v4()));
        let result: Result<(), CliError> = async {
            let response = self
                .http
                .get(format!("{endpoint}/file"))
                .bearer_auth(&self.token)
                .send()
                .await
                .map_err(|e| CliError::Http(e.without_url()))?
                .error_for_status()
                .map_err(|e| CliError::Http(e.without_url()))?;
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&part)
                .await?;
            let mut stream = response.bytes_stream();
            let mut hash = Sha256::new();
            let mut count = 0;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| CliError::Http(e.without_url()))?;
                count += chunk.len() as u64;
                if count > bytes {
                    return Err(CliError::Download(
                        "remote-pool file exceeds declared size".into(),
                    ));
                }
                hash.update(&chunk);
                file.write_all(&chunk).await?;
            }
            file.flush().await?;
            file.sync_all().await?;
            drop(file);
            if count != bytes || format!("{:x}", hash.finalize()) != digest {
                return Err(CliError::Download(
                    "remote-pool file size/digest mismatch".into(),
                ));
            }
            crate::download::validate_audio_file(&part, format.extension()).await?;
            // Atomic publication without replacement, including racing CLI processes.
            tokio::fs::hard_link(&part, &path).await?;
            Ok(())
        }
        .await;
        let _ = tokio::fs::remove_file(&part).await;
        result?;
        Ok(std::fs::canonicalize(path)?.display().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn origins_and_formats_are_strict() {
        for origin in [
            "https://127.0.0.1",
            "http://example.com",
            "http://localhost",
            "http://127.0.0.1/x",
            "http://user@127.0.0.1",
            "http://127.0.0.1?secret=x",
        ] {
            assert!(RemotePool::new(origin.into(), "test".into()).is_err());
        }
        assert!(RemotePool::new("http://127.0.0.1:8787".into(), "test".into()).is_ok());
        assert!(validate_format(DownloadFormat::Mp4).is_err());
        assert!(validate_format(DownloadFormat::M4a).is_err());
    }

    #[test]
    fn source_parses_for_every_generation_and_resume_flow() {
        use clap::Parser;
        for argv in [
            vec![
                "generate",
                "--instrumental",
                "--wait",
                "--download",
                "/tmp/music",
            ],
            vec!["describe", "--prompt", "test", "--download", "/tmp/music"],
            vec!["cover", "clip", "--download", "/tmp/music"],
            vec!["remaster", "clip", "--download", "/tmp/music"],
            vec!["extend", "clip", "--at", "30", "--wait"],
            vec!["status", "clip", "--download", "/tmp/music"],
            vec!["download", "clip"],
        ] {
            let mut command = vec!["suno"];
            command.extend(argv);
            command.extend(["--source", "remote-pool"]);
            let cli = crate::cli::Cli::try_parse_from(command).unwrap();
            assert_eq!(cli.source, crate::cli::DownloadSource::RemotePool);
        }
    }

    async fn mock_download(bad_digest: bool, existing: bool, failed: bool) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let audio = b"ID3test-audio";
        let digest = if bad_digest {
            "0".repeat(64)
        } else {
            format!("{:x}", Sha256::digest(audio))
        };
        let job_id = format!("job_{}", "a".repeat(32));
        let status = serde_json::json!({"job_id":job_id,"status":if failed {"failed"} else {"received"},"bytes":audio.len(),"sha256":digest}).to_string();
        let server = std::thread::spawn(move || {
            let mut replies = vec![
                (
                    "POST /admin/jobs ".to_string(),
                    serde_json::json!({"job_id":job_id,"status":"queued"})
                        .to_string()
                        .into_bytes(),
                ),
                (format!("GET /admin/jobs/{job_id} "), status.into_bytes()),
            ];
            if !failed {
                replies.push((format!("GET /admin/jobs/{job_id}/file "), audio.to_vec()));
            }
            for (target, body) in replies {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut data = Vec::new();
                let mut chunk = [0; 4096];
                loop {
                    let n = stream.read(&mut chunk).unwrap();
                    assert!(n > 0);
                    data.extend_from_slice(&chunk[..n]);
                    if let Some(end) = data.windows(4).position(|v| v == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&data[..end]).to_lowercase();
                        let length: usize = headers
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length: "))
                            .map(|s| s.parse().unwrap())
                            .unwrap_or(0);
                        if data.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                let request = String::from_utf8_lossy(&data);
                assert!(request.starts_with(&target));
                assert!(
                    request
                        .to_lowercase()
                        .contains("authorization: bearer test-token")
                );
                if target.starts_with("POST") {
                    assert!(request.contains("\"format\":\"mp3\""));
                }
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let clip: Clip = serde_json::from_value(serde_json::json!({"id":"453c7ddd-9252-4d57-85b9-5310316f8d0d","title":"test","status":"complete","model_name":"chirp-hawk","created_at":"now"})).unwrap();
        let path = dir
            .path()
            .join(crate::download::clip_filename(&clip.title, &clip.id, "mp3"));
        if existing {
            std::fs::write(&path, b"original").unwrap();
        }
        let result = RemotePool::new(base, "test-token".into())
            .unwrap()
            .download(&clip, dir.path().to_str().unwrap(), DownloadFormat::Mp3)
            .await;
        server.join().unwrap();
        assert_eq!(result.is_ok(), !bad_digest && !existing && !failed);
        assert!(
            !std::fs::read_dir(dir.path()).unwrap().any(|p| p
                .unwrap()
                .path()
                .extension()
                .is_some_and(|x| x == "part"))
        );
        if existing {
            assert_eq!(std::fs::read(path).unwrap(), b"original");
        } else if result.is_ok() {
            assert_eq!(std::fs::read(path).unwrap(), audio);
        } else {
            assert!(!path.exists());
        }
    }
    #[tokio::test]
    async fn authenticated_enqueue_poll_receive_and_digest() {
        mock_download(false, false, false).await;
    }
    #[tokio::test]
    async fn rejects_digest_mismatch_and_cleans_partial() {
        mock_download(true, false, false).await;
    }
    #[tokio::test]
    async fn preserves_existing_destination() {
        mock_download(false, true, false).await;
    }
    #[tokio::test]
    async fn failed_worker_never_fetches_or_falls_back() {
        mock_download(false, false, true).await;
    }
}
