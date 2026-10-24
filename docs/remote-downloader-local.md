# GSuno local worker service

Requires Python 3.8+ on macOS/Linux (the state lock uses POSIX `fcntl`; the helper
is not supported on native Windows). The Rust CLI can connect to a compatible
local service, but this repository does not provide a Windows service helper.

This development service binds only to `127.0.0.1:8787`. It does not access Suno credentials or install extensions. Use authorized clips only.

## Start

From the suno-cli repository, enqueue while the service is stopped:

```sh
python3 scripts/remote_downloader.py enqueue CLIP_UUID --format mp3
python3 scripts/remote_downloader.py serve --port 8787
```

State and files are under `~/.suno/remote-downloader`. Do not commit or share this directory. A worker token is generated on first serve in `worker-token`, with mode 0600. Upload URLs contain independent secret capabilities. Only one service/enqueue process can hold the state lock. Offline CLI mutations still require the stopped-service lock. The authenticated admin API below can enqueue additional jobs while the updated service runs; do not stop active work. The service does not automatically reassign expired or ambiguous jobs.

## Extension remote configuration

Reload the updated unpacked GSuno extension in Chrome. In its side panel, enable Remote automation and enter:

- Claim URL: `http://127.0.0.1:8787/jobs/claim`
- Token: contents of `~/.suno/remote-downloader/worker-token` (copy locally; do not paste into logs/chat)
- Upload origins: `http://127.0.0.1:8787`
- Poll interval: 30 seconds

Choose **Approve access & save**, then **Poll now**. Keep Chrome running and complete any provider verification manually. No provider verification or access restriction is bypassed.

## Report from terminal

```sh
python3 scripts/remote_downloader.py status
```

This prints safe JSON with job ID, clip ID, status, receipt status, received file path, byte count and SHA-256, without the worker token or upload URL. The helper reports service state; `suno download --source remote-pool` and generation/status `--download DIR --source remote-pool` now consume this service through its admin API.

`received` means raw bytes passed a basic container-signature check and were atomically stored; it does not prove audio decoding/playability. Verify the received file with an audio player or decoder before reporting download success. Receipt `uploaded` independently confirms the worker acknowledged upload. Failed receipt delivery does not discard received files. Duplicate event IDs are accepted only for identical payloads. A repeated PUT is rejected, not silently overwritten.

## Limits

Development use only: 256 MiB maximum upload retained in memory, 30-minute lease, one serialized request mutation. Restart preserves claims; interrupted/ambiguous jobs require manual reconciliation. HTTP is loopback-only; do not expose this service to a network without additional hardening and TLS. If a crash occurs between file rename and state persistence, reconcile the output file before retrying. No automatic fallback or transcoding occurs in this service.

Run tests:

```sh
python3 -m unittest discover -s tests -p 'test_remote_downloader.py' -v
```

## Multiple devices and connectivity probes

Use the latest sibling `gsuno-downloader` extension and set a distinct **Device
identifier** on each browser (for example `office-chrome` and `laptop-chrome`).
IDs are 1–128 ASCII letters, digits, `_` or `-`; they are routing labels, not
credentials. Keep the bearer token configured. This loopback service supports
multiple browser installations on the **same host**; `127.0.0.1` on a different
computer is that computer, not this server. Remote-host deployment is not provided.

The following are **offline administration commands**: run only after the
service has been safely stopped, never during active work. They refuse to write
while any server/admin process holds this state directory's lock. Device registration, probes, reassignment and distribution have no
live admin API; the authenticated download enqueue API is described below. Do not stop the existing service or edit its live state merely
to queue a probe. Updating this script does not hot-reload an already running
server; defer restart until work has completed or has been reconciled.

```sh
# --state-dir DIR goes before the command when using an isolated state directory.
python3 scripts/remote_downloader.py register office-chrome
python3 scripts/remote_downloader.py register laptop-chrome
python3 scripts/remote_downloader.py probe --device office-chrome
python3 scripts/remote_downloader.py probe --device laptop-chrome
python3 scripts/remote_downloader.py enqueue CLIP_UUID --format wav --device office-chrome
# Without --device, downloads are available to the first compatible idle poller.
python3 scripts/remote_downloader.py enqueue CLIP_UUID --format mp3
python3 scripts/remote_downloader.py reassign JOB_ID --device laptop-chrome
python3 scripts/remote_downloader.py distribute
python3 scripts/remote_downloader.py serve --port 8787
# Read-only status is safe while serving:
python3 scripts/remote_downloader.py status
```

Registration is optional: valid claims automatically register the device and bind
it to its installation `worker_id`. A different worker claiming the same device
ID receives 409; configure unique identifiers rather than sharing one. Claims
advertise `accept_types: ["download", "probe"]`, format capabilities and device
ID. Older workers without these fields can claim downloads only (device defaults
to worker ID). Every new response includes matching `type` and `device_id`;
receipts must match the claimed worker/device/type. Claiming and receipt updates
are atomic within the service. Legacy persisted jobs remain readable; previously
claimed jobs are never reset on restart.

`probe` generates a fresh unique job ID on each invocation. Only its target device
can claim it, and only when the extension is idle. The response has just job ID,
`type: "probe"` and device ID; no provider request, audio conversion or upload
occurs. A durable, idempotent `status: "probed"` receipt marks completion and sets
`last_probed`. A successful probe proves claim/receipt connectivity **only**, not
provider availability. Pending receipt delivery can delay further claims.

### Interpreting status and distributing safely

Status JSON includes a `devices` array with `assigned_jobs` (including completed
history), `active_jobs`, observed `last_seen` claim time, optional `last_probed`
receipt time, `recent_poll`, and derived `activity`:

