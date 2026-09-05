# Video Marketing

A Python CLI for scheduling **when a video uploads** and **when it becomes public on YouTube**.
Runs on macOS or Linux, with one YouTube account and one worker per data directory.
SQLite stores the queue; Google OAuth enables unattended uploads. No database server is needed.

## Quick start

Install Python 3.11+ and [uv](https://docs.astral.sh/uv/getting-started/installation/), then:

```bash
uv sync --locked
uv run video-marketing init
uv run video-marketing --help
```

### Connect YouTube

1. Create a Google Cloud project and enable **YouTube Data API v3**.
2. Configure the OAuth consent screen. Add your Google account as a test user if using Testing mode.
3. Create an OAuth client with application type **Desktop app**, then download its JSON.
4. Run this on a machine with a browser, choosing the account/channel you intend to upload to:

```bash
uv run video-marketing auth --client-secrets /absolute/path/to/client_secret.json
```

The browser returns to a temporary localhost port. Offline credentials are saved to `.data/token.json`
with owner-only permissions. The `youtube.force-ssl` scope is used because scheduling an uploaded
video requires `videos.update`; the narrower `youtube.upload` scope does not support that method.
Use a separate data directory for each account; always reconnect the same account for an existing queue.
Never commit credentials, queue files, or upload session URLs.

**Google project requirement:** uploads from unverified API projects created after July 28, 2020
are restricted to private viewing until the project passes a YouTube API compliance audit.
Public scheduling requires an eligible project. OAuth consent verification and the YouTube audit
are separate requirements. External OAuth apps in Testing mode generally receive refresh tokens
that expire after seven days, so reconnect or configure the app appropriately for ongoing use.
See [upload restrictions](https://developers.google.com/youtube/v3/docs/videos/insert),
[update scopes](https://developers.google.com/youtube/v3/docs/videos/update), and
[token expiration](https://developers.google.com/identity/protocols/oauth2#expiration).

### Queue a video

Replace these example dates with future times. Dates require an explicit offset or `Z`; they are
stored as UTC. `+02:00` is Budapest summer time; use the correct offset for your chosen date.

```bash
uv run video-marketing schedule /absolute/path/to/video.mp4 \
  --title "Introducing our product" \
  --description "A quick tour of what we are building." \
  --tag product --tag demo \
  --upload-at '2026-10-01T09:00:00+02:00' \
  --publish-at '2026-10-02T18:00:00+02:00' \
  --not-made-for-kids
```

Omit `--upload-at` to upload on the next worker pass. Set `--made-for-kids` instead when appropriate.
Use `--synthetic-media` to disclose realistic altered or synthetic content. Category defaults to `22`
(People & Blogs); `--category` accepts another YouTube category ID.
Choose enough lead time for transfer and YouTube processing; several hours is a useful starting point.
The minimum accepted lead time is 60 seconds, but that is a guard against stale schedules, not a
promise that a video can finish uploading in that time.

The queue references the original file by absolute path and records its SHA-256 digest. Keep it
available and unchanged until the upload completes. Scheduling the same file again creates another
independent job; retries of an existing job preserve its remote upload identity.

### Preview and run

```bash
# Works without credentials. Validates files and previews every pending job; changes no job state.
uv run video-marketing worker --dry-run

# Runs continuously; keep this process and machine running while uploads are pending.
uv run video-marketing worker

# Alternatively, process all currently due jobs once (suitable for cron).
uv run video-marketing worker --once

uv run video-marketing list
uv run video-marketing show JOB_ID
```

The worker uploads privately using persisted resumable sessions, saves the YouTube video ID,
then sets `status.publishAt` and `privacyStatus=private`. YouTube performs delayed publication
after accepting the schedule, even if this worker is subsequently offline. The worker checks
remote state before repeating a scheduling request and preserves mutable status settings.
See [scheduled publication behavior](https://developers.google.com/youtube/v3/docs/videos#status.publishAt)
and the [resumable protocol](https://developers.google.com/youtube/v3/guides/using_resumable_upload_protocol).

`scheduled` means YouTube accepted the schedule (or the video was already public during recovery).
It does **not** verify eventual publication, processing success, or freedom from platform restrictions.
Use the video URL from `show` and YouTube Studio to verify those. This bootstrap does not continuously
monitor published videos, offer a web dashboard, or manage thumbnails/playlists.

## Recovery and queue management

- `queued`: waiting for upload time; `uploading`: an attempt is in progress or was interrupted.
- `retry`: transient failure; automatic exponential backoff (8 attempts, respecting Retry-After).
- `failed`: permanent error or exhausted retries; inspect `show JOB_ID`.
- `missed`: deadline passed before scheduling; an uploaded video remains private unless a previous
  scheduling request was already accepted. Check Studio if the last request had an uncertain outcome.
- `scheduled`: YouTube accepted the schedule; `cancelled`: locally cancelled before upload began.

```bash
uv run video-marketing cancel JOB_ID
uv run video-marketing retry JOB_ID
uv run video-marketing retry JOB_ID --publish-at '2026-10-03T18:00:00+02:00'
```

Cancellation is supported only before any upload session exists. To cancel or alter publication
after upload, use YouTube Studio. Stop the worker before `cancel`, `retry`, or `auth`; these commands
take the same process lock. Queueing/listing remain available while the worker runs.

On restart, interrupted uploads query YouTube for the received byte offset, including recovering
a completed upload whose final response was lost. Expired/lost sessions are never automatically
replaced: inspect Studio before creating a new job, since the original may have completed.
A local OS lock prevents concurrent workers from uploading the same queue. Store the database on
a local disk, not a network filesystem. Back up the data directory securely while the worker is stopped.

`worker --once` exits with 1 if the queue contains failed, missed, or retrying jobs; use `show` to inspect.
SIGINT/SIGTERM finish the current batch before stopping; a forced stop is recoverable from the queue.

For unattended operation, run the worker under your process supervisor or invoke `worker --once`
periodically with cron. Set an absolute data directory to ensure all invocations use the same queue:

```bash
export VIDEO_MARKETING_DATA_DIR=/absolute/path/to/video-marketing-data
uv run video-marketing init
# Use the same variable for auth, schedule, list, and worker.
```

## Development

```bash
uv sync --locked
uv run pytest
uv run ruff check .
uv run ruff format --check .
```

Tests use temporary databases and a simulated YouTube HTTP transport. They exercise queue timing,
private upload, scheduling, retry/restart recovery, cancellation, and validation without contacting
Google or publishing content. A real upload requires your own OAuth client and eligible channel.

Code layout: `cli.py` commands, `store.py` durable queue and locking, `worker.py` execution/recovery,
`youtube.py` HTTP protocol, `auth.py` OAuth, and `models.py` timestamps/metadata.
