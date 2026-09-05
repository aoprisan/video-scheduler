# Release Room

An in-house YouTube release scheduler for game studios. Release Room is a single Rust binary
that serves a small web UI and runs an upload worker in the same process: you drop a video into
the browser, choose **when it uploads** and **when it becomes public**, and the worker uploads it
privately and hands YouTube the publication time.

Runs on macOS or Linux. One YouTube channel and one worker per data directory. SQLite stores the
queue; Google OAuth enables unattended uploads. No database server is needed.

> The original Python CLI (`video-marketing`) has been retired; this crate replaces it. Existing
> `.data` directories keep working — the queue schema is migrated in place on first open, and
> credentials written by the old CLI are reused until you reconnect through the web UI.

## Quick start

Install a stable Rust toolchain (edition 2024, Rust 1.88+), then:

```bash
cargo run --release
```

That serves <http://localhost:3000> and starts the worker. The data directory defaults to `.data`
in the working directory and is created with owner-only permissions.

| Setting | Flag | Environment variable | Default |
| --- | --- | --- | --- |
| Data directory | `--data-dir` | `VIDEO_MARKETING_DATA_DIR` | `.data` |
| Listen address | `--bind` | `RELEASE_ROOM_BIND` | `127.0.0.1:3000` |
| Public origin | `--public-url` | `RELEASE_ROOM_PUBLIC_URL` | `http://localhost:3000` |
| OAuth client JSON | `--client-secrets` | `YOUTUBE_CLIENT_SECRETS` | `<data-dir>/client_secret.json` |
| Basic-auth password | — | `RELEASE_ROOM_PASSWORD` | unset (loopback only) |

Subcommands: no subcommand serves the UI and the worker; `release-room list` prints the queue as
JSON; `release-room worker [--once]` runs the worker without the web UI, which is useful under cron.
Serving and the standalone worker each take an exclusive lock on the data directory, so only one of
them can run at a time; `list` reads the queue without the lock and works while the server is up.

### Connect YouTube

The web UI uses a **Web application** OAuth client, not the Desktop client the old CLI used.

1. Create a Google Cloud project and enable **YouTube Data API v3**.
2. Configure the OAuth consent screen. Add your Google account as a test user if using Testing mode.
3. Create an OAuth client with application type **Web application** and add the authorized redirect
   URI `http://localhost:3000/auth/callback` — your public URL plus `/auth/callback`.
4. Save the downloaded JSON as `client_secret.json` in the data directory, or point
   `YOUTUBE_CLIENT_SECRETS` at it.
5. Open **Connect YouTube** in the UI and choose the channel you intend to upload to.

The authorization code exchange uses PKCE and a one-time state cookie. Offline credentials are
saved to `<data-dir>/token.json` with owner-only permissions. The `youtube.force-ssl` scope is used
because scheduling an uploaded video requires `videos.update`; the narrower `youtube.upload` scope
does not support that method.

A queue is bound to the channel it was first connected to. Reconnecting a different channel is
refused — use a separate data directory per channel. Never commit credentials, queue files, or
upload session URLs.

