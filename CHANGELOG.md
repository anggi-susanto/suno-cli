# Changelog

## v0.11.0 — Explicit remote-pool downloads and timing diagnostics

### Added

- Global `--source remote-pool` for the create → wait → download flow and existing-clip downloads. The authenticated loopback worker service supports MP3/WAV; creation/status `--download` remains MP3. An admin health check runs before paid generation requesting remote downloads. No automatic provider fallback occurs.
- Local Python worker service with authenticated admin enqueue/status/file endpoints, device routing and connectivity probes, safe offline administration, durable receipts, and explicit ambiguous-work reconciliation. The development helper requires macOS/Linux (POSIX `fcntl`); it does not bypass provider authorization or verification.
- Remote transfers check size, SHA-256, and container signatures before atomic no-clobber publication. Existing MP3 tagging and `local_path` output remain available.
- Opt-in global `--timings` reports monotonic stage durations on stderr, including auth, captcha/browser, generation, downloads and tagging. JSON failures retain a single error envelope; timing records omit credentials and request data.
- Clip `media_urls` and opaque `encoding` metadata are preserved for inspection only.

### Changed / migration

- Removed `--source playback`; use official signed `auto`/`studio`/`library` downloads, or explicitly select `remote-pool`. The unused experimental local crypto implementation is not supported. No encoded progressive downloads or local decryption are provided.
- `auto` uses official signed Studio/Library preparation only (MP4 always Library). Billing, authorization and preparation errors propagate without silent fallback.
- Help and README document remote-pool setup, create/wait/download recovery, and separate `suno timed-lyrics CLIP_UUID --lrc > song.lrc` export via Suno API authentication. Downloads do not automatically create LRC sidecars; MP3 ID3 embedding is unchanged.

### Verification scope

- `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D warnings`, and `cargo test --locked` passed locally (207 Rust tests).
- All 10 Python worker-service tests passed; CI and release verification now run them.
- `cargo build --locked --release` and the shell framework conformance probe passed; the release binary reports `suno 0.11.0`.
- The separate Python schema validator could not run locally because `jsonschema` is not installed; CI installs `conformance/requirements.txt` before running it. Cross-platform release builds remain CI work.

Local Rust and Python tests use isolated state and loopback mocks; they do not
establish live provider decoding, browser-worker availability or credit-spending
generation. Reconcile existing active/ambiguous work before safely restarting an
older service to enable its new admin endpoints.

## v0.10.2 — Playback URL downloads

- Added `suno download --source playback` to use clip metadata URLs for MP3/MP4 without signed download preparation. Atomic transfers, container validation, and MP3 lyric tagging remain enabled.
- Playback rejects WAV/M4A, missing or non-HTTPS URLs, and incomplete clips; failures do not fall back to signed preparation.
- Batch download retry commands now preserve the selected source.

### Usage

```bash
suno download <clip_id> --source playback --output ./songs/
suno download <clip_id> --source playback --format mp4 --output ./videos/
```

MP3 uses `audio_url`; MP4 uses `video_url`. The default `--source auto` behavior is unchanged. Playback does not perform billing lookup, library authorization, format conversion, or signed URL preparation. Playback URLs can expire or return an unsupported container; those failures are reported rather than silently switching sources. Use only media you have permission to access.

### Verification

- All 195 local tests passed, including playback URL selection, format restrictions, URL validation, and CLI parsing.
- `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo build --release` passed locally on macOS ARM64.
- The installed release binary reports `suno 0.10.2`. Live Suno downloads have not been tested for this release; cross-platform builds are handled by the release workflow.

## v0.10.1 — v6 catalogue, resumable jobs, and signed downloads

- Added Turnstile provider detection, numeric `--token-provider`, and the website's hCaptcha fallback. Captcha calls have absolute deadlines; `--quiet` keeps failures machine-readable.
- Release builds now run checks before packaging; crates.io publication failures are surfaced.

**Models and request validation:**

- The out-of-box default is now v6 (`chirp-hawk`), with v6-wild (`chirp-hawk-wild`) and v6-mini (`chirp-goose`) available. The v6 remaster key is `chirp-halibut`.
- Pre-v6 flag values remain parseable for compatibility. Suno retired those models on September 9, 2026, so the CLI now checks the account catalogue and rejects absent or unavailable models before submission.
- Limits are validated as UTF-16 units to match the web forms: title 100, custom prompt/lyrics 5000, tags 1000, excluded styles 1000, and Simple description 3000.
- `generate` and `describe` gained `--dry-run`, `--max-mode`, and a UUID `--request-id`. Dry runs are offline previews and explicitly report that live model validation and submission have not occurred.
- Suno documents standard v6 generation as 10 credits for two songs. Max Mode uses more credits; the account response remains authoritative.

