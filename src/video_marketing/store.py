import fcntl
import json
import sqlite3
import uuid
from contextlib import contextmanager
from pathlib import Path

from .models import fingerprint, utc_now, validate_metadata

SCHEMA = """
CREATE TABLE IF NOT EXISTS jobs (
    id TEXT PRIMARY KEY,
    path TEXT NOT NULL,
    size INTEGER NOT NULL,
    sha256 TEXT NOT NULL,
    title TEXT NOT NULL,
    description TEXT NOT NULL,
    tags TEXT NOT NULL,
    category TEXT NOT NULL,
    made_for_kids INTEGER NOT NULL,
    synthetic_media INTEGER NOT NULL,
    upload_at REAL NOT NULL,
    publish_at REAL NOT NULL,
    state TEXT NOT NULL DEFAULT 'queued'
      CHECK (state IN ('queued','uploading','retry','scheduled','failed','missed','cancelled')),
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt REAL NOT NULL,
    session_url TEXT,
    video_id TEXT,
    last_error TEXT,
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL
);
CREATE INDEX IF NOT EXISTS jobs_due ON jobs(state, next_attempt);
"""


class Store:
    def __init__(self, directory: Path):
        self.directory = directory.expanduser().resolve()
        self.directory.mkdir(parents=True, exist_ok=True, mode=0o700)
        self.db = sqlite3.connect(self.directory / "queue.sqlite3", timeout=30)
        (self.directory / "queue.sqlite3").chmod(0o600)
        self.db.row_factory = sqlite3.Row
        self.db.execute("PRAGMA journal_mode=WAL")
        self.db.executescript(SCHEMA)

    def close(self):
        self.db.close()

    @contextmanager
    def worker_lock(self):
        """An OS lock is released even on SIGKILL; no stale lease can cause duplicates."""
        with (self.directory / "worker.lock").open("a") as handle:
            try:
                fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError as exc:
                raise ValueError(
                    "A worker or queue maintenance command is already running"
                ) from exc
            try:
                yield
            finally:
                fcntl.flock(handle, fcntl.LOCK_UN)

    @staticmethod
    def decode(row):
        job = dict(row)
        job["tags"] = json.loads(job["tags"])
        return job

    def get(self, job_id: str) -> dict:
        row = self.db.execute("SELECT * FROM jobs WHERE id = ?", (job_id,)).fetchone()
        if row is None:
            raise ValueError(f"Unknown job: {job_id}")
        return self.decode(row)

    def list(self) -> list[dict]:
        return [
            self.decode(row) for row in self.db.execute("SELECT * FROM jobs ORDER BY upload_at")
        ]

    def add(
        self,
        *,
        path: Path,
        title: str,
        description: str,
        tags: list[str],
        category: str,
        made_for_kids: bool,
        synthetic_media: bool,
        upload_at: float,
        publish_at: float,
        now: float | None = None,
    ) -> dict:
        now = utc_now() if now is None else now
        validate_metadata(title, description, tags, category)
        if publish_at <= max(now, upload_at) + 60:
            raise ValueError("Publication must be over 60 seconds after now and upload time")
        path = path.expanduser().resolve(strict=True)
        size, digest = fingerprint(path)
        job_id = str(uuid.uuid4())
        with self.db:
            self.db.execute(
                """INSERT INTO jobs
                (id,path,size,sha256,title,description,tags,category,made_for_kids,
                 synthetic_media,upload_at,publish_at,next_attempt,created_at,updated_at)
                VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)""",
                (
                    job_id,
                    str(path),
                    size,
                    digest,
                    title,
                    description,
                    json.dumps(tags),
                    category,
                    made_for_kids,
                    synthetic_media,
                    upload_at,
                    publish_at,
                    upload_at,
                    now,
                    now,
                ),
            )
        return self.get(job_id)

    def update(self, job_id: str, **fields) -> dict:
        allowed = {
            "state",
            "attempts",
            "next_attempt",
            "session_url",
            "video_id",
            "last_error",
            "publish_at",
        }
        if not fields or not fields.keys() <= allowed:
            raise ValueError("Invalid job update")
        fields["updated_at"] = utc_now()
        with self.db:
            self.db.execute(
                f"UPDATE jobs SET {', '.join(f'{key} = ?' for key in fields)} WHERE id = ?",
                (*fields.values(), job_id),
            )
        return self.get(job_id)

    def due(self, now: float) -> list[dict]:
        return [
            self.decode(row)
            for row in self.db.execute(
                """SELECT * FROM jobs WHERE state IN ('queued','retry','uploading')
               AND next_attempt <= ? ORDER BY upload_at""",
                (now,),
            )
        ]

    def cancel(self, job_id: str) -> dict:
        with self.worker_lock():
            job = self.get(job_id)
            if job["state"] not in {"queued", "failed", "missed", "retry"} or job["session_url"]:
                raise ValueError(
                    "Only jobs with no upload session can be cancelled locally; "
                    "manage uploaded videos in YouTube Studio"
                )
            return self.update(job_id, state="cancelled")

    def retry(self, job_id: str, publish_at: float | None = None) -> dict:
        with self.worker_lock():
            job = self.get(job_id)
            if job["state"] not in {"failed", "missed", "retry"}:
                raise ValueError("Only failed, missed, or retrying jobs can be retried")
            when = job["publish_at"] if publish_at is None else publish_at
            if when <= utc_now() + 60:
                raise ValueError("Provide --publish-at over 60 seconds in the future")
            return self.update(
                job_id,
                state="retry",
                publish_at=when,
                attempts=0,
                next_attempt=utc_now(),
                last_error=None,
            )
