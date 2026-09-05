import logging
import random
from pathlib import Path

from .models import fingerprint, utc_now
from .youtube import APIError

log = logging.getLogger(__name__)
MAX_ATTEMPTS = 8


def process(store, youtube, job, clock=utc_now):
    job = store.update(job["id"], state="uploading", attempts=job["attempts"] + 1)
    try:
        if not job["video_id"]:
            if job["publish_at"] <= clock() + 60:
                store.update(job["id"], state="missed", last_error="Publication deadline missed")
                return
            size, digest = fingerprint(Path(job["path"]))
            if (size, digest) != (job["size"], job["sha256"]):
                raise APIError(
                    "Video file changed since scheduling; restore it or create a new job"
                )
            if not job["session_url"]:
                url = youtube.start(job)
                # Commit the session before sending ANY video bytes.
                job = store.update(job["id"], session_url=url)
            video_id = youtube.upload(job)
            # Commit remote identity before setting publishAt, so retries cannot upload again.
            job = store.update(job["id"], video_id=video_id)
        if youtube.schedule(job, clock):
            store.update(job["id"], state="scheduled", last_error=None)
            log.info("Job %s: YouTube accepted schedule (video %s)", job["id"], job["video_id"])
        else:
            store.update(
                job["id"],
                state="missed",
                last_error="Upload completed privately, but publication deadline was missed",
            )
    except APIError as exc:
        retry = exc.retryable and job["attempts"] < MAX_ATTEMPTS
        delay = max(exc.retry_after, min(3600, 30 * 2 ** (job["attempts"] - 1)))
        store.update(
            job["id"],
            state="retry" if retry else "failed",
            last_error=str(exc),
            next_attempt=clock() + delay + random.uniform(0, 5),
        )
        log.warning("Job %s: %s", job["id"], exc)
    except (OSError, ValueError) as exc:
        # Avoid storing paths or remote payloads from unexpected underlying errors.
        store.update(
            job["id"],
            state="failed",
            last_error=f"Local file or data error ({type(exc).__name__}); check job inputs",
        )
        log.warning("Job %s: local file or data error", job["id"])


def run_once(store, youtube, clock=utc_now) -> int:
    """Caller holds worker_lock for the lifetime of its worker process."""
    jobs = store.due(clock())
    for job in jobs:
        process(store, youtube, job, clock)
    return len(jobs)
