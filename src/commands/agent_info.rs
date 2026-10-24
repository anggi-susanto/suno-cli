//! Machine-readable capability manifest (agent-cli-framework shape).
//!
//! Always raw JSON, never wrapped in the envelope — an agent calling
//! agent-info is bootstrapping and this IS the schema definition.
//! Every command key must stay routable (`suno <key> --help` exits 0);
//! the conformance probe enforces it.

use clap::{Arg, ArgAction, Command, CommandFactory, ValueEnum};
use serde_json::{Map, Value, json};

use crate::{
    cli::{Cli, ModelVersion, RemasterModel},
    errors::CliError,
    output,
};

/// Model maps are generated from the clap enums so the manifest can never
/// drift from what `--model` actually accepts.
fn model_map() -> serde_json::Map<String, Value> {
    ModelVersion::value_variants()
        .iter()
        .map(|m| (m.display_name().to_string(), m.to_api_key().into()))
        .collect()
}

fn remaster_model_map() -> serde_json::Map<String, Value> {
    RemasterModel::value_variants()
        .iter()
        .map(|m| {
            let name = m.to_possible_value().expect("no skipped variants");
            (name.get_name().to_string(), m.to_api_key().into())
        })
        .collect()
}

/// Older values remain accepted so existing scripts fail gracefully and the
/// manifest remains an exact description of clap. Suno retired these models
/// from the live catalogue on September 9; agents should select from v6.
fn retired_models() -> Vec<Value> {
    [
        "v5.5", "v5", "v4.5+", "v4.5-all", "v4.5", "v4", "v3.5", "v3", "v2",
    ]
    .into_iter()
    .map(Value::from)
    .collect()
}

/// Top-level discovery list for the built-in guides, generated from the
/// `guide` registry so it can never drift from what `suno guide` serves.
fn guide_map() -> Vec<Value> {
    crate::commands::guide::GUIDES
        .iter()
        .map(|g| json!({ "name": g.name, "description": g.description }))
        .collect()
}

