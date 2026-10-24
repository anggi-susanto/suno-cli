use super::{SunoClient, types::Clip};
use crate::{
    cli::{DownloadFormat, DownloadSource},
    errors::CliError,
};
use std::{
    collections::VecDeque,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
struct Request {
    method: String,
    target: String,
    body: String,
}

struct Reply {
    status: u16,
    body: &'static str,
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        422 => "Unprocessable Entity",
        503 => "Service Unavailable",
        _ => "Test Response",
    }
}

fn read_request(stream: &mut TcpStream) -> Request {
    stream
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buf = [0_u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut buf).expect("mock request read failed");
        assert!(n > 0, "client closed before finishing request headers");
        bytes.extend_from_slice(&buf[..n]);
        if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]).into_owned();
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    while bytes.len() < header_end + content_length {
        let n = stream
            .read(&mut buf)
            .expect("mock request body read failed");
        assert!(n > 0, "client closed before finishing request body");
        bytes.extend_from_slice(&buf[..n]);
    }
    let request_line = headers.lines().next().expect("missing request line");
    let mut parts = request_line.split_whitespace();
    Request {
        method: parts.next().unwrap().to_string(),
        target: parts.next().unwrap().to_string(),
        body: String::from_utf8_lossy(&bytes[header_end..header_end + content_length]).into_owned(),
    }
}

fn mock_server(replies: Vec<Reply>) -> (String, Arc<Mutex<Vec<Request>>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&requests);
    let handle = thread::spawn(move || {
        let mut replies: VecDeque<_> = replies.into();
        let deadline = Instant::now() + Duration::from_secs(10);
        while let Some(reply) = replies.pop_front() {
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "mock server timed out waiting for request"
                        );
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(e) => panic!("mock accept failed: {e}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            recorded.lock().unwrap().push(read_request(&mut stream));
            let response = format!(
                "HTTP/1.1 {} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                reply.status,
                reason(reply.status),
                reply.body.len(),
                reply.body
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
    });
    (format!("http://{address}"), requests, handle)
}

fn clip(id: &str, status: &str, unlocked: Option<bool>) -> Clip {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "title": "test clip",
        "status": status,
        "model_name": "chirp-hawk",
        "audio_url": null,
        "video_url": null,
        "image_url": null,
        "created_at": "2026-09-28T00:00:00Z",
        "is_download_unlocked": unlocked,
    }))
    .unwrap()
}

fn feed_clip(id: &str, status: &str) -> String {
    serde_json::to_string(&vec![clip(id, status, Some(true))]).unwrap()
}

#[tokio::test]
async fn polling_deadline_bounds_a_stalled_http_response() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (accepted_tx, accepted_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_request(&mut stream);
        accepted_tx.send(request).unwrap();
        release_rx.recv_timeout(Duration::from_secs(4)).unwrap();
    });
    let client = SunoClient::for_test(format!("http://{address}"));
    let ids = vec!["stalled".to_string()];
    let started = Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(3), client.poll_clips(&ids, 1, 1))
        .await
        .expect("poll_clips itself exceeded its bounded deadline");
    let elapsed = started.elapsed();
    assert!(matches!(result, Err(CliError::GenerationPending(_))));
    assert!(
        elapsed < Duration::from_millis(2500),
        "elapsed: {elapsed:?}"
    );
    let request = accepted_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(request.method, "GET");
    assert!(request.target.starts_with("/api/feed/?ids=stalled"));
    release_tx.send(()).unwrap();
    server.join().unwrap();
}

