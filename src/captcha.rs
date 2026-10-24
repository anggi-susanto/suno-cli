//! Suno captcha adapters: current Turnstile and the site's hCaptcha fallback.
//! HTTP preflight chooses the provider. Chrome is started only for a required
//! challenge; --headless prohibits headed fallback and --no-browser skips this
//! module entirely. Each process owns an isolated temporary profile and closes
//! its Chrome on exit. Tokens are single-use and never printed.

use std::collections::HashSet;
use std::process::Stdio;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::Message;

use crate::auth::AuthState;
use crate::errors::CliError;

static SOLVER_QUIET: AtomicBool = AtomicBool::new(false);
macro_rules! solver_log {
    ($($arg:tt)*) => { if !SOLVER_QUIET.load(Ordering::Relaxed) { eprintln!($($arg)*); } };
}

/// Suno's hCaptcha sitekey, captured from the live web app's
/// `hcaptcha.render(...)` arguments on 2026-04-08.
const SUNO_HCAPTCHA_SITEKEY: &str = "d65453de-3f1a-4aac-9366-a0f06e52b2ce";
/// Preferred port for the suno-cli managed Chrome instance. Picked high to
/// avoid colliding with the user's main Chrome (which is rarely on 9233). If a
/// non-Chrome service already holds it, we fall back to an OS-assigned port
/// tracked in `ACTIVE_PORT`.
const CDP_PORT: u16 = 9233;
const CDP_HOST: &str = "127.0.0.1";

/// The loopback port our managed Chrome is actually reachable on. Starts at
/// `CDP_PORT`; switches to a free port only when something we can't identify
/// as Chrome already occupies the preferred one.
static ACTIVE_PORT: AtomicU16 = AtomicU16::new(CDP_PORT);

fn active_port() -> u16 {
    ACTIVE_PORT.load(Ordering::Relaxed)
}

/// How the solver Chrome is displayed. Headless is tried first (fully
/// invisible); Headed is the offscreen-window fallback for when hCaptcha
/// flags headless.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ChromeMode {
    Headless,
    Headed,
}

impl ChromeMode {
    fn label(self) -> &'static str {
        match self {
            Self::Headless => "headless",
            Self::Headed => "offscreen",
        }
    }
}

/// Singleton holder for the Chrome process (and the mode it was spawned in)
/// so the same instance survives across multiple `generate()` calls within
/// one CLI invocation. Killed by `shutdown()` when the CLI exits.
static CHROME: OnceLock<Mutex<Option<(Child, ChromeMode, tempfile::TempDir)>>> = OnceLock::new();

fn chrome_slot() -> &'static Mutex<Option<(Child, ChromeMode, tempfile::TempDir)>> {
    CHROME.get_or_init(|| Mutex::new(None))
}

/// Serializes solver runs across every captcha-gated command in one process.
/// generate/describe/cover/remaster all pilot the same Chrome page, and an
/// hCaptcha token is single-use — two concurrent solves would race the DOM or
/// hand back a token another arm already spent.
static SOLVE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn solve_lock() -> &'static Mutex<()> {
    SOLVE_LOCK.get_or_init(|| Mutex::new(()))
}

/// Solve a fresh hCaptcha challenge and return the token to attach to a
/// `/api/generate/v2-web/` request body.
///
/// Headless first; if hCaptcha rejects it (it fingerprints headless Chrome
/// intermittently), fall back once to a headed instance parked offscreen.
/// `SUNO_CAPTCHA_HEADLESS=1` / `SUNO_CAPTCHA_HEADED=1` pin a mode.
pub async fn solve(
    auth: &AuthState,
    headless_only: bool,
    provider: u8,
    quiet: bool,
) -> Result<(String, u8), CliError> {
    let _timing = crate::timings::span("captcha.total");
    let wait = crate::timings::span("captcha.lock_wait");
    let _serialized = solve_lock().lock().await;
    drop(wait);
    SOLVER_QUIET.store(quiet, Ordering::Relaxed);
    let pin_headless = headless_only || env_flag("SUNO_CAPTCHA_HEADLESS");
    let pin_headed = !pin_headless && env_flag("SUNO_CAPTCHA_HEADED");
    let first = if pin_headed {
        ChromeMode::Headed
    } else {
        ChromeMode::Headless
    };
    // Suno's current client also latches an hCaptcha fallback after a
    // Turnstile failure. Return the provider that actually produced the token.
    let providers: &[u8] = if provider == 2 { &[2, 1] } else { &[1] };
    let mut last = None;
    let mut attempt = 0;
    for &p in providers {
        attempt += 1;
        match measured_solve(first, auth, p, attempt).await {
            Ok(token) => return Ok((token, p)),
            Err(e) => {
                solver_log!("Captcha provider {p} failed: {e}");
                last = Some(e);
                shutdown().await;
            }
        }
    }
    if first == ChromeMode::Headless && !pin_headless {
        solver_log!("Retrying captcha with the offscreen browser fallback...");
        match measured_solve(ChromeMode::Headed, auth, 1, attempt + 1).await {
            Ok(token) => return Ok((token, 1)),
            Err(e) => last = Some(e),
        }
    }
    Err(CliError::Recovery { source:Box::new(last.unwrap()), suggestion:
        "Retry without --headless to allow the offscreen fallback, or supply a solved --token with --token-provider 1 (hCaptcha) or 2 (Turnstile). No generation was submitted.".into() })
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v == "1")
}