/// One `json!` per command: a single macro invocation for the whole map
/// blows the macro recursion limit, and per-command blocks read better.
fn domain_annotations() -> serde_json::Map<String, Value> {
    let models: Vec<String> = ModelVersion::value_variants()
        .iter()
        .map(|m| m.display_name().to_string())
        .collect();

    let model_option = json!({
        "name": "--model", "type": "string", "required": false,
        "default": "v6 (config `default_model`)", "values": models,
        "description": "Model version"
    });
    let wait_option = json!({
        "name": "--wait", "type": "bool", "required": false, "default": false,
        "description": "Block until generation completes (config `poll_timeout_secs` caps the wait)"
    });
    let download_option = json!({
        "name": "--download", "type": "string", "required": false,
        "description": "Download finished MP3s to this directory (lyrics embedded); implies --wait and returned clips include local_path"
    });
    let token_option = json!({
        "name": "--token", "type": "string", "required": false,
        "description": "Pre-solved captcha token (combine with --token-provider to skip preflight and solver)"
    });
    let token_provider_option = json!({
        "name":"--token-provider", "type":"number", "required":false, "values":[1,2],
        "description":"Captcha provider override: 1=hCaptcha, 2=Turnstile. Alias --captcha-provider; inferred from Suno preflight when omitted"
    });
    let no_captcha_option = json!({
        "name": "--no-captcha", "type": "bool", "required": false, "default": false,
        "description": "Never run the captcha solver (headless boxes supplying --token)"
    });
    let force_option = json!({
        "name": "--force", "type": "bool", "required": false, "default": false,
        "description": "Bypass the duplicate-run guard"
    });
    let clip_id_arg = json!({
        "name": "clip_id", "kind": "positional", "type": "string", "required": true,
        "description": "Clip ID"
    });
    let ids_arg = json!({
        "name": "ids", "kind": "positional", "type": "string...", "required": true,
        "description": "One or more clip IDs"
    });

    let commands: Vec<(&str, Value)> = vec![
        (
            "prompt",
            json!({
                "description": "Build a musical brief and generation command offline (free)",
                "args": [],
                "options": [],
                "output_fields": [
                    "brief", "style_prompt", "negative_tags", "instrumental",
                    "missing_directions", "warnings", "controls", "next_action", "guide"
                ],
                "credit_cost": "none — this command is local and does not contact Suno"
            }),
        ),
        (
            "generate",
            json!({
                "description": "Generate music with custom lyrics, tags, and controls",
                "args": [],
                "options": [
                    {"name": "--title", "type": "string", "required": false, "description": "Song title"},
                    {"name": "--tags", "type": "string", "required": false, "description": "Style tags, comma-separated"},
                    {"name": "--exclude", "type": "string", "required": false, "description": "Styles to avoid"},
                    {"name": "--lyrics", "type": "string", "required": false, "description": "Lyrics text with [Verse]/[Chorus] tags"},
                    {"name": "--lyrics-file", "type": "string", "required": false, "description": "Read lyrics from file"},
                    model_option,
                    {"name": "--vocal", "type": "string", "required": false, "values": ["male", "female"], "description": "Vocal gender"},
                    {"name": "--weirdness", "type": "number", "required": false, "description": "0-100"},
                    {"name": "--style-influence", "type": "number", "required": false, "description": "0-100"},
                    {"name": "--audio-influence", "type": "number", "required": false, "description": "0-100"},
                    {"name": "--instrumental", "type": "bool", "required": false, "default": false, "description": "No vocals"},
                    {"name": "--persona", "type": "string", "required": false, "description": "Voice persona UUID"},
                    {"name": "--dry-run", "type": "bool", "required": false, "default": false, "description": "Validate and preview the request offline without authentication, submission, or credits"},
                    {"name": "--max-mode", "type": "bool", "required": false, "default": false, "description": "Ask Suno to spend more compute and credits on this generation"},
                    {"name": "--request-id", "type": "string", "required": false, "description": "Stable UUID for an idempotent retry: reuses saved submission IDs only when the request matches, and rejects mismatched or uncertain reuse"},
                    wait_option, download_option, token_option, token_provider_option, no_captcha_option, force_option
                ],
                "returned_clip_fields": ["id", "status", "local_path"]
            }),
        ),
        (
            "describe",
            json!({
                "description": "Generate music from a text description (Suno writes lyrics)",
                "args": [],
                "options": [
                    {"name": "--prompt", "type": "string", "required": true, "description": "What the song should be"},
                    {"name": "--tags", "type": "string", "required": false, "description": "Style tags"},
                    model_option,
                    {"name": "--vocal", "type": "string", "required": false, "values": ["male", "female"], "description": "Vocal gender"},
                    {"name": "--weirdness", "type": "number", "required": false, "description": "0-100"},
                    {"name": "--style-influence", "type": "number", "required": false, "description": "0-100"},
                    {"name": "--instrumental", "type": "bool", "required": false, "default": false, "description": "No vocals"},
                    {"name": "--persona", "type": "string", "required": false, "description": "Voice persona UUID"},
                    {"name": "--dry-run", "type": "bool", "required": false, "default": false, "description": "Validate and preview the request offline without authentication, submission, or credits"},
                    {"name": "--max-mode", "type": "bool", "required": false, "default": false, "description": "Ask Suno to spend more compute and credits on this generation"},
                    {"name": "--request-id", "type": "string", "required": false, "description": "Stable UUID for an idempotent retry: reuses saved submission IDs only when the request matches, and rejects mismatched or uncertain reuse"},
                    wait_option, download_option, token_option, token_provider_option, no_captcha_option, force_option
                ],
                "returned_clip_fields": ["id", "status", "local_path"]
            }),
        ),
        (
            "lyrics",
            json!({
                "description": "Generate lyrics only (free, no credits used)",
                "args": [],
                "options": [
                    {"name": "--prompt", "type": "string", "required": true, "description": "What the song should be about"}
                ]
            }),
        ),
        (
            "write",
            json!({
                "description": "Compose a Suno-ready song scaffold from the built-in grammar (Style Prompt + meta-tagged lyric skeleton + Suno Tags + a structured next_action for `suno generate`). Free, no credits used",
                "args": [],
                "options": [
                    {"name": "--theme", "type": "string", "required": false, "description": "What the song is about (fills the {theme} placeholders)"},
                    {"name": "--genre", "type": "string", "required": false, "description": "Genre or subgenre (fuzzy match; unknown → used verbatim as a style tag)"},
                    {"name": "--mood", "type": "string", "required": false, "description": "Mood override, e.g. \"bittersweet and hopeful\" (else the genre default)"},
                    {"name": "--vocal", "type": "string", "required": false, "values": ["male", "female"], "description": "Vocal gender direction"},
                    {"name": "--bpm", "type": "number", "required": false, "description": "Tempo in BPM (else the genre's default tempo)"},
                    {"name": "--viral", "type": "bool", "required": false, "default": false, "description": "Add earworm/hook meta-tags and catchiness tags"},
                    {"name": "--instrumental", "type": "bool", "required": false, "default": false, "description": "No vocals and no lyric placeholders; the emitted generate command carries --instrumental"},
                    {"name": "--title", "type": "string", "required": false, "description": "Song title (else derived from the theme)"},
                    {"name": "--mode", "type": "string", "required": false, "default": "songwriting", "values": ["songwriting", "priming"], "description": "Composition mode (unknown → exit 3)"},
                    {"name": "--target", "type": "string", "required": false, "description": "[priming] REQUIRED. Named consenting target or anonymised batch descriptor"},
                    {"name": "--objective", "type": "string", "required": false, "description": "[priming] REQUIRED. Specific, falsifiable priming objective (also seeds the song theme)"},
                    {"name": "--domain", "type": "string", "required": false, "values": ["investment", "marketing", "sales", "political", "health", "other"], "description": "[priming] REQUIRED. Domain of the objective"},
                    {"name": "--subtlety", "type": "string", "required": false, "default": "medium", "values": ["stealth", "medium", "overt"], "description": "[priming] Subtlety dial"},
                    {"name": "--out", "type": "string", "required": false, "description": "Write the lyric block ONLY to FILE — the file `generate --lyrics-file` reads. No headers, tags or metadata"},
                    {"name": "--project-out", "type": "string", "required": false, "description": "Also write the composite human document (title + style prompt + lyrics + tags + priming artefact). Never a generation input; must differ from --out (same path → exit 3)"},
                    {"name": "--download", "type": "string", "required": false, "default": "./", "description": "Download directory baked into the emitted generate command"}
                ],
                "required_fields_by_mode": {
                    "songwriting": [],
                    "priming": ["--target", "--objective", "--domain"]
                },
                "output_schema": {
                    "title": "string", "mode": "songwriting|priming", "genre": "string",
                    "style_prompt": "string — pass verbatim to `generate --tags`; authoritative over suno_tags",
                    "structure": "string — the lyric block; identical to the bytes written to --out",
                    "suno_tags": "string — lower-level comma tag list, derived from the same resolved controls",
                    "structure_tags": "array of the meta-tag vocabulary",
                    "bpm": "number", "vocal": "male|female|null", "theme": "string|null",
                    "viral": "bool", "instrumental": "bool",
                    "placeholders_remaining": "number of unresolved <...> lines in `structure`",
                    "ready_to_generate": "bool — false while placeholders remain or no --out file exists",
                    "missing_requirements": "array of human-readable blockers to clear before generating",
                    "next_action": "{argv: [string], command: string} or null — argv is authoritative, never shell-parse `command`. Null until --out names a real file",
                    "written": "path of the lyrics file (present only with --out)",
                    "project_written": "path of the composite document (present only with --project-out)",
                    "priming": "{target, objective, domain, subtlety, prime_stack_map} (priming mode only)"
                },
                "workflow": [
                    "suno write --genre <g> --theme <t> --out song.txt --json",
                    "edit song.txt: replace every <...> span; keep [Section] tags; repeat the chorus verbatim",
                    "run data.next_action.argv — renders on the configured default model (v6; standard generation is documented as 10 credits for two songs)",
                    "`generate` and `extend` exit 3 if any <...> placeholder survives (unless --allow-placeholders is explicit), so --force stays limited to lock recovery"
                ],
                "raw_output": "human mode without --out: the lyric skeleton ONLY on stdout (safe to redirect or paste as a lyrics input); title, style prompt, suno tags and handoff on stderr. With --out: nothing on stdout, the file holds lyrics only. JSON envelope when piped or --json"
            }),
        ),
        (
            "extend",
            json!({
                "description": "Continue/extend a clip from a timestamp",
                "args": [clip_id_arg],
                "options": [
                    {"name": "--at", "type": "number", "required": true, "description": "Timestamp in seconds to continue from"},
                    {"name": "--lyrics", "type": "string", "required": false, "description": "New lyrics for the extension"},
                    {"name": "--tags", "type": "string", "required": false, "description": "Style tags"},
                    model_option, wait_option, token_option, token_provider_option, no_captcha_option, force_option
                ]
            }),
        ),
        (
            "concat",
            json!({
                "description": "Concatenate an extended clip into a full song",
                "args": [clip_id_arg],
                "options": []
            }),
        ),
        (
            "cover",
            json!({
                "description": "Create a cover of an existing clip",
                "args": [clip_id_arg],
                "options": [
                    {"name": "--tags", "type": "string", "required": false, "description": "Style tags for the cover"},
                    model_option,
                    {"name": "--audio-influence", "type": "number", "required": false, "description": "0-100, how strongly the source clip shapes the cover"},
                    wait_option, download_option, token_option, token_provider_option, no_captcha_option, force_option
                ],
                "returned_clip_fields": ["id", "status", "local_path"]
            }),
        ),
        (
            "remaster",
            json!({
                "description": "Remaster a clip with a different model",
                "args": [clip_id_arg],
                "options": [
                    {"name": "--model", "type": "string", "required": false, "default": "v6",
                     "values": remaster_model_map().keys().cloned().collect::<Vec<_>>(),
                     "description": "Remaster model version"},
                    wait_option, download_option, token_option, token_provider_option, no_captcha_option, force_option
                ],
                "returned_clip_fields": ["id", "status", "local_path"]
            }),
        ),
        (
            "stems",
            json!({
                "description": "Extract stems (vocals, instruments) from a clip",
                "args": [clip_id_arg],
                "options": []
            }),
        ),
        (
            "info",
            json!({
                "description": "Show detailed info for a single clip",
                "args": [{"name": "id", "kind": "positional", "type": "string", "required": true, "description": "Clip ID"}],
                "options": []
            }),
        ),
        (
            "persona",
            json!({
                "description": "View a voice persona",
                "args": [{"name": "id", "kind": "positional", "type": "string", "required": true, "description": "Persona ID"}],
                "options": []
            }),
        ),
        (
            "list",
            json!({
                "description": "List your songs (JSON data: {clips, next_cursor, has_more})",
                "aliases": ["ls"],
                "args": [],
                "options": [
                    {"name": "--cursor", "type": "string", "required": false, "description": "Opaque next_cursor token from a previous page"}
                ]
            }),
        ),
        (
            "search",
            json!({
                "description": "Search your songs by title or tags",
                "args": [{"name": "query", "kind": "positional", "type": "string", "required": true, "description": "Search query"}],
                "options": []
            }),
        ),
        (
            "status",
            json!({
                "description": "Check or wait for existing generation IDs; never submits a generation",
                "args": [ids_arg],
                "options": [
                    {"name": "--wait", "type": "bool", "required": false, "default": false, "description": "Wait for these existing clips to finish; never submits a new generation"},
                    {"name": "--download", "type": "string", "required": false, "description": "Download completed clips to this directory; implies --wait and returned clips include local_path"}
                ],
                "returned_clip_fields": ["id", "status", "local_path"]
            }),
        ),
        (
            "jobs",
            json!({
                "description": "List durable generation receipts for recovery after interruption or an uncertain submission",
                "args": [],
                "options": [
                    {"name": "--limit", "type": "number", "required": false, "default": 10, "description": "Maximum number of recent receipts (1-1000)"}
                ],
                "receipt_fields": ["transaction_id", "request_sha256", "updated_at", "state", "ids", "next_action", "path"],
                "omitted_from_receipts": ["credentials", "lyrics"],
                "recovery_rule": "If IDs exist, run the receipt's next_action (`suno status <ids> --wait`). If submission state is unknown, inspect `suno jobs` and the library; never resubmit the unknown request."
            }),
        ),
        (
            "download",
            json!({
                "description": "Download audio/video for clip(s), embedding lyrics into MP3s",
                "aliases": ["dl"],
                "args": [ids_arg],
                "options": [
                    {"name": "--output", "type": "string", "required": false, "default": ". (config `output_dir`)", "description": "Output directory"},
                    {"name": "--format", "type": "string", "required": false, "default": "mp3", "values": ["mp3", "wav", "m4a", "mp4"], "description": "Prepared file format"},
                    {"name": "--source", "type": "string", "required": false, "default": "auto", "values": ["auto", "studio", "library", "remote-pool"], "description": "auto uses official signed Studio/library downloads only (MP4 always Library); remote-pool is explicit opt-in"},
                    {"name": "--video", "type": "bool", "required": false, "default": false, "description": "Compatibility shortcut for --format mp4; conflicts with an explicit --format"}
                ],
                "transport": "Explicit global --source remote-pool uses the authenticated HTTPS remote or HTTP numeric-loopback worker service (MP3/WAV only) without provider preparation or fallback. Export SUNO_REMOTE_POOL_URL (e.g. https://gsunopool.gunamaya.id) and SUNO_REMOTE_POOL_TOKEN_FILE (local bearer-token file path); local defaults are unchanged. HTTPS validates TLS certificates; HTTP is only 127.0.0.1/[::1]. No redirects, environment proxies, URL credentials, query, fragment or non-root path. Private/non-unicast IP literals are rejected except loopback; DNS names are trusted configuration. No .env autoload; parent tools such as yt-to-flowmusic must inherit exported variables and explicitly pass --source remote-pool. By default, formats use Suno's signed download-preparation APIs. Prepared URLs are short-lived HTTPS URLs; auto selects Studio when the account has access and otherwise uses the library route. MP4 always uses Library in auto mode. Billing and preparation errors propagate without fallback to metadata URLs or remote-pool. media_urls[].encoding remains metadata only; encoded progressive downloads and local decryption are not supported."
            }),
        ),
        (
            "delete",
            json!({
                "description": "Move clip(s) to trash (recoverable); --restore undoes it. Trashing requires -y (no interactive confirmation)",
                "aliases": ["rm"],
                "args": [ids_arg],
                "options": [
                    {"name": "--yes", "type": "bool", "required": false, "default": false, "description": "Confirm trashing (-y); required unless --restore"},
                    {"name": "--restore", "type": "bool", "required": false, "default": false, "description": "Restore the clip(s) from trash instead of trashing (no -y needed)"}
                ]
            }),
        ),
        (
            "set",
            json!({
                "description": "Update clip title, lyrics, or caption",
                "args": [{"name": "id", "kind": "positional", "type": "string", "required": true, "description": "Clip ID"}],
                "options": [
                    {"name": "--title", "type": "string", "required": false, "description": "New title"},
                    {"name": "--lyrics", "type": "string", "required": false, "description": "New lyrics"},
                    {"name": "--lyrics-file", "type": "string", "required": false, "description": "Read lyrics from file"},
                    {"name": "--caption", "type": "string", "required": false, "description": "New caption"},
                    {"name": "--remove-cover", "type": "bool", "required": false, "default": false, "description": "Remove custom cover image"}
                ]
            }),
        ),
        (
            "publish",
            json!({
                "description": "Toggle clip public/private",
                "args": [ids_arg],
                "options": [
                    {"name": "--private", "type": "bool", "required": false, "default": false, "description": "Make private instead of public"}
                ]
            }),
        ),
        (
            "timed-lyrics",
            json!({
                "description": "Word-level timestamped lyrics via a separate authenticated Suno API request, not the remote worker pool; downloads do not create LRC sidecars",
                "args": [{"name": "id", "kind": "positional", "type": "string", "required": true, "description": "Clip ID"}],
                "options": [
                    {"name": "--lrc", "type": "bool", "required": false, "default": false,
                     "description": "Raw LRC on stdout — stays raw even when piped or --json is set (documented envelope exception); unavailable alignment may be empty"}
                ]
            }),
        ),
        (
            "credits",
            json!({
                "description": "Show credit balance and plan info",
                "args": [],
                "options": [],
                "data_fields": ["credits", "total_credits_left", "monthly_usage", "monthly_limit", "plan", "models", "download_usage"]
            }),
        ),
        (
            "models",
            json!({
                "description": "List available models (live from your plan)",
                "args": [],
                "options": []
            }),
        ),
        (
            "auth",
            json!({
                "description": "Set up authentication (browser extract, cookie, JWT, refresh, logout)",
                "args": [],
                "options": [
                    {"name": "--login", "type": "bool", "required": false, "default": false, "description": "Auto-extract from browser (recommended)"},
                    {"name": "--refresh", "type": "bool", "required": false, "default": false, "description": "Force-refresh the JWT via stored Clerk session"},
                    {"name": "--cookie", "type": "string", "required": false, "description": "Cookie header or raw __client value"},
                    {"name": "--jwt", "type": "string", "required": false, "description": "Direct JWT (~1h lifetime)"},
                    {"name": "--cookie-stdin", "type": "bool", "required": false, "default": false, "description": "Read a Clerk cookie from stdin so it is not exposed in process arguments"},
                    {"name": "--jwt-stdin", "type": "bool", "required": false, "default": false, "description": "Read a JWT from stdin so it is not exposed in process arguments"},
                    {"name": "--device", "type": "string", "required": false, "description": "Device ID override"},
                    {"name": "--logout", "type": "bool", "required": false, "default": false, "description": "Remove stored authentication"}
                ]
            }),
        ),
        (
            "config show",
            json!({
                "description": "Show the effective merged configuration",
                "args": [],
                "options": []
            }),
        ),
        (
            "config set",
            json!({
                "description": "Set a configuration value in the config file",
                "args": [
                    {"name": "key", "kind": "positional", "type": "string", "required": true,
                     "description": crate::config::CONFIG_KEYS.join(" | ")},
                    {"name": "value", "kind": "positional", "type": "string", "required": true, "description": "New value"}
                ],
                "options": []
            }),
        ),
        (
            "config path",
            json!({
                "description": "Show the configuration file path",
                "args": [],
                "options": []
            }),
        ),
        (
            "config check",
            json!({
                "description": "Validate the configuration file",
                "args": [],
                "options": []
            }),
        ),
        (
            "doctor",
            json!({
                "description": "Check auth, Chrome, network, credits, captcha state, and config health",
                "args": [],
                "options": [],
                "exit_behavior": "0 if no check fails (warnings allowed), 2 if any check fails"
            }),
        ),
        (
            "agent-info",
            json!({
                "description": "This manifest",
                "args": [],
                "options": []
            }),
        ),
        (
            "guide",
            json!({
                "description": "List built-in songwriting guides, or print one as raw markdown to stdout",
                "aliases": ["guides"],
                "args": [{
                    "name": "name", "kind": "positional", "type": "string", "required": false,
                    "description": "Guide name or alias; omit to list all guides"
                }],
                "options": [],
                "raw_output": "with a name: raw markdown on stdout (documented envelope exception)"
            }),
        ),
        (
            "skill install",
            json!({
                "description": "Install the agent skill to all detected platforms (idempotent)",
                "args": [],
                "options": []
            }),
        ),
        (
            "skill status",
            json!({
                "description": "Check which platforms have the skill installed and current",
                "args": [],
                "options": []
            }),
        ),
        (
            "update",
            json!({
                "description": "Distribution-aware update check/apply",
                "args": [],
                "options": [
                    {"name": "--check", "type": "bool", "required": false, "default": false, "description": "Check only, don't install"},
                    force_option
                ],
                "install_sources": ["standalone", "homebrew", "cargo"],
                "data_fields": [
                    "current_version", "latest_version", "status", "install_source",
                    "update_mode", "upgrade_command", "release_url", "requires_skill_reinstall"
                ]
            }),
        ),
    ];

    let mut annotations: Map<String, Value> = commands
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();

    let semantics = json!({
        "prompt": {
            "effect": "mixed", "idempotent": true,
            "effect_detail": "Builds the brief and next-action argv entirely offline without using credits; writes a local file only when --out is supplied",
            "examples": [["prompt", "--preset", "comic-folk"]]
        },
        "write": {
            "effect": "mixed", "idempotent": true,
            "effect_detail": "Pure composition unless --out or --project-out writes local files; repeated writes replace the same requested paths",
            "examples": [["write", "--theme", "night drive"]]
        },
        "generate": {
            "effect": "write", "idempotent": false,
            "effect_detail": "Submits a paid Suno generation. --request-id provides local receipt reconciliation for retries; it does not claim provider-side exactly-once execution",
            "examples": [["generate", "--title", "Night Drive", "--tags", "indie rock", "--lyrics", "[Verse] City lights"]]
        },
        "describe": {
            "effect": "write", "idempotent": false,
            "effect_detail": "Submits a paid Suno generation. --request-id provides local receipt reconciliation for retries; it does not claim provider-side exactly-once execution",
            "examples": [["describe", "--prompt", "a quiet piano nocturne"]]
        },
        "lyrics": {"effect": "write", "idempotent": false, "examples": [["lyrics", "--prompt", "rainy mornings"]]},
        "extend": {"effect": "write", "idempotent": false, "examples": [["extend", "clip_id", "--at", "30"]]},
        "concat": {"effect": "write", "idempotent": false, "examples": [["concat", "clip_id"]]},
        "cover": {"effect": "write", "idempotent": false, "examples": [["cover", "clip_id", "--tags", "jazz"]]},
        "remaster": {"effect": "write", "idempotent": false, "examples": [["remaster", "clip_id"]]},
        "stems": {"effect": "write", "idempotent": false, "examples": [["stems", "clip_id"]]},
        "info": {"effect": "read", "idempotent": true, "examples": [["info", "clip_id"]]},
        "persona": {"effect": "read", "idempotent": true, "examples": [["persona", "persona_id"]]},
        "list": {"effect": "read", "idempotent": true, "examples": [["list"]]},
        "search": {"effect": "read", "idempotent": true, "examples": [["search", "night drive"]]},
        "status": {
            "effect": "mixed", "idempotent": true,
            "effect_detail": "Reads clip state; --download additionally writes completed media to local paths and never submits generation",
            "examples": [["status", "clip_id"]]
        },
        "jobs": {"effect": "read", "idempotent": true, "examples": [["jobs"]]},
        "download": {
            "effect": "write", "idempotent": true,
            "effect_detail": "Writes prepared media to the requested local output directory",
            "examples": [["download", "clip_id", "--format", "mp3"]]
        },
        "delete": {
            "effect": "write", "idempotent": false,
            "effect_detail": "Moves clips to trash; --restore reverses that state",
            "examples": [["delete", "clip_id", "--yes"]]
        },
        "set": {"effect": "write", "idempotent": true, "examples": [["set", "clip_id", "--title", "New title"]]},
        "publish": {"effect": "write", "idempotent": true, "examples": [["publish", "clip_id", "--private"]]},
        "timed-lyrics": {"effect": "read", "idempotent": true, "examples": [["timed-lyrics", "clip_id", "--lrc"]]},
        "credits": {"effect": "read", "idempotent": true, "examples": [["credits"]]},
        "models": {"effect": "read", "idempotent": true, "examples": [["models"]]},
        "auth": {
            "effect": "mixed", "idempotent": false,
            "effect_detail": "Checks or refreshes authentication and may store or remove local credentials depending on flags",
            "examples": [["auth"]]
        },
        "config show": {"effect": "read", "idempotent": true, "examples": [["config", "show"]]},
        "config set": {"effect": "write", "idempotent": true, "examples": [["config", "set", "default_model", "v6"]]},
        "config path": {"effect": "read", "idempotent": true, "examples": [["config", "path"]]},
        "config check": {"effect": "read", "idempotent": true, "examples": [["config", "check"]]},
        "doctor": {"effect": "read", "idempotent": true, "examples": [["doctor"]]},
        "agent-info": {"effect": "read", "idempotent": true, "examples": [["agent-info", "--command", "generate"]]},
        "guide": {"effect": "read", "idempotent": true, "examples": [["guide", "songwriting"]]},
        "skill install": {
            "effect": "write", "idempotent": true,
            "effect_detail": "Writes the embedded skill to detected agent-platform directories",
            "examples": [["skill", "install"]]
        },
        "skill status": {"effect": "read", "idempotent": true, "examples": [["skill", "status"]]},
        "update": {
            "effect": "mixed", "idempotent": false,
            "effect_detail": "--check only reads release state; without it the command can replace the installed binary",
            "examples": [["update", "--check"]]
        }
    });
    for (path, semantic) in semantics.as_object().expect("static semantic annotations") {
        annotations
            .get_mut(path)
            .unwrap_or_else(|| panic!("semantic annotation names missing command: {path}"))
            .as_object_mut()
            .expect("command annotation object")
            .extend(
                semantic
                    .as_object()
                    .expect("semantic annotation object")
                    .clone(),
            );
    }
    annotations
}