- `available`: a valid claim poll within 120 seconds, without unresolved work.
  This is a scheduling hint, not a guaranteed online/provider-ready assertion.
- `busy_lease`: unresolved claimed work with an unexpired 30-minute reservation.
  A browser can be busy without recent polls; the extension polls only when idle.
- `reconciliation_required`: unresolved work without a live lease, including
  expired claims and ambiguous outcomes. This **does not mean safe to retry**.
- `offline_inferred`: no recent poll and no unresolved work (including registered
  devices that have never polled). Absence of evidence is not proof of shutdown.

Probe acknowledgment does not refresh `last_seen`, and a recent/old `last_probed`
does not make a device available. Receipt retries do not rerun a probe or refresh
its acknowledgment timestamp. Status excludes submission URLs/headers and tokens.

`reassign` changes routing **only for queued jobs**. `distribute` is an explicit
operator action balancing **all queued downloads**, including previously targeted
ones, across recently polling available devices advertising compatible formats
and download support. It deterministically balances by queued-job count, does not
move probes, and leaves incompatible jobs untouched. Stop/restart promptly if
using this offline operation: observations older than 120 seconds are excluded.
No automatic redistribution on missed polls, lease expiry, errors or restart is
performed. No device means no distribution; untargeted downloads remain first-poll
wins. Claimed jobs reserve the device until a terminal result; a device/worker
with an unresolved or ambiguous job cannot acquire another job even after expiry.

**Never automatically reassign claimed, started/processing, uploading or unknown
jobs.** A lost response, browser crash or expired lease may hide completed
provider work or a successful PUT. Reconcile the job identity, local output file
and hash, stored `received` state and worker receipt/outbox first. File rename can
succeed before state persistence; verify actual disk bytes as well. This helper
deliberately provides no force-reset/requeue switch or automated reconciliation.
After proving the outcome, plan explicit operator recovery (and a new unique job
ID only if a new download is actually necessary); do not edit live state or
repeat a PUT solely because its receipt failed.


## Rust CLI integration and live admin API

The service must be started from the updated script to provide these endpoints.
Existing running processes do not hot-reload. Reconcile active work, then arrange
an operator-controlled safe restart; never stop a claimed/ambiguous job merely to
upgrade. No restart is required for enqueueing once this version is running.

All admin requests require `Authorization: Bearer <worker-token>`; use a local
HTTP client reading the token file, not a shell argument containing the token.
The API reuses the existing worker credential and process/thread state lock:

- `GET /admin/health`: protocol version and supported formats (no mutation).
- `POST /admin/jobs`: JSON `{"content_id":"CLIP_UUID","format":"mp3"}`
  (`mp3` or `wav`), returns HTTP 201 with `job_id` and `status`.
- `GET /admin/jobs/JOB_ID`: allowlisted JSON `job_id`, `content_id`, `filetype`,
  `status`, and, when received, `bytes`, `sha256`, `receipt_status`.
  Never returns upload capabilities, worker credentials, or filesystem paths.
- `GET /admin/jobs/JOB_ID/file`: authenticated raw received bytes, otherwise 409;
  the path is derived inside the state directory, not accepted from state/caller.

Enqueue does not modify/reassign existing claims. Existing device capability
matching, one unresolved claim per worker/device, leases, receipts, duplicate
receipt handling and ambiguous-outcome reconciliation remain unchanged. New jobs
are unassigned so idle compatible workers can claim them normally. The admin API
is not an idempotent submission API: do not repeat a POST after an uncertain HTTP
outcome. Inspect service `status` first. It does not cancel jobs when a CLI exits.

CLI configuration (no extra flags on each generation command):

- Global `--source remote-pool`, accepted before or after a subcommand.
- `SUNO_REMOTE_POOL_URL`: default `http://127.0.0.1:8787`; HTTP numeric loopback
  origin only, no URL credentials/query/path/fragment, no redirects or proxies.
- `SUNO_REMOTE_POOL_TOKEN_FILE`: default `~/.suno/remote-downloader/worker-token`.
  `serve --token-file PATH` selects a service token file; use the same file.

The CLI checks authenticated admin availability before costly generation when
remote `--download` is requested, but this is not proof a browser is online.
Each clip is enqueued once, polled at one second up to 30 minutes, and received
through the authenticated file endpoint. Each HTTP call has a 5s connect, 30s
inactivity and 120s total bound. JSON responses are capped at 32 KiB; files at
256 MiB. Failed/cancelled/unknown jobs return errors without provider fallback.
Timeouts do not cancel or reassign jobs; use their IDs in errors to reconcile.
A failed request after enqueue can leave a job running: consult service status
before retrying. Batch callers retain ordinary per-clip successes and errors.
Downloads currently process clips sequentially; independent CLI clients share the
pool, but one generation command does not enqueue its entire batch concurrently.

Rust verifies nonempty length, expected byte count, SHA-256 and basic container
signature before atomic no-clobber publication from a unique sibling `.part`.
Publication uses a same-directory hard link followed by unlink (atomic exclusive
creation, unlike rename which can overwrite). Filesystems without hard-link
support fail safely. Existing destination files are never replaced. Generation
MP3s then use the existing ID3/lyrics pipeline, so their final digest can differ
from the raw service digest. Basic signatures are not an audio decode/audition.
Timing output includes `download.remote_pool`, existing totals and tagging.

Tests use disposable loopback mocks and temporary state; they do not download
from providers or spend credits. Live integration requires an updated running
service and an authorized browser worker; it is not established by mock tests.

## Lyrics

The service transports audio only and does not create LRC sidecars. Use
`suno timed-lyrics CLIP_UUID --lrc > song.lrc` as a separate authenticated Suno
API request when alignment is available. Automatic LRC export is not implemented.