async fn measured_solve(
    mode: ChromeMode,
    auth: &AuthState,
    provider: u8,
    number: u8,
) -> Result<String, CliError> {
    let mut timing = crate::timings::span("captcha.attempt").attempt(
        number,
        if mode == ChromeMode::Headless {
            "headless"
        } else {
            "headed"
        },
        provider,
    );
    let result = solve_in(mode, auth, provider).await;
    timing.outcome(match &result {
        Ok(_) => "success",
        Err(error) => error.error_code(),
    });
    result
}

async fn solve_in(mode: ChromeMode, auth: &AuthState, provider: u8) -> Result<String, CliError> {
    ensure_chrome_running(mode).await?;
    let target = find_or_create_suno_tab().await?;
    // A malicious local listener on the debug port can hand back a remote
    // debugger URL to siphon the injected Clerk cookie. Only attach to
    // loopback.
    if !is_loopback_ws(&target.web_socket_debugger_url) {
        return Err(CliError::Config(format!(
            "CDP returned a non-loopback debugger URL ({}) — refusing to send Suno cookies off-host",
            target.web_socket_debugger_url
        )));
    }
    render_and_execute(&target.web_socket_debugger_url, auth, provider).await
}

/// Kill the Chrome this process spawned, if any. Called on every CLI exit
/// path (and before a mode-fallback respawn) — the solver must never outlive
/// the invocation. Instances we didn't spawn are never touched.
pub async fn shutdown() {
    let mut slot = chrome_slot().lock().await;
    if let Some((mut child, _, _profile)) = slot.take() {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
}

/// Reject a CDP target whose debugger URL is not loopback. Parses the
/// `ws://host:port/...` authority by hand (no url dep) and accepts only
/// 127.0.0.0/8, `::1`, or `localhost`.
fn is_loopback_ws(ws_url: &str) -> bool {
    let Some(rest) = ws_url
        .strip_prefix("ws://")
        .or_else(|| ws_url.strip_prefix("wss://"))
    else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or("");
    // Drop any userinfo, then split host from port. IPv6 literals are bracketed.
    let hostport = authority.rsplit('@').next().unwrap_or(authority);
    let host = if let Some(v6) = hostport.strip_prefix('[') {
        v6.split(']').next().unwrap_or("")
    } else {
        hostport
            .rsplit_once(':')
            .map(|(h, _)| h)
            .unwrap_or(hostport)
    };
    // Parse as an IP so `127.0.0.1.evil.com` (a resolvable non-loopback name
    // that merely starts with "127.") can't slip through a prefix check.
    if host == "localhost" {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

/// Detect a CDP endpoint answering on the solver's preferred port. The CLI
/// kills its own Chrome on exit, so a hit means a crashed run (or some other
/// tool) left an instance behind. Report only — solves never attach to an
/// instance this process didn't spawn, so a leftover is inert.
pub async fn detect_solver_chrome() -> Option<u16> {
    cdp_version().await.ok().map(|_| CDP_PORT)
}

/// Reuse the managed Chrome when it's alive and in the requested mode;
/// otherwise (re)spawn one. Only ever attaches to an instance this process
/// spawned — anything else squatting on the preferred port (a crashed run's
/// leftover, an unrelated service) just pushes us to an OS-assigned port, so
/// our Suno cookies can't reach a stranger.
async fn ensure_chrome_running(mode: ChromeMode) -> Result<(), CliError> {
    let _timing = crate::timings::span("browser.ensure_running");
    {
        let mut slot = chrome_slot().lock().await;
        if let Some((_, running_mode, _)) = slot.as_ref() {
            if *running_mode == mode && cdp_version().await.is_ok() {
                let _reuse = crate::timings::span("browser.reused");
                return Ok(());
            }
            // Wrong mode or dead — replace it.
            if let Some((mut child, _, _profile)) = slot.take() {
                let _ = child.start_kill();
                let _ = child.wait().await;
            }
        }
    }

    // Bind-test the preferred port: free → use it, occupied → OS-assigned.
    let port = match std::net::TcpListener::bind((CDP_HOST, CDP_PORT)) {
        Ok(probe) => {
            drop(probe);
            CDP_PORT
        }
        Err(_) => free_loopback_port()?,
    };
    ACTIVE_PORT.store(port, Ordering::Relaxed);

    let _launch = crate::timings::span("browser.launch_to_cdp_ready");
    let chrome_path = locate_chrome()?;
    // Never share a live profile across separate CLI processes. Chrome's
    // singleton lock otherwise makes concurrent create/cover commands launch
    // into each other's process and miss the requested CDP port.
    let profiles = crate::config::data_dir().join("chrome-profiles");
    std::fs::create_dir_all(&profiles)?;
    let profile = tempfile::Builder::new()
        .prefix("suno-")
        .tempdir_in(profiles)?;
    let profile_dir = profile.path();

    // Either mode keeps a desktop-sized viewport: a 1x1 window makes Suno
    // serve its mobile interstitial, which never loads hCaptcha.
    solver_log!(
        "Launching {} Chrome for captcha solver (cleaned up on exit)...",
        mode.label()
    );

    let mut cmd = Command::new(&chrome_path);
    cmd.arg(format!("--remote-debugging-port={port}"))
        .arg(format!("--user-data-dir={}", profile_dir.display()))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-search-engine-choice-screen")
        .arg("--disable-features=TranslateUI")
        .arg("--window-size=1280,900");
    match mode {
        ChromeMode::Headless => {
            cmd.arg("--headless=new");
        }
        ChromeMode::Headed => {
            cmd.arg("--window-position=-32000,-32000")
                .arg("--silent-launch");
        }
    }
    let mut child = cmd
        .arg("about:blank")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| CliError::Config(format!("failed to spawn Chrome at {chrome_path:?}: {e}")))?;
    drain_stderr(&mut child);

    {
        let mut slot = chrome_slot().lock().await;
        *slot = Some((child, mode, profile));
    }

    // Wait up to 10s for CDP to come up, and insist the responder is really
    // our Chromium (defense-in-depth against a port race).
    for _ in 0..20 {
        sleep(Duration::from_millis(500)).await;
        if let Ok(ver) = cdp_version().await
            && cdp_looks_like_chrome(&ver)
        {
            return Ok(());
        }
    }

    Err(CliError::Config(
        "Chrome was spawned but never opened the CDP port. Check that Chrome can start normally, or set SUNO_CHROME_PATH to a Chrome/Chromium binary.".into(),
    ))
}

/// Locate a Chrome binary on the host. Looks in the usual macOS / Linux /
/// Windows install paths and falls back to `$PATH`. Also used by `doctor`
/// to report solver availability without spawning anything.
pub(crate) fn locate_chrome() -> Result<String, CliError> {
    if let Ok(path) = std::env::var("SUNO_CHROME_PATH")
        && !path.trim().is_empty()
    {
        if std::path::Path::new(&path).exists() {
            return Ok(path);
        }
        return Err(CliError::Config(format!(
            "SUNO_CHROME_PATH points to a missing file: {path}"
        )));
    }

    let candidates: &[&str] = if cfg!(target_os = "macos") {
        &[
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
        ]
    } else if cfg!(target_os = "linux") {
        &[
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
            "/snap/bin/chromium",
        ]
    } else {
        &[
            "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
            "C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe",
        ]
    };
    for c in candidates {
        if std::path::Path::new(c).exists() {
            return Ok(c.to_string());
        }
    }
    Err(CliError::Config(
        "Could not find a Chrome/Chromium binary. Install Google Chrome or set SUNO_CHROME_PATH."
            .into(),
    ))
}

/// CDP target descriptor returned by `/json/list`.
#[derive(Debug, Deserialize)]
struct Target {
    #[serde(rename = "type")]
    target_type: String,
    url: String,
    #[serde(rename = "webSocketDebuggerUrl")]
    web_socket_debugger_url: String,
}

/// A CDP `/json/version` payload that names a Chromium-family browser. Guards
/// against handing Suno cookies to an unrelated local service that happens to
/// answer the debug port.
fn cdp_looks_like_chrome(version: &serde_json::Value) -> bool {
    version
        .get("Browser")
        .and_then(|b| b.as_str())
        .map(|b| {
            let b = b.to_ascii_lowercase();
            b.contains("chrome") || b.contains("chromium")
        })
        .unwrap_or(false)
}

/// Reserve an OS-assigned loopback port, then release it for Chrome to bind.
/// The brief release→bind window is acceptable for a single-user debug port.
fn free_loopback_port() -> Result<u16, CliError> {
    let listener = std::net::TcpListener::bind((CDP_HOST, 0))
        .map_err(|e| CliError::Config(format!("could not reserve a debug port: {e}")))?;
    listener
        .local_addr()
        .map(|a| a.port())
        .map_err(|e| CliError::Config(format!("debug port address: {e}")))
}

async fn cdp_version() -> Result<serde_json::Value, CliError> {
    let port = active_port();
    let url = format!("http://{CDP_HOST}:{port}/json/version");
    let resp = reqwest::Client::new()
        .get(&url)
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .map_err(|e| CliError::Config(format!("CDP /json/version: {e}")))?;
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| CliError::Config(format!("CDP json parse: {e}")))?;
    Ok(v)
}

async fn cdp_list() -> Result<Vec<Target>, CliError> {
    let port = active_port();
    let url = format!("http://{CDP_HOST}:{port}/json/list");
    let resp = reqwest::Client::new()
        .get(&url)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .map_err(|e| CliError::Config(format!("CDP /json/list: {e}")))?;
    let list: Vec<Target> = resp
        .json()
        .await
        .map_err(|e| CliError::Config(format!("CDP json parse: {e}")))?;
    Ok(list)
}

/// Find the existing page in the managed Chrome, or open a new blank one.
/// Navigation to suno.com/create happens only after we have cleared stale
/// cookies and installed the small cookie subset the captcha page needs.
async fn find_or_create_suno_tab() -> Result<Target, CliError> {
    let _timing = crate::timings::span("browser.find_tab");
    let targets = cdp_list().await?;
    if let Some(t) = targets.into_iter().find(|t| {
        t.target_type == "page"
            && !t.web_socket_debugger_url.is_empty()
            && !t.url.starts_with("chrome://")
    }) {
        return Ok(t);
    }

    // No page target — open a blank one. CDP exposes a /json/new?url= helper.
    let url = format!(
        "http://{CDP_HOST}:{}/json/new?{}",
        active_port(),
        urlencode("about:blank")
    );
    let resp = reqwest::Client::new()
        .put(&url)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| CliError::Config(format!("CDP /json/new: {e}")))?;
    let t: Target = resp
        .json()
        .await
        .map_err(|e| CliError::Config(format!("CDP /json/new parse: {e}")))?;
    // Give the page a moment to start loading before we attach
    sleep(Duration::from_millis(800)).await;
    Ok(t)
}