fn argument_name(arg: &Arg) -> String {
    arg.get_long()
        .map(|long| format!("--{long}"))
        .unwrap_or_else(|| arg.get_id().to_string())
}

fn argument(command: &Command, arg: &Arg) -> Value {
    let boolean = matches!(arg.get_action(), ArgAction::SetTrue | ArgAction::SetFalse);
    let multiple = arg
        .get_num_args()
        .is_some_and(|range| range.max_values() > 1);
    let mut value = json!({
        "name": argument_name(arg),
        "type": if boolean { "bool" } else if multiple { "string..." } else { "string" },
        "required": arg.is_required_set(),
        "description": arg.get_help().map(ToString::to_string).unwrap_or_default(),
    });
    if arg.get_index().is_some() {
        value["kind"] = json!("positional");
    }
    if let Some(short) = arg.get_short() {
        value["short"] = json!(format!("-{short}"));
    }
    if let Some(aliases) = arg.get_visible_aliases()
        && !aliases.is_empty()
    {
        value["aliases"] = json!(
            aliases
                .into_iter()
                .map(|alias| format!("--{alias}"))
                .collect::<Vec<_>>()
        );
    }
    let defaults: Vec<String> = arg
        .get_default_values()
        .iter()
        .map(|default| default.to_string_lossy().into_owned())
        .collect();
    if defaults.len() == 1 {
        value["default"] = if boolean {
            json!(defaults[0] == "true")
        } else {
            json!(defaults[0])
        };
    } else if !defaults.is_empty() {
        value["default"] = json!(defaults);
    }
    if let Some(values) = arg
        .get_value_parser()
        .possible_values()
        .filter(|_| !boolean)
    {
        value["values"] = values
            .filter(|possible| !possible.is_hide_set())
            .map(|possible| json!(possible.get_name()))
            .collect();
    }
    if !boolean && let Some(range) = arg.get_num_args() {
        let max = range.max_values();
        value["arity"] = json!({
            "min": range.min_values(),
            "max": if max == usize::MAX { Value::Null } else { json!(max) }
        });
    }
    let conflicts: Vec<String> = command
        .get_arg_conflicts_with(arg)
        .into_iter()
        .filter(|other| !other.is_hide_set())
        .map(argument_name)
        .collect();
    if !conflicts.is_empty() {
        value["conflicts"] = json!(conflicts);
    }
    value
}