#[tokio::test]
async fn polling_retries_503_and_retains_a_completed_sibling_from_partial_feeds() {
    let first = Box::leak(feed_clip("a", "complete").into_boxed_str());
    let second = Box::leak(feed_clip("b", "complete").into_boxed_str());
    let (base, requests, server) = mock_server(vec![
        Reply {
            status: 503,
            body: "temporary",
        },
        Reply {
            status: 200,
            body: first,
        },
        Reply {
            status: 200,
            body: second,
        },
    ]);
    let client = SunoClient::for_test(base);
    let clips = client
        .poll_clips(&["a".into(), "b".into()], 5, 1)
        .await
        .unwrap();
    server.join().unwrap();
    assert_eq!(clips.len(), 2);
    assert!(clips.iter().any(|c| c.id == "a" && c.status == "complete"));
    assert!(clips.iter().any(|c| c.id == "b" && c.status == "complete"));
    assert_eq!(requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn studio_download_uses_current_route_and_accepts_a_signed_https_url() {
    let signed = "https://cdn.example.invalid/song.wav?X-Amz-Signature=test";
    let body = Box::leak(
        serde_json::json!({"ok": true, "status": "ready", "download_url": signed})
            .to_string()
            .into_boxed_str(),
    );
    let (base, requests, server) = mock_server(vec![Reply { status: 200, body }]);
    let client = SunoClient::for_test(base);
    let url = client
        .prepare_download(
            &clip("clip-id", "complete", Some(true)),
            DownloadFormat::Wav,
            DownloadSource::Studio,
        )
        .await
        .unwrap();
    server.join().unwrap();
    assert_eq!(url, signed);
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(
        requests[0].target,
        "/api/studio/clip/clip-id/download?format=wav"
    );
}

#[tokio::test]
async fn download_rejects_a_pending_clip_before_network_access() {
    let client = SunoClient::for_test("http://127.0.0.1:9".into());
    let err = client
        .prepare_download(
            &clip("pending", "streaming", None),
            DownloadFormat::Mp3,
            DownloadSource::Library,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, CliError::Download(_)));
    assert!(err.to_string().contains("wait for completion"));
}

#[tokio::test]
async fn locked_library_download_authorizes_once_then_polls_processing_to_ready() {
    let ready = r#"{"ok":true,"status":"ready","download_url":"https://cdn.example.invalid/library.mp3?sig=test"}"#;
    let (base, requests, server) = mock_server(vec![
        Reply {
            status: 200,
            body: r#"{"ok":true}"#,
        },
        Reply {
            status: 200,
            body: r#"{"ok":true,"status":"processing"}"#,
        },
        Reply {
            status: 200,
            body: ready,
        },
    ]);
    let client = SunoClient::for_test(base);
    let url = client
        .prepare_download(
            &clip("locked", "complete", Some(false)),
            DownloadFormat::Mp3,
            DownloadSource::Library,
        )
        .await
        .unwrap();
    server.join().unwrap();
    assert!(url.starts_with("https://"));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.iter().filter(|r| r.method == "POST").count(), 1);
    assert_eq!(requests[0].target, "/api/download/authorize");
    assert!(requests[0].body.contains(r#""item_id":"locked""#));
    assert_eq!(requests[1].target, "/api/download/clip/locked?format=mp3");
    assert_eq!(requests[2].target, "/api/download/clip/locked?format=mp3");
}

#[tokio::test]
async fn transient_prepare_failure_does_not_replay_library_authorization() {
    let (base, requests, server) = mock_server(vec![
        Reply {
            status: 200,
            body: r#"{"ok":true}"#,
        },
        Reply {
            status: 503,
            body: "temporary",
        },
        Reply {
            status: 200,
            body: r#"{"ok":true,"status":"ready","download_url":"https://cdn.example.invalid/retry.mp3?sig=test"}"#,
        },
    ]);
    let client = SunoClient::for_test(base);
    client
        .prepare_download(
            &clip("retry", "complete", Some(false)),
            DownloadFormat::Mp3,
            DownloadSource::Library,
        )
        .await
        .unwrap();
    server.join().unwrap();
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.target == "/api/download/authorize")
            .count(),
        1
    );
    assert_eq!(requests.iter().filter(|r| r.method == "POST").count(), 1);
}

