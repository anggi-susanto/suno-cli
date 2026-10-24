use std::path::Path;
use std::time::Duration;

use futures_util::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};

use id3::TagLike;

use crate::api::types::{AlignedWord, Clip};
use crate::errors::CliError;

pub async fn download_clip(
    client: &crate::api::SunoClient,
    clip: &Clip,
    output_dir: &str,
    format: crate::cli::DownloadFormat,
    source: crate::cli::DownloadSource,
    quiet: bool,
) -> Result<String, CliError> {
    let _total = crate::timings::span("download.total");
    let prepare = crate::timings::span("download.prepare");
    let source = client.download_source(source, format).await?;
    if source == crate::cli::DownloadSource::RemotePool {
        drop(prepare);
        return crate::remote_pool::RemotePool::from_env()?
            .download(clip, output_dir, format)
            .await;
    }
    let url = client.prepare_download(clip, format, source).await?;
    drop(prepare);
    match transfer(clip, output_dir, format.extension(), &url, quiet).await {
        Err(CliError::Http(e)) if matches!(e.status().map(|s| s.as_u16()), Some(401 | 403)) => {
            let fresh = client
                .prepared_download_url(&clip.id, format, source)
                .await?;
            transfer(clip, output_dir, format.extension(), &fresh, quiet).await
        }
        result => result,
    }
}

async fn transfer(
    clip: &Clip,
    output_dir: &str,
    ext: &str,
    url: &str,
    quiet: bool,
) -> Result<String, CliError> {
    let _transfer = crate::timings::span("download.transfer");
    let filename = clip_filename(&clip.title, &clip.id, ext);
    // Create the target dir up front: generation has already spent credits by
    // the time we download, so a missing `--download` dir must not error out.
    tokio::fs::create_dir_all(output_dir).await?;
    let path = Path::new(output_dir).join(&filename);
    // Stream into a sibling `.part` and only rename into place on full success,
    // so an interrupted or truncated transfer never leaves a file that looks
    // like a finished download.
    let part_path = path.with_extension(format!("{ext}.{}.part", uuid::Uuid::new_v4()));

    // Bounded client: connect timeout, per-read inactivity timeout (catches a
    // stalled CDN mid-stream), and an overall cap. Without these a hung
    // connection would block the download forever.
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|e| CliError::Http(e.without_url()))?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| CliError::Http(e.without_url()))?
        .error_for_status()
        .map_err(|e| CliError::Http(e.without_url()))?;

    let total = resp.content_length().unwrap_or(0);
    let pb = if quiet {
        ProgressBar::hidden()
    } else {
        ProgressBar::new(total)
    };
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{msg} [{bar:40}] {bytes}/{total_bytes} ({eta})")
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("=> "),
    );
    pb.set_message(filename.clone());

    let written = match stream_to_file(&part_path, resp, &pb).await {
        Ok(n) => n,
        Err(e) => {
            let _ = tokio::fs::remove_file(&part_path).await;
            return Err(e);
        }
    };

    // Reject a short read against the advertised size instead of tagging a
    // truncated MP3 downstream.
    if written == 0 || (total > 0 && written != total) {
        let _ = tokio::fs::remove_file(&part_path).await;
        return Err(CliError::Download(format!(
            "incomplete download: received {written} of {total} bytes for {filename}"
        )));
    }

    // A 200 response can still be an expired-link HTML/JSON error page.
    // Check the container before giving the file its final audio extension.
    if let Err(e) = validate_audio_file(&part_path, ext).await {
        let _ = tokio::fs::remove_file(&part_path).await;
        return Err(e);
    }
    if let Err(e) = tokio::fs::rename(&part_path, &path).await {
        let _ = tokio::fs::remove_file(&part_path).await;
        return Err(e.into());
    }
    pb.finish_with_message("done");

    Ok(std::fs::canonicalize(&path)?.display().to_string())
}

pub(crate) async fn validate_audio_file(path: &Path, ext: &str) -> Result<(), CliError> {
    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(path).await?;
    let mut header = [0u8; 12];
    let count = file.read(&mut header).await?;
    let valid = match ext {
        "mp3" => {
            count >= 3
                && (header.starts_with(b"ID3") || (header[0] == 0xff && header[1] & 0xe0 == 0xe0))
        }
        "wav" => {
            count >= 12
                && (header.starts_with(b"RIFF") || header.starts_with(b"RF64"))
                && &header[8..12] == b"WAVE"
        }
        "m4a" | "mp4" => count >= 12 && &header[4..8] == b"ftyp",
        _ => false,
    };
    if !valid {
        return Err(CliError::Download(format!(
            "response is not a valid {ext} container; no final file was saved"
        )));
    }
    Ok(())
}