fn command_syntax(root: &Command, prefix: &str, result: &mut Map<String, Value>) {
    for command in root
        .get_subcommands()
        .filter(|command| !command.is_hide_set())
    {
        if command.get_name() == "help" {
            continue;
        }
        let path = format!("{prefix}{}", command.get_name());
        if command.get_subcommands().next().is_some() {
            command_syntax(command, &format!("{path} "), result);
            continue;
        }
        let mut args = Vec::new();
        let mut options = Vec::new();
        for arg in command
            .get_arguments()
            .filter(|arg| !arg.is_hide_set() && !arg.is_global_set())
        {
            if matches!(
                arg.get_action(),
                ArgAction::Help | ArgAction::HelpShort | ArgAction::HelpLong | ArgAction::Version
            ) {
                continue;
            }
            if arg.get_index().is_some() {
                args.push(argument(command, arg));
            } else {
                options.push(argument(command, arg));
            }
        }
        let mut value = json!({
            "description": command.get_about().map(ToString::to_string).unwrap_or_default(),
            "args": args,
            "options": options,
        });
        let aliases: Vec<&str> = command.get_visible_aliases().collect();
        if !aliases.is_empty() {
            value["aliases"] = json!(aliases);
        }
        result.insert(path, value);
    }
}

fn generated_commands(root: &Command) -> Result<Map<String, Value>, CliError> {
    let mut entries = Map::new();
    command_syntax(root, "", &mut entries);

    let annotations = domain_annotations();
    for path in annotations.keys() {
        if !entries.contains_key(path) {
            return Err(CliError::Config(format!(
                "agent-info metadata names missing command: {path}"
            )));
        }
    }
    for (path, entry) in &mut entries {
        let annotation = annotations.get(path).ok_or_else(|| {
            CliError::Config(format!(
                "add agent-info effect metadata for public command: {path}"
            ))
        })?;
        let mut semantic = annotation
            .as_object()
            .expect("static command annotation object")
            .clone();
        for syntax_field in ["description", "args", "options", "aliases"] {
            semantic.remove(syntax_field);
        }
        entry
            .as_object_mut()
            .expect("generated command object")
            .extend(semantic);
    }
    Ok(entries)
}