fn urlencode(s: &str) -> String {
    s.replace(":", "%3A").replace("/", "%2F")
}

/// CDP request envelope.
#[derive(Serialize)]
struct CdpReq<'a> {
    id: u64,
    method: &'a str,
    params: serde_json::Value,
}

/// CDP cookie struct (subset of Network.CookieParam).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CdpCookie {
    name: String,
    value: String,
    domain: String,
    path: String,
    secure: bool,
    http_only: bool,
    same_site: &'static str,
}

type CdpStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Send a CDP method and wait for the response (drains intervening events).
async fn cdp_call(
    ws: &mut CdpStream,
    id: u64,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, CliError> {
    let req = CdpReq { id, method, params };
    let payload = serde_json::to_string(&req).unwrap();
    ws.send(Message::Text(payload))
        .await
        .map_err(|e| CliError::Config(format!("CDP ws send {method}: {e}")))?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let msg = tokio::time::timeout_at(deadline, ws.next())
            .await
            .map_err(|_| CliError::Config(format!("CDP {method} timeout")))?
            .ok_or_else(|| CliError::Config(format!("CDP {method} ws closed")))?
            .map_err(|e| CliError::Config(format!("CDP {method} ws err: {e}")))?;
        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Binary(_) | Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {
                continue;
            }
            Message::Close(_) => {
                return Err(CliError::Config(format!("CDP {method} ws closed mid-call")));
            }
        };
        let v: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| CliError::Config(format!("CDP {method} json: {e}")))?;
        if v.get("id").and_then(|x| x.as_u64()) == Some(id) {
            if let Some(err) = v.get("error") {
                return Err(CliError::Config(format!("CDP {method} error: {err}")));
            }
            return Ok(v.get("result").cloned().unwrap_or(serde_json::Value::Null));
        }
        // Ignore unrelated events
    }
}