/// Stream a response body to `part_path`, returning the byte count written.
/// The caller removes the partial file on any error.
async fn stream_to_file(
    part_path: &Path,
    resp: reqwest::Response,
    pb: &ProgressBar,
) -> Result<u64, CliError> {
    use tokio::io::AsyncWriteExt as _;
    let mut file = tokio::fs::File::create(part_path).await?;
    let mut stream = resp.bytes_stream();
    let mut written: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| CliError::Http(e.without_url()))?;
        pb.inc(chunk.len() as u64);
        written += chunk.len() as u64;
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    Ok(written)
}

/// Build `<title-slug>-<id8>.<ext>`. Runs of non-alphanumeric chars collapse
/// to a single `-` (a naive `replace("--", "-")` leaves `--` behind for 3+
/// char runs), and empty/symbol-only titles must not yield a leading dash.
pub(crate) fn clip_filename(title: &str, id: &str, ext: &str) -> String {
    let mut slug = String::new();
    for c in title.to_lowercase().chars() {
        if c.is_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    if slug.ends_with('-') {
        slug.pop();
    }
    // Bound the UTF-8 filename and tolerate unexpected IDs without panics.
    while slug.len() > 180 {
        slug.pop();
    }
    let slug = slug.trim_end_matches('-');
    let short_id: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(8)
        .collect();
    if slug.is_empty() {
        format!("{short_id}.{ext}")
    } else {
        format!("{slug}-{short_id}.{ext}")
    }
}

/// Embed lyrics and metadata into an MP3 file using ID3v2 tags.
/// - USLT: unsynchronized (plain) lyrics — shown in most players
/// - SYLT: synchronized lyrics with word timestamps — shown in Apple Music, Spotify, etc.
/// - TIT2: title, TPE1: artist
pub fn embed_lyrics_in_mp3(
    mp3_path: &str,
    title: &str,
    plain_lyrics: Option<&str>,
    aligned_words: Option<&[AlignedWord]>,
) -> Result<(), CliError> {
    let _tagging = crate::timings::span("download.tagging");
    let mut tag = id3::Tag::read_from_path(mp3_path).unwrap_or_else(|_| id3::Tag::new());

    // Set title
    tag.set_title(title);

    // Plain lyrics (USLT) — shown in most players
    if let Some(lyrics) = plain_lyrics {
        tag.add_frame(id3::frame::Lyrics {
            lang: "eng".to_string(),
            description: String::new(),
            text: lyrics.to_string(),
        });
    }

    // Synchronized lyrics (SYLT) — timed word-by-word display
    if let Some(words) = aligned_words {
        let content: Vec<(u32, String)> = words
            .iter()
            .filter(|w| w.success)
            .map(|w| ((w.start_s * 1000.0) as u32, w.word.clone()))
            .collect();

        if !content.is_empty() {
            tag.add_frame(id3::frame::SynchronisedLyrics {
                lang: "eng".to_string(),
                timestamp_format: id3::frame::TimestampFormat::Ms,
                content_type: id3::frame::SynchronisedLyricsType::Lyrics,
                description: String::new(),
                content,
            });
        }
    }

    tag.write_to_path(mp3_path, id3::Version::Id3v24)
        .map_err(|e| CliError::Download(format!("failed to write ID3 tags: {e}")))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_successful_http_error_pages_before_saving_audio() {
        use std::io::{Read, Write};
        let dir = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 2048];
            let _ = stream.read(&mut request).unwrap();
            let body = "<html>expired signed URL</html>";
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let clip:Clip=serde_json::from_value(serde_json::json!({"id":"test","title":"test","status":"complete","model_name":"chirp-hawk","created_at":"now"})).unwrap();
        let result = transfer(
            &clip,
            dir.path().to_str().unwrap(),
            "mp3",
            &format!("http://{address}/audio"),
            true,
        )
        .await;
        server.join().unwrap();
        assert!(matches!(result, Err(CliError::Download(_))));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn filename_collapses_separator_runs() {
        assert_eq!(
            clip_filename("Hello, World!", "0123456789abcdef", "mp3"),
            "hello-world-01234567.mp3"
        );
        // 3+ char symbol run — the old replace("--","-") left "--" behind.
        assert_eq!(
            clip_filename("a — b", "0123456789abcdef", "mp3"),
            "a-b-01234567.mp3"
        );
    }

    #[test]
    fn filename_handles_empty_and_symbol_only_titles() {
        // No leading dash when the title slugs away to nothing.
        assert_eq!(clip_filename("", "0123456789abcdef", "mp3"), "01234567.mp3");
        assert_eq!(
            clip_filename("!!!", "0123456789abcdef", "mp4"),
            "01234567.mp4"
        );
    }

    #[test]
    fn filename_keeps_unicode_titles() {
        assert_eq!(
            clip_filename("夜の歌 Remix", "0123456789abcdef", "mp3"),
            "夜の歌-remix-01234567.mp3"
        );
        assert_eq!(clip_filename("short", "abc", "mp3"), "short-abc.mp3");
    }
}