fn global_flags(root: &Command) -> Map<String, Value> {
    root.get_arguments()
        .filter(|arg| arg.is_global_set() && !arg.is_hide_set())
        .map(|arg| {
            let mut value = argument(root, arg);
            let name = value
                .as_object_mut()
                .expect("global flag object")
                .remove("name")
                .expect("global flag name")
                .as_str()
                .expect("global flag name string")
                .to_owned();
            (name, value)
        })
        .collect()
}

fn base_manifest(commands: Map<String, Value>, global_flags: Map<String, Value>) -> Value {
    let auth_path = crate::config::config_dir()
        .join("auth.json")
        .display()
        .to_string();

    let info = json!({
        "name": "suno",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "Write, generate, and manage Suno music — a native song composer (`write`), v6 generation, resumable jobs, signed downloads, voice personas, covers, remasters, and built-in songwriting guides",
        "commands": commands,
        // Built-in songwriting knowledge, discoverable via `suno guide <name>`.
        "guides": guide_map(),
        "global_flags": global_flags,
        "exit_codes": {
            "0": "Success",
            "1": "Transient error (network, API, download) — retry with backoff",
            "2": "Configuration or auth error — run `suno doctor`; for auth run `suno auth --login`",
            "3": "Bad input (arguments, unknown ID, moderation rejection, duplicate run) — fix before retrying",
            "4": "Rate limited — wait 30-60s and retry",
        },
        "breaking_changes": {
            "0.10.1": "Failures use one stderr envelope, with recoverable results in error.details. --force only bypasses locking; use --allow-placeholders to send literal lyric markers.",
            "0.6.0": "Exit codes remapped to the framework contract: auth errors 3→2, not-found 5→3, code 5 removed. `list --json` data is now {clips, next_cursor, has_more}; `list --page` → `--cursor`; `generate --variation` removed."
        },
        "envelope": {
            "version": "1",
            "success": "{ version, status, data }",
            "error": "{ version, status, error: { code, message, suggestion, details? } }",
            "statuses": ["success", "no_results", "error"],
            "error_details": "Optional structured recovery data: clip IDs, paths, failed downloads, doctor checks, or next_action.argv",
            "raw_output_exceptions": [
                "agent-info (this manifest)",
                "timed-lyrics --lrc (raw LRC on stdout even when piped)"
            ]
        },
        "config": {
            "path": crate::config::config_path().display().to_string(),
            "env_prefix": "SUNO_",
            "keys": {
                "default_model": "Default --model for generate/describe/extend/cover (clap name, v6 out of the box)",
                "poll_interval_secs": "Initial poll backoff for --wait (doubles up to 15s)",
                "poll_timeout_secs": "Total --wait timeout before giving up",
                "output_dir": "Default directory for `download`",
            },
            "precedence": "flag > SUNO_* env > config file > default"
        },
        "auto_json_when_piped": true,
        // Domain extras below (schema allows additionalProperties).
        // Generated from the --model clap enums, so they cannot drift.
        "models": model_map(),
        "active_models": {
            "v6": "chirp-hawk",
            "v6-wild": "chirp-hawk-wild",
            "v6-mini": "chirp-goose"
        },
        "retired_models": retired_models(),
        "retired_models_note": "Suno retired the legacy models on September 9, 2026. Their values remain accepted for compatibility, but agents should use v6, v6-wild, or v6-mini for new work.",
        "remaster_models": remaster_model_map(),
        "default_remaster_model": "chirp-halibut (v6)",
        "generation_cost": {
            "standard_v6": {
                "credits": 10,
                "outputs": 2,
                "description": "Official standard v6 generation price: 10 credits for two songs",
                "source": "https://help.suno.com/en/articles/13924481",
                "as_of": "2026-09-28"
            },
            "max_mode": "Max Mode costs more credits than standard generation.",
            "authority": "The actual billing shown by the caller's Suno account is authoritative.",
            "lyrics": "Free; no audio generation"
        },
        "features": [
            "song_composer", "priming_mode", "builtin_guides",
            "tags", "negative_tags", "vocal_gender",
            "weirdness", "style_influence", "audio_influence",
            "instrumental", "extend", "concat", "cover", "remaster",
            "stems", "lyrics", "timed_lyrics", "set_metadata",
            "set_visibility", "search", "delete", "captcha_check",
            "id3_lyrics_embedding", "voice_persona", "clip_info",
            "headless", "no_browser", "dry_run", "max_mode",
            "generation_receipts", "idempotent_request_id", "signed_download_preparation"
        ],
        "workflows": {
            "generation_recovery": {
                "steps": [
                    "Run `suno jobs` to inspect durable receipts and recover saved clip IDs.",
                    "Run `suno status <id>... --wait --download DIR` to resume waiting and download completed clips.",
                    "If a submission is uncertain and no IDs were saved, inspect the receipt and library; never resubmit the unknown request."
                ],
                "receipts": "Contain transaction state, IDs, and next_action; credentials and lyric payloads are omitted.",
                "idempotency": "Pass --request-id UUID to generate or describe for local receipt reconciliation. A matching completed receipt reuses saved IDs; mismatched or uncertain reuse is rejected. This does not claim provider-side exactly-once execution."
            },
            "downloads": "Default downloads use signed download-preparation APIs; explicit --source remote-pool uses the authenticated HTTPS remote or HTTP numeric-loopback worker pool (MP3/WAV only, never falls back). Source auto selects Studio for audio when the account has access, otherwise Library; MP4 always uses Library. No automatic metadata or remote-pool fallback. Downloads returned by creation/status commands set local_path on each completed clip."
        },
        "auth_path": auth_path,
        "auth": {
            "recommended": "suno auth --login",
            "methods": [
                "browser_cookie_extract",
                "full_cookie_header",
                "raw_clerk_client_cookie",
                "direct_jwt",
                "stored_clerk_refresh",
            ],
            "logout": "suno auth --logout",
            "generation_captcha": "Generation preflights /api/c/check and skips the solver when the account is not captcha-gated. When gated, provider 1 uses hCaptcha and provider 2 uses Turnstile; browser-backed solving runs automatically; --headless restricts it to invisible Chrome. --no-browser forbids browser use and returns captcha_required. Use --no-captcha only with a valid --token or for deliberate API tests.",
            "browser_policy": "--headless permits invisible Chrome and never shows a headed window. --no-browser forbids browser auth extraction and captcha solving; stored credentials and --cookie-stdin/--jwt-stdin still work.",
        },
        "provider": "direct_suno_unofficial",
        "auth_required": true,
        "default_model": "chirp-hawk (v6)",
        "agent_cli_framework": {
            "commit": "cdb6add7bcf8896a5a8c1d6015e36887aa642e1a"
        },
    });
    info
}