/// Connect to the page websocket, inject cookies, navigate to suno.com/create
/// if needed, then render an invisible hCaptcha widget and call
/// `hcaptcha.execute()` to obtain a token.
async fn render_and_execute(
    ws_url: &str,
    auth: &AuthState,
    provider: u8,
) -> Result<String, CliError> {
    let connect = crate::timings::span("browser.cdp_connect");
    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url)
        .await
        .map_err(|e| CliError::Config(format!("CDP ws connect: {e}")))?;

    drop(connect);
    let setup = crate::timings::span("browser.page_setup");
    let mut next_id: u64 = 0;
    let mut next = || -> u64 {
        next_id += 1;
        next_id
    };

    cdp_call(&mut ws, next(), "Network.enable", serde_json::json!({})).await?;
    cdp_call(&mut ws, next(), "Page.enable", serde_json::json!({})).await?;
    cdp_call(&mut ws, next(), "Runtime.enable", serde_json::json!({})).await?;
    cdp_call(
        &mut ws,
        next(),
        "Emulation.setDeviceMetricsOverride",
        serde_json::json!({
            "width": 1280,
            "height": 900,
            "deviceScaleFactor": 1,
            "mobile": false
        }),
    )
    .await?;

    // Keep only the Suno-origin cookie set in our isolated profile. Duplicate
    // Clerk/analytics cookies can cause HTTP 431 before the captcha SDK loads.
    clear_suno_cookies(&mut ws, &mut next).await?;

    // Inject only the narrow Suno/Clerk cookie subset required to let the web
    // app initialize. Replaying the whole browser Cookie header is brittle and
    // can exceed Suno/Clerk request-header limits.
    let cookies = extract_cookies(auth)?;
    if !cookies.is_empty() {
        cdp_call(
            &mut ws,
            next(),
            "Network.setCookies",
            serde_json::json!({ "cookies": cookies }),
        )
        .await?;
    }

    drop(setup);
    let navigate = crate::timings::span("browser.navigate_dispatch");
    cdp_call(
        &mut ws,
        next(),
        "Page.navigate",
        serde_json::json!({ "url": "https://suno.com/create" }),
    )
    .await?;

    drop(navigate);
    let readiness = crate::timings::span("browser.sdk_ready");
    if provider == 2 {
        cdp_call(&mut ws, next(), "Runtime.evaluate", serde_json::json!({
            "expression": "if (!window.turnstile && !document.querySelector('script[src*=\"challenges.cloudflare.com/turnstile\"]')) { const s = document.createElement('script'); s.src='https://challenges.cloudflare.com/turnstile/v0/api.js?render=explicit'; document.head.appendChild(s); }",
            "returnByValue": true,
        })).await?;
    }
    let ready_expression = if provider == 2 {
        "typeof turnstile !== 'undefined' && !!turnstile.render"
    } else {
        "typeof hcaptcha !== 'undefined' && !!hcaptcha.render"
    };
    // Wait for the provider SDK (up to 30s).
    let mut ready = false;
    for _ in 0..30 {
        sleep(Duration::from_secs(1)).await;
        let probe = cdp_call(
            &mut ws,
            next(),
            "Runtime.evaluate",
            serde_json::json!({
                "expression": ready_expression,
                "returnByValue": true,
            }),
        )
        .await?;
        if probe
            .get("result")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            ready = true;
            break;
        }
    }
    if !ready {
        let page_state = page_state_excerpt(&mut ws, &mut next).await?;
        return Err(CliError::Config(format!(
            "captcha provider {provider} never finished loading on suno.com/create ({page_state})"
        )));
    }
    drop(readiness);
    let settle = crate::timings::span("browser.sdk_settle");
    // Extra settle so the SDK is fully wired up.
    sleep(Duration::from_secs(2)).await;

    drop(settle);
    let _challenge = crate::timings::span("captcha.challenge");
    // Render an invisible widget and execute it
    let solve_js = if provider == 2 {
        r#"(async () => {
            return await new Promise(resolve => {
                const div = document.createElement('div');
                div.style.cssText='position:fixed;bottom:16px;right:16px;z-index:2147483647;';
                document.body.appendChild(div);
                let widget; let settled=false;
                const finish = value => { if(settled) return; settled=true; clearTimeout(timer); resolve(value); };
                const timer=setTimeout(()=>finish('ERR:Turnstile verification timed out'),55000);
                try {
                    widget=turnstile.render(div, {
                        sitekey:'0x4AAAAAADI7xDNyj-3LcIbi', execution:'execute', appearance:'interaction-only',
                        callback: token => finish(token),
                        'error-callback': code => finish('ERR:Turnstile '+code),
                        'timeout-callback': ()=>finish('ERR:Turnstile challenge timed out')
                    });
                    turnstile.execute(widget);
                } catch(e) { finish('ERR:'+String(e)); }
            });
        })()"#.to_string()
    } else {
        format!(
            r#"
        (async () => {{
            try {{
                const div = document.createElement('div');
                div.style.cssText = 'position:fixed;top:-9999px;left:-9999px;';
                document.body.appendChild(div);
                const id = hcaptcha.render(div, {{
                    sitekey: '{SUNO_HCAPTCHA_SITEKEY}',
                    size: 'invisible',
                    sentry: false,
                    endpoint: 'https://hcaptcha-endpoint-prod.suno.com',
                    assethost: 'https://hcaptcha-assets-prod.suno.com',
                    imghost: 'https://hcaptcha-imgs-prod.suno.com',
                    reportapi: 'https://hcaptcha-reportapi-prod.suno.com',
                }});
                const r = await hcaptcha.execute(id, {{ async: true }});
                return (r && r.response) ? r.response : '';
            }} catch (e) {{
                return 'ERR:' + String(e);
            }}
        }})()
        "#
        )
    };

    let result = cdp_call(
        &mut ws,
        next(),
        "Runtime.evaluate",
        serde_json::json!({
            "expression": solve_js,
            "awaitPromise": true,
            "returnByValue": true,
        }),
    )
    .await?;

    let token = result
        .get("result")
        .and_then(|r| r.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if token.is_empty() {
        return Err(CliError::Config("captcha returned empty token".into()));
    }
    if token.starts_with("ERR:") {
        return Err(CliError::Config(format!(
            "captcha provider {provider}: {token}"
        )));
    }
    Ok(token)
}