#[tokio::test]
async fn http_400_and_422_are_bad_input_exit_three() {
    let (base, _requests, server) = mock_server(vec![
        Reply {
            status: 400,
            body: "bad request",
        },
        Reply {
            status: 422,
            body: "unprocessable",
        },
    ]);
    let client = SunoClient::for_test(base);
    for path in ["/bad-400", "/bad-422"] {
        let response = client.get(path).send().await.unwrap();
        let err = client.check_response(response).await.unwrap_err();
        assert!(matches!(err, CliError::InvalidInput(_)), "{err:?}");
        assert_eq!(err.exit_code(), 3);
    }
    server.join().unwrap();
}

#[tokio::test]
async fn library_wav_starts_conversion_once_and_waits_for_signed_url() {
    let (base, requests, server) = mock_server(vec![
        Reply {
            status: 200,
            body: r#"{"wav_file_url":null}"#,
        },
        Reply {
            status: 200,
            body: "{}",
        },
        Reply {
            status: 200,
            body: r#"{"wav_file_url":"https://cdn.example.invalid/out.wav?sig=test"}"#,
        },
    ]);
    let url = SunoClient::for_test(base)
        .prepare_download(
            &clip("wav", "complete", Some(true)),
            DownloadFormat::Wav,
            DownloadSource::Library,
        )
        .await
        .unwrap();
    server.join().unwrap();
    assert!(url.contains("out.wav"));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].target, "/api/gen/wav/wav_file/");
    assert_eq!(requests[1].method, "POST");
    assert_eq!(requests[1].target, "/api/gen/wav/convert_wav/");
    assert_eq!(requests[2].target, "/api/gen/wav/wav_file/");
}

#[tokio::test]
async fn rate_limit_keeps_the_server_retry_after() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_request(&mut stream);
        stream.write_all(b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 137\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
    });
    let client = SunoClient::for_test(format!("http://{address}"));
    let response = client.get("/api/feed/").send().await.unwrap();
    let err = client.check_response(response).await.unwrap_err();
    server.join().unwrap();
    assert_eq!(err.exit_code(), 4);
    assert_eq!(err.retry_delay(), Some(Duration::from_secs(137)));
    assert!(err.suggestion().contains("137"));
}