pub fn manifest(filter: Option<&str>) -> Result<Value, CliError> {
    let mut root = Cli::command();
    root.build();
    let globals = global_flags(&root);
    let mut entries = generated_commands(&root)?;

    if let Some(filter) = filter {
        let path = filter.split_whitespace().collect::<Vec<_>>().join(" ");
        if path.is_empty() {
            return Err(CliError::InvalidInput(
                "command path cannot be empty".into(),
            ));
        }
        let prefix = format!("{path} ");
        entries.retain(|key, _| key == &path || key.starts_with(&prefix));
        if entries.is_empty() {
            return Err(CliError::InvalidInput(format!(
                "unknown command path: {path}; use a canonical path from --help"
            )));
        }
    }

    Ok(base_manifest(entries, globals))
}

pub fn run(filter: Option<&str>) -> Result<(), CliError> {
    output::json::print(&manifest(filter)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_maps_mirror_the_clap_enums() {
        let m = model_map();
        assert_eq!(m.len(), ModelVersion::value_variants().len());
        assert_eq!(m["v6"], "chirp-hawk");
        assert_eq!(m["v6-wild"], "chirp-hawk-wild");
        assert_eq!(m["v6-mini"], "chirp-goose");
        assert_eq!(m["v4.5-all"], "chirp-auk-turbo");
        let r = remaster_model_map();
        assert_eq!(r.len(), RemasterModel::value_variants().len());
        assert_eq!(r["v6"], "chirp-halibut");
        assert_eq!(r["v5.5"], "chirp-flounder");
        assert!(retired_models().contains(&json!("v5.5")));
        assert!(!retired_models().contains(&json!("v6")));
    }
}