**Recovery and headless operation:**

- Generation receipts are reserved before submission and saved under the data directory. `suno jobs` lists their state, IDs, payload hash, and recovery action without storing lyrics, credentials, or tokens.
- Reusing `--request-id` with an identical submitted payload returns the saved clip IDs. Payload mismatches and outcome-unknown submissions fail closed instead of issuing another paid request.
- `suno status <ids> --wait --download DIR` resumes existing work and never submits a generation. `--download` implies `--wait`; downloaded clip objects include `local_path`.
- Global `--headless` allows invisible Chrome for a required captcha with no headed fallback. Global `--no-browser` is HTTP-only and returns `captcha_required` when a browser challenge is required.
- Each captcha process has an isolated temporary Chrome profile, preventing cross-command profile locks. JSON errors suppress progress chatter.
- `suno auth --cookie-stdin` and `suno auth --jwt-stdin` keep credentials out of process arguments.

**Downloads:**

- Downloads use Suno's signed preparation flow instead of stale playback URLs. `--format` supports MP3, WAV, M4A, and MP4; `--source` supports `auto`, `studio`, and `library`; `--video` remains a compatibility shortcut.
- Auto source selection uses Studio when the account advertises the `studio` feature. Otherwise it authorizes the library item once and prepares a short-lived signed URL. Expired preparation URLs are refreshed without repeating authorization.
- Downloads validate their file container before the atomic rename; HTML error pages cannot become completed audio files.
- MP3 lyric embedding remains available. A failed multi-download exits nonzero with completed paths, failed IDs, and a retry argv in `error.details`; successful downloads keep `{downloaded, failed}`.

**Agent workflows and framework:**

- New offline `suno prompt` makes tempo, beat unit, meter, groove, voice, delivery, instruments, arrangement, and exclusions explicit. Original editable examples include comic folk, work song, solo lament, and electronic music. Returned preview/generation argv uses a stable request UUID.
- Prompting and songwriting guides cite current official sources, remove obsolete model/cost guidance and unsupported rigid rules, and recommend auditioning a pair before batching.
- Updated to agent-cli-framework cdb6add: Clap-derived syntax, scoped discovery through `agent-info --command` (or `info --command`), compact fallible JSON, single failure envelope with diagnostic details, kernel file locks, and verified standalone updates.
- `--force` now bypasses only duplicate locking. `--allow-placeholders` explicitly sends literal scaffold markers. `delete --confirm` complements the existing `--yes`.

**Migration:**

- Existing installs with an old persisted override should run `suno config set default_model v6`.
- Refresh installed agent instructions with `suno skill install` after updating the binary.

The v6 catalogue and limits were live verified. The current generate and download request shapes are supported by web-bundle and independent implementation evidence plus local tests; live v6 generation, MP3/WAV/M4A/MP4 decoding, Studio and Library downloads, Library MP3 authorization, and request-ID replay passed. Strict headless captcha challenges required the existing offscreen fallback on the tested account.

## v0.8.0 — the composer and the renderer agree about the artifact

One invariant now holds end to end: the file named by the emitted generate command exists, is directly consumable by `--lyrics-file`, contains no unresolved instructions, and reflects every selected control.

**Breaking** (agents reading `write --json`):

- `write --out FILE` writes the **lyric block only**. It previously wrote a composite document (title, Style Prompt, `---` rules, Suno Tags, and in priming mode the Prime-Stack Map and research artefact) that the emitted workflow then handed to `--lyrics-file` — so headers, tags and research metadata were sent to Suno as lyrics and embedded in the MP3. The composite document moved to the new `--project-out FILE`.
- The `generate` string field is replaced by `next_action: {argv, command}`. `argv` is authoritative; `command` is shell-escaped display text. It is `null` when no `--out` file exists, instead of advertising a hardcoded `song.txt` that was never written.
- `write --mode priming` requires `--target`, `--objective` and `--domain` (exit 3 when missing) — priming is consent-based and every run must be auditable. `--domain` and `--subtlety` values are validated.
- `guide` lost the `write` alias (it competed with the `suno write` command).

**Fixes:**