/// Delete every cookie scoped to a Suno origin, leaving other sites' cookies
/// untouched. Enumerate via `Network.getCookies` for the Suno URLs, then
/// `Network.deleteCookies` by (name, domain, path).
async fn clear_suno_cookies(
    ws: &mut CdpStream,
    next: &mut impl FnMut() -> u64,
) -> Result<(), CliError> {
    let existing = cdp_call(
        ws,
        next(),
        "Network.getCookies",
        serde_json::json!({
            "urls": [
                "https://suno.com/",
                "https://auth.suno.com/",
                "https://studio-api-prod.suno.com/",
            ]
        }),
    )
    .await?;
    let Some(cookies) = existing.get("cookies").and_then(|c| c.as_array()) else {
        return Ok(());
    };
    for ck in cookies {
        let name = ck.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if name.is_empty() {
            continue;
        }
        let mut params = serde_json::json!({ "name": name });
        if let Some(domain) = ck.get("domain").and_then(|v| v.as_str()) {
            params["domain"] = domain.into();
        }
        if let Some(path) = ck.get("path").and_then(|v| v.as_str()) {
            params["path"] = path.into();
        }
        cdp_call(ws, next(), "Network.deleteCookies", params).await?;
    }
    Ok(())
}

/// Snapshot the page URL + visible text for solver failure diagnostics.
async fn page_state_excerpt(
    ws: &mut CdpStream,
    next: &mut impl FnMut() -> u64,
) -> Result<String, CliError> {
    let state = cdp_call(
        ws,
        next(),
        "Runtime.evaluate",
        serde_json::json!({
            "expression": "JSON.stringify({ href: location.href, body: (document.body && document.body.innerText || '').slice(0, 240) })",
            "returnByValue": true,
        }),
    )
    .await?;
    let raw = state
        .get("result")
        .and_then(|r| r.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("{}");
    let parsed: serde_json::Value = serde_json::from_str(raw).unwrap_or_default();
    let href = parsed.get("href").and_then(|v| v.as_str()).unwrap_or("");
    let body = parsed
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .replace(['\n', '\r'], " ");
    if body.is_empty() {
        Ok(format!("page={href}"))
    } else {
        Ok(format!("page={href}; body={body}"))
    }
}

/// Collect the narrow cookie subset the captcha page needs, in CDP
/// `Network.CookieParam` shape. Live browser cookies (via `rookie`) win;
/// stored auth state is the fallback.
fn extract_cookies(auth: &AuthState) -> Result<Vec<CdpCookie>, CliError> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    // Live browser cookies win (they are freshest), but we NEVER stop there: a
    // browser signed out of Suno still carries useless analytics cookies. Always
    // merge stored auth afterward so a missing Clerk session cookie is
    // backfilled from auth.json — push_cookie dedups by (name, domain), so live
    // cookies keep priority over the stored fallback.
    add_live_browser_cookies(&mut out, &mut seen);
    merge_stored_auth(auth, &mut out, &mut seen);
    Ok(out)
}