#[test]
#[allow(clippy::result_large_err)] // figment Jail requires this error type
fn repeated_request_id_performs_only_one_paid_post() {
    figment::Jail::expect_with(|jail| {
        jail.set_env("SUNO_DATA_DIR", jail.directory().display().to_string());
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let feed=Box::leak(feed_clip("created", "complete").into_boxed_str());
            let submission=Box::leak(format!(r#"{{"clips":{feed}}}"#).into_boxed_str());
            let (base,requests,server)=mock_server(vec![
                Reply{status:200,body:r#"{"total_credits_left":100,"plan":{"name":"test"},"models":[{"name":"v6","external_key":"chirp-hawk","can_use":true}]}"#},
                Reply{status:200,body:submission},
                Reply{status:200,body:feed},
            ]);
            let client=SunoClient::for_test(base);
            let mut req=super::types::GenerateRequest::new("chirp-hawk","custom");
            req.token=Some("test-token".into());
            req.token_provider=Some(2);
            let first=client.generate(&req).await.unwrap();
            req.token=None;
            req.token_provider=None;
            req.metadata.create_session_token="new-session".into();
            let second=client.generate(&req).await.unwrap();
            server.join().unwrap();
            assert_eq!(first[0].id,second[0].id);
            let requests=requests.lock().unwrap();
            assert_eq!(requests.iter().filter(|r| r.method=="POST").count(),1);
            let posted:serde_json::Value=serde_json::from_str(&requests[1].body).unwrap();
            assert_eq!(posted["token_provider"],2);
        });
        Ok(())
    });
}

#[tokio::test]
async fn saved_request_never_silently_loses_ids_to_a_partial_feed() {
    let feed = Box::leak(feed_clip("a", "complete").into_boxed_str());
    let (base, _, server) = mock_server(vec![
        Reply {
            status: 200,
            body: "[]",
        },
        Reply {
            status: 200,
            body: feed,
        },
    ]);
    let client = SunoClient::for_test(base);
    let ids = vec!["a".into(), "b".into()];
    for _ in 0..2 {
        let error = client.get_existing_clips(&ids).await.unwrap_err();
        assert_eq!(error.error_code(), "generation_pending");
        assert!(error.to_string().contains("a b"));
    }
    server.join().unwrap();
}

#[test]
fn removed_playback_source_is_rejected_by_cli() {
    use clap::Parser;
    let error =
        crate::cli::Cli::try_parse_from(["suno", "download", "clip-id", "--source", "playback"])
            .err()
            .expect("removed source must fail");
    assert_eq!(error.kind(), clap::error::ErrorKind::InvalidValue);
}

#[tokio::test]
async fn auto_audio_uses_signed_entitlement_route_not_encoded_metadata() {
    for (features, expected) in [
        (
            r#"[{"name":"studio"}]"#,
            "/api/studio/clip/encoded/download?format=m4a",
        ),
        ("[]", "/api/download/clip/encoded?format=m4a"),
    ] {
        let billing = Box::leak(format!(
            r#"{{"total_credits_left":100,"plan":{{"name":"test"}},"accessible_features":{features}}}"#
        ).into_boxed_str());
        let (base, requests, server) = mock_server(vec![
            Reply {
                status: 200,
                body: billing,
            },
            Reply {
                status: 200,
                body: r#"{"ok":true,"status":"ready","download_url":"https://cdn.example.invalid/signed.m4a"}"#,
            },
        ]);
        let client = SunoClient::for_test(base);
        let mut clip = clip("encoded", "complete", Some(true));
        clip.audio_url = Some("https://cdn.example.invalid/legacy.mp3".into());
        clip.media_urls = vec![super::types::MediaUrl {
            url: "https://cdn.example.invalid/encoded.m4a".into(),
            content_type: "m4a-opus".into(),
            delivery: "progressive".into(),
            encoding: Some("9.9.9".into()),
        }];
        let url = client
            .prepare_download(&clip, DownloadFormat::M4a, DownloadSource::Auto)
            .await
            .unwrap();
        server.join().unwrap();
        assert_eq!(url, "https://cdn.example.invalid/signed.m4a");
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].target, "/api/billing/info/");
        assert_eq!(requests[1].target, expected);
        let json = serde_json::to_value(&clip).unwrap();
        assert_eq!(json["media_urls"][0]["encoding"], "9.9.9");
    }
}

#[tokio::test]
async fn auto_video_uses_library_without_billing_lookup() {
    let (base, requests, server) = mock_server(vec![Reply {
        status: 200,
        body: r#"{"ok":true,"status":"ready","download_url":"https://cdn.example.invalid/signed.mp4"}"#,
    }]);
    let client = SunoClient::for_test(base);
    client
        .prepare_download(
            &clip("video", "complete", Some(true)),
            DownloadFormat::Mp4,
            DownloadSource::Auto,
        )
        .await
        .unwrap();
    server.join().unwrap();
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].target, "/api/download/clip/video?format=mp4");
}

#[tokio::test]
async fn auto_billing_failure_does_not_fall_back_to_metadata_or_remote_pool() {
    let (base, requests, server) = mock_server(vec![Reply {
        status: 400,
        body: "billing unavailable",
    }]);
    let client = SunoClient::for_test(base);
    let mut clip = clip("failure", "complete", Some(true));
    clip.audio_url = Some("https://cdn.example.invalid/legacy.mp3".into());
    assert!(
        client
            .prepare_download(&clip, DownloadFormat::Mp3, DownloadSource::Auto)
            .await
            .is_err()
    );
    server.join().unwrap();
    assert_eq!(requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn signed_preparation_rejects_unresolved_or_remote_sources_without_network() {
    let client = SunoClient::for_test("http://127.0.0.1:9".into());
    for source in [DownloadSource::Auto, DownloadSource::RemotePool] {
        assert!(matches!(
            client
                .prepared_download_url("id", DownloadFormat::Mp3, source)
                .await,
            Err(CliError::InvalidInput(_))
        ));
    }
}