- `--mood` / `--vocal` / `--bpm` / `--instrumental` now drive the Style Prompt, the `[Mood:]`/`[Energy:]`/vocal meta-tags and `suno_tags` from one resolved-controls struct. `--mood "dark and brooding"` no longer emitted `[Mood: Uplifting]` and an "uplifting" tag alongside it.
- `--instrumental` is coherent: no `<...>` fill instructions, `--instrumental` in the emitted command, no vocal-only tags.
- Titles and paths in the emitted command are shell-escaped (`She Said "Go"` produced invalid shell).
- `generate` refuses lyrics containing unresolved `<...>` scaffold placeholders (exit 3, naming the line numbers) so an unfilled draft cannot spend generation credits. `--force` overrides.
- The emitted command stopped pinning `--model v4.5-all` and instead used the configured default. At the time of this v0.8.0 release, that default was v5.5; v0.10.1 later moved it to v6.
- New fields: `placeholders_remaining`, `ready_to_generate`, `missing_requirements`, `project_written`.

**Discovery:**

- `write` leads the command list in `--help`; the root example is the full write → fill → generate → download flow, with a one-liner distinguishing write/generate/describe/lyrics. README Quick Start mirrors it.
- `write --help` no longer claims plain text on stdout while the framework sends JSON when piped: shell redirection gets the envelope, `--out` gets the lyrics file.
- `agent-info` gained the `write` output schema, workflow, and mode-specific required fields.

## v0.6.0 — framework conformance, captcha preflight, real config

**Breaking** (agents pinned to the 0.5.x contract must update):

- Exit codes remapped to the [agent-cli-framework](https://github.com/paperfoot/agent-cli-framework) contract: 0 success, 1 transient, 2 config/auth (auth was 3), 3 bad input incl. not-found (was 5), 4 rate limited. Code 5 removed.
- `list --json` data is now `{clips, next_cursor, has_more}` (was a bare clip array); `list --page` replaced by `--cursor <token>` (the old page numbers never worked against feed/v3's opaque cursors).
- `generate --variation` removed — it was parsed and silently ignored.
- `download --json` data is now `{downloaded, failed}` with `partial_success` status when some clips fail (was a bare path array).
- `delete`/`auth`/`set`/`publish`/`config` now emit success envelopes in JSON mode.

**Captcha & generation:**

- Captcha preflight: every gated command asks `/api/c/check` first and skips the Chrome solver entirely when the account isn't captcha-gated (most aren't).
- `extend`/`cover`/`remaster` now route through the same captcha pipeline as `generate`/`describe` (they previously posted `token: null` and failed on captcha-enforced accounts), and gained `--token`/`--no-captcha`.
- Fixed the bare `__client` cookie being dropped on the solver's cookie replay — the root cause of "hcaptcha never finished loading" on sub-threshold accounts.
- `--wait` now exits non-zero when generation fails (moderation rejections exit 3); previously failed clips exited 0.
- New model `v4.5-all` (chirp-auk-turbo, Suno's "best free model"); `extend` gained `--model`; new `--audio-influence` slider on generate/cover.
- Documented the then-observed v5.5 credit cost. Suno's current v6 documentation supersedes that historical value.

**Tooling:**

- `suno doctor` — auth/JWT/Chrome/API/credits/captcha health checks.
- `suno skill install|status` — agent skill for Claude Code, Codex CLI, Gemini CLI (replaces `install-skill`, which remains a hidden alias).
- `suno update` is distribution-aware: brew- and cargo-owned binaries are never self-replaced; the owner channel's upgrade command is returned instead.
- Config layer is now real: TOML file + `SUNO_*` env vars actually drive polling, default model, and download dir (`config show|set|path|check`).
- Duplicate-run guard on generate/describe/cover/remaster/update (`--force` bypasses).
- Vendored framework conformance probe + schemas under `conformance/`, run in CI; integration test suite under `tests/`.
- Fixed a UTF-8 panic when tables truncated multi-byte (CJK/emoji) prompts.

## v0.5.x

- v0.5.7 — fix captcha desktop viewport
- v0.5.6 — fix captcha cookie replay
- v0.5.5 — auth hardening and release cleanup
- v0.5.4 — auto-solve hCaptcha via piloted Chrome (CDP)
- v0.5.3 — add `suno auth --refresh` + clearer captcha-rollout error
- v0.5.2 — add in-process JWT refresh retry
- v0.5.1 — rename package to `suno`, add `install-skill` command
- v0.5.0 — fix cover/remaster endpoints, add persona + info commands, framework alignment

## Earlier

- v0.4.0 — zero-friction auth: `suno auth --login`
- v0.3.0 — audit fixes, set metadata, publish, timed lyrics, ID3 embedding
- v0.2.0 — search, delete, slug filenames, renamed commands
- v0.1.0 — initial release