/// Backfill from stored auth.json: the Clerk `__client` session cookie, the
/// analytics device id, and any captcha-relevant cookies in the saved header.
/// Runs after the live-browser pass so it only fills genuine gaps.
fn merge_stored_auth(
    auth: &AuthState,
    out: &mut Vec<CdpCookie>,
    seen: &mut HashSet<(String, String)>,
) {
    if let Some(clerk) = auth
        .clerk_client_cookie
        .as_deref()
        .filter(|c| !c.trim().is_empty())
    {
        push_cookie(out, seen, "__client", clerk.trim(), "auth.suno.com", true);
        push_cookie(out, seen, "__client", clerk.trim(), ".suno.com", true);
    }
    if let Some(device_id) = auth.device_id.as_deref().filter(|d| !d.trim().is_empty()) {
        push_cookie(
            out,
            seen,
            "ajs_anonymous_id",
            device_id.trim(),
            ".suno.com",
            false,
        );
    }
    if let Some(cookie_header) = auth.cookie.as_deref().filter(|c| !c.trim().is_empty()) {
        add_minimal_cookies_from_header(cookie_header, out, seen);
    }
}

/// A Clerk session cookie — the one cookie the solver truly needs. Without it
/// suno.com/create loads signed out and hCaptcha never renders.
fn is_session_cookie(name: &str) -> bool {
    name == "__client"
        || name == "__session"
        || name.starts_with("__client_")
        || name.starts_with("__session_")
}