**Google project requirement:** uploads from unverified API projects created after July 28, 2020
are restricted to private viewing until the project passes a YouTube API compliance audit.
Public scheduling requires an eligible project. OAuth consent verification and the YouTube audit
are separate requirements. External OAuth apps in Testing mode generally receive refresh tokens
that expire after seven days, so reconnect or configure the app appropriately for ongoing use.
See [upload restrictions](https://developers.google.com/youtube/v3/docs/videos/insert),
[update scopes](https://developers.google.com/youtube/v3/docs/videos/update), and
[token expiration](https://developers.google.com/identity/protocols/oauth2#expiration).

### Schedule a video

**Schedule video** takes the file plus its metadata. The browser uploads it to this server first,
where it is stored under `<data-dir>/media/` and fingerprinted with SHA-256; the queue then owns
its own copy, so the original can move or change afterwards. Accepted extensions are MP4, MOV,
WebM, MKV, M4V, and AVI, up to 256 GB.

Dates are entered in the browser's local timezone or an explicit UTC offset and stored as UTC.
Leave the upload time empty to start on the next worker pass. Validation mirrors YouTube's limits:
a title of 1–100 characters, a description of at most 5000 bytes, tags totalling at most 500
characters, a valid category, an explicit audience answer, and a publication time more than
60 seconds after both now and the upload time.

That 60-second floor is a guard against stale schedules, not a promise that a video can finish
uploading in that time. Choose enough lead time for transfer and YouTube processing; several hours
is a useful starting point. Use the synthetic media checkbox to disclose realistic altered content.

### Watch it run

The queue lists every video with its state, live upload progress, and both dates, filtered by
**All videos**, **Upcoming**, **Needs attention**, or **Scheduled**. The detail view plays the
stored file, shows the schedule and attempt count, links to YouTube Studio once a video ID exists,
and offers retry or cancel where they apply.

The worker uploads privately using persisted resumable sessions, saves the YouTube video ID, then
sets `status.publishAt` with `privacyStatus=private`. YouTube performs delayed publication after
accepting the schedule, even if this server is subsequently offline. The worker checks remote state
before repeating a scheduling request and preserves mutable status settings.
See [scheduled publication behavior](https://developers.google.com/youtube/v3/docs/videos#status.publishAt)
and the [resumable protocol](https://developers.google.com/youtube/v3/guides/using_resumable_upload_protocol).

`scheduled` means YouTube accepted the schedule (or the video was already public during recovery).
It does **not** verify eventual publication, processing success, or freedom from platform
restrictions. Use Studio to verify those. Release Room does not monitor published videos or manage
thumbnails and playlists.

## Queue states and recovery

- `queued`: waiting for upload time; `uploading`: an attempt is in progress or was interrupted.
- `retry`: transient failure; automatic exponential backoff (8 attempts, capped at one hour,
  respecting Retry-After).
- `failed`: permanent error or exhausted retries; the reason appears on the detail page.
- `missed`: deadline passed before scheduling; an uploaded video remains private unless a previous
  scheduling request was already accepted. Check Studio if the last request had an uncertain outcome.
- `scheduled`: YouTube accepted the schedule; `cancelled`: locally cancelled before upload began.

Cancellation is available only before any upload session exists. To cancel or alter publication
after upload, use YouTube Studio. Retrying a failed, missed, or retrying job asks for a fresh
publication time and resets the attempt counter.

On startup, interrupted uploads are returned to `retry` and query YouTube for the received byte
offset, including recovering a completed upload whose final response was lost. Expired or lost
sessions are never automatically replaced: inspect Studio before creating a new job, since the
original may have completed. A file lock on the data directory prevents concurrent workers from
uploading the same queue. Store the data directory on a local disk, not a network filesystem, and
back it up securely while the server is stopped.

`worker --once` exits with 1 if the queue contains failed, missed, or retrying jobs. SIGINT and
SIGTERM finish the current job before stopping; a forced stop is recoverable from the queue.

## Running beyond localhost

The default configuration binds to loopback with no password. Binding to any other address requires
both `RELEASE_ROOM_PASSWORD` of at least 24 characters and an `https://` public URL, terminated by
your own reverse proxy — the server refuses to start otherwise. The password is checked with HTTP
Basic auth under the username `studio`.

Requests must carry the expected `Host`; writes must carry a matching `Origin`, and API writes an
additional per-process CSRF token. Responses set a restrictive `Content-Security-Policy`,
`no-store`, `nosniff`, and `no-referrer`. Give the proxy a generous request body limit and timeout:
video uploads pass through it.

Set an absolute data directory so every invocation uses the same queue:

```bash
export VIDEO_MARKETING_DATA_DIR=/absolute/path/to/release-room-data
```

## Data directory

```
<data-dir>/queue.sqlite3   queue and settings (WAL)
<data-dir>/media/          uploaded source videos
<data-dir>/token.json      OAuth credentials, mode 0600
<data-dir>/client_secret.json  OAuth client, unless YOUTUBE_CLIENT_SECRETS points elsewhere
<data-dir>/worker.lock     process lock
```

## Development

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo run
```

CI runs those three checks on every push and pull request. The crate has no automated tests yet;
`cargo test` currently passes vacuously, and a real upload requires your own OAuth client and an
eligible channel.

Code layout: `main.rs` startup, CLI, and safety checks; `web.rs` routes, auth middleware, and the
upload endpoint; `views.rs` server-rendered maud markup; `store.rs` the SQLite queue; `worker.rs`
execution and recovery; `youtube.rs` OAuth and the resumable upload protocol; `model.rs` timestamps,
validation, and fingerprinting. Static assets live in `web/`, and `docs/design.md` records the
visual system the UI implements.