fn add_live_browser_cookies(
    out: &mut Vec<CdpCookie>,
    seen: &mut HashSet<(String, String)>,
) -> bool {
    let domains: Vec<String> = vec![
        "suno.com".into(),
        "auth.suno.com".into(),
        ".suno.com".into(),
    ];
    // Search EVERY installed browser and prefer one that actually holds a Clerk
    // session cookie. The first-match-wins scan let a browser carrying only
    // `ajs_anonymous_id` shadow another that was genuinely signed in.
    let mut chosen: Option<(&'static str, Vec<rookie::enums::Cookie>)> = None;
    let mut chosen_has_session = false;
    for (browser_name, result) in [
        ("Chrome", rookie::chrome(Some(domains.clone()))),
        ("Arc", rookie::arc(Some(domains.clone()))),
        ("Brave", rookie::brave(Some(domains.clone()))),
        ("Firefox", rookie::firefox(Some(domains.clone()))),
        ("Edge", rookie::edge(Some(domains.clone()))),
    ] {
        let Ok(cookies) = result else { continue };
        let suno: Vec<_> = cookies
            .into_iter()
            .filter(|c| c.domain.contains("suno.com"))
            .collect();
        if suno.is_empty() {
            continue;
        }
        let has_session = suno.iter().any(|c| is_session_cookie(&c.name));
        if chosen.is_none() || (has_session && !chosen_has_session) {
            chosen = Some((browser_name, suno));
            chosen_has_session = has_session;
        }
        if chosen_has_session {
            // A signed-in browser is as good as this gets — stop scanning.
            break;
        }
    }

    let Some((browser_name, cookies)) = chosen else {
        return false;
    };
    for c in &cookies {
        add_minimal_cookie(&c.name, &c.value, &c.domain, c.http_only, out, seen);
    }
    if !out.is_empty() {
        solver_log!("Using fresh Suno browser cookies from {browser_name}");
    }
    !out.is_empty()
}

fn add_minimal_cookies_from_header(
    cookie_header: &str,
    out: &mut Vec<CdpCookie>,
    seen: &mut HashSet<(String, String)>,
) {
    for part in cookie_header.split(';') {
        let Some((name, value)) = part.trim().split_once('=') else {
            continue;
        };
        add_minimal_cookie(name.trim(), value.trim(), ".suno.com", false, out, seen);
    }
}

fn add_minimal_cookie(
    name: &str,
    value: &str,
    domain: &str,
    http_only: bool,
    out: &mut Vec<CdpCookie>,
    seen: &mut HashSet<(String, String)>,
) {
    if name.is_empty() || value.is_empty() || !is_captcha_cookie(name) {
        return;
    }

    if name == "__client" || name.starts_with("__client_") {
        push_cookie(out, seen, name, value, "auth.suno.com", true);
        push_cookie(out, seen, name, value, ".suno.com", true);
        return;
    }

    let cookie_domain = if domain.contains("auth.suno.com") {
        "auth.suno.com"
    } else {
        ".suno.com"
    };
    push_cookie(out, seen, name, value, cookie_domain, http_only);
}

fn is_captcha_cookie(name: &str) -> bool {
    // Bare "__client" MUST match: it's the Clerk session cookie itself.
    // Without it the live-browser path loads suno.com/create signed out →
    // sign-in redirect → "hcaptcha never finished loading".
    matches!(
        name,
        "__client"
            | "__session"
            | "clerk_active_context"
            | "ajs_anonymous_id"
            | "suno_device_id"
            | "statsig_stable_id"
            | "ssr_bucket"
            | "has_logged_in_before"
    ) || name.starts_with("__client_")
        || name.starts_with("__session_")
}

fn push_cookie(
    out: &mut Vec<CdpCookie>,
    seen: &mut HashSet<(String, String)>,
    name: &str,
    value: &str,
    domain: &str,
    http_only: bool,
) {
    let key = (name.to_string(), domain.to_string());
    if !seen.insert(key) {
        return;
    }
    out.push(CdpCookie {
        name: name.to_string(),
        value: value.to_string(),
        domain: domain.to_string(),
        path: "/".to_string(),
        secure: true,
        http_only,
        same_site: "Lax",
    });
}

/// Drain the spawned Chrome's stderr in the background — keeps it from
/// blocking on a full pipe and gives us logs for debugging.
fn drain_stderr(child: &mut Child) {
    if let Some(stderr) = child.stderr.take() {
        let mut reader = BufReader::new(stderr).lines();
        tokio::spawn(async move {
            while let Ok(Some(_)) = reader.next_line().await {
                // discard
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_ws_accepts_only_local_hosts() {
        assert!(is_loopback_ws("ws://127.0.0.1:9233/devtools/page/ABCD1234"));
        assert!(is_loopback_ws("ws://localhost:9233/devtools/browser/x"));
        assert!(is_loopback_ws("ws://[::1]:9233/devtools/page/x"));
        assert!(is_loopback_ws("ws://127.9.9.9:1/x"));
        // A remote debugger URL must be rejected before any cookie is sent.
        assert!(!is_loopback_ws("ws://10.0.0.5:9233/devtools/page/x"));
        assert!(!is_loopback_ws("ws://evil.example.com:9233/devtools/x"));
        assert!(!is_loopback_ws("ws://127.0.0.1.evil.com:9233/x"));
        assert!(!is_loopback_ws("http://127.0.0.1:9233/x"));
        assert!(!is_loopback_ws(""));
    }

    #[test]
    fn only_chromium_family_listeners_are_reusable() {
        assert!(cdp_looks_like_chrome(&serde_json::json!({
            "Browser": "Chrome/146.0.0.0"
        })));
        assert!(cdp_looks_like_chrome(&serde_json::json!({
            "Browser": "HeadlessChromium/120"
        })));
        // A stranger answering the debug port must not be trusted with cookies.
        assert!(!cdp_looks_like_chrome(&serde_json::json!({
            "Browser": "my-debug-proxy/1.0"
        })));
        assert!(!cdp_looks_like_chrome(&serde_json::json!({})));
    }

    #[test]
    fn session_cookie_matches_clerk_variants() {
        assert!(is_session_cookie("__client"));
        assert!(is_session_cookie("__session"));
        assert!(is_session_cookie("__client_uat"));
        assert!(is_session_cookie("__session_ABC"));
        assert!(!is_session_cookie("ajs_anonymous_id"));
        assert!(!is_session_cookie("client"));
    }
}
