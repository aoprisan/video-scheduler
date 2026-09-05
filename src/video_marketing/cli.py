import argparse
import json
import logging
import os
import signal
import sys
import threading
from pathlib import Path

import requests
from google.auth.exceptions import GoogleAuthError

from .auth import authorized_session, login
from .models import fingerprint, iso_time, parse_time, upload_body, utc_now
from .store import Store
from .worker import run_once
from .youtube import APIError, YouTube


def public_job(job: dict) -> dict:
    result = {key: value for key, value in job.items() if key != "session_url"}
    result["has_upload_session"] = bool(job["session_url"])
    result["youtube_url"] = f"https://youtu.be/{job['video_id']}" if job["video_id"] else None
    for key in ("upload_at", "publish_at", "next_attempt", "created_at", "updated_at"):
        result[key] = iso_time(job[key])
    return result


def output(value):
    print(json.dumps(value, indent=2, ensure_ascii=False))


def parser() -> argparse.ArgumentParser:
    app = argparse.ArgumentParser(description="Schedule YouTube uploads and delayed publication")
    app.add_argument(
        "--data-dir",
        type=Path,
        default=Path(os.environ.get("VIDEO_MARKETING_DATA_DIR", ".data")),
        help="Queue and OAuth directory (default: .data or VIDEO_MARKETING_DATA_DIR)",
    )
    commands = app.add_subparsers(dest="command", required=True)
    commands.add_parser("init", help="Create the local queue")
    auth = commands.add_parser("auth", help="Connect a YouTube account in your browser")
    auth.add_argument("--client-secrets", type=Path, required=True)
    schedule = commands.add_parser("schedule", help="Queue a local video file")
    schedule.add_argument("file", type=Path)
    schedule.add_argument("--title", required=True)
    schedule.add_argument("--description", default="")
    schedule.add_argument("--tag", action="append", default=[], help="Repeat for multiple tags")
    schedule.add_argument("--category", default="22")
    schedule.add_argument("--upload-at", default="now", help="ISO 8601 with offset, or now")
    schedule.add_argument("--publish-at", required=True, help="ISO 8601 with offset")
    audience = schedule.add_mutually_exclusive_group(required=True)
    audience.add_argument("--made-for-kids", action="store_true", dest="made_for_kids")
    audience.add_argument("--not-made-for-kids", action="store_false", dest="made_for_kids")
    schedule.add_argument(
        "--synthetic-media",
        action="store_true",
        help="Disclose realistic altered or synthetic content",
    )
    listing = commands.add_parser("list", help="Show queue status")
    listing.add_argument("--json", action="store_true")
    show = commands.add_parser("show", help="Inspect one job, including errors")
    show.add_argument("job_id")
    cancel = commands.add_parser("cancel", help="Cancel a job that has not started uploading")
    cancel.add_argument("job_id")
    retry = commands.add_parser("retry", help="Retry a failed job using its existing upload")
    retry.add_argument("job_id")
    retry.add_argument("--publish-at", help="Optionally choose a new future publication time")
    worker = commands.add_parser("worker", help="Process the queue (makes real YouTube changes)")
    worker.add_argument("--once", action="store_true", help="Process currently due jobs and exit")
    worker.add_argument("--poll-seconds", type=float, default=15)
    worker.add_argument(
        "--dry-run",
        action="store_true",
        help="Validate and preview all pending jobs without OAuth or uploads",
    )
    return app


def preview(store):
    plans = []
    for job in store.list():
        if job["state"] not in {"queued", "retry", "uploading"}:
            continue
        if not job["video_id"]:
            if fingerprint(Path(job["path"])) != (job["size"], job["sha256"]):
                raise ValueError(f"Video file changed for job {job['id']}")
        plans.append(
            {
                "id": job["id"],
                "upload_at": iso_time(job["upload_at"]),
                "publish_at": iso_time(job["publish_at"]),
                "due_now": job["next_attempt"] <= utc_now(),
                "deadline_missed": job["publish_at"] <= utc_now() + 60,
                "upload_body": upload_body(job),
                "schedule_status": {
                    "privacyStatus": "private",
                    "publishAt": iso_time(job["publish_at"]),
                },
            }
        )
    output(plans)


def execute(args, store):
    if args.command == "init":
        print(f"Queue ready: {store.directory}")
    elif args.command == "auth":
        with store.worker_lock():
            # Replacing credentials while an upload is in flight is unsafe.
            login(args.client_secrets.expanduser(), store.directory / "token.json")
        print("YouTube connected. Offline credentials saved locally.")
    elif args.command == "schedule":
        now = utc_now()
        job = store.add(
            path=args.file,
            title=args.title,
            description=args.description,
            tags=args.tag,
            category=args.category,
            made_for_kids=args.made_for_kids,
            synthetic_media=args.synthetic_media,
            upload_at=now if args.upload_at == "now" else parse_time(args.upload_at),
            publish_at=parse_time(args.publish_at),
            now=now,
        )
        output(public_job(job))
    elif args.command == "list":
        jobs = [public_job(job) for job in store.list()]
        if args.json:
            output(jobs)
        elif not jobs:
            print("No videos queued. Use schedule --help to add one.")
        else:
            for job in jobs:
                print(
                    f"{job['id']}  {job['state']:10}  publish {job['publish_at']}  "
                    f"{json.dumps(job['title'], ensure_ascii=False)}"
                )
    elif args.command == "show":
        output(public_job(store.get(args.job_id)))
    elif args.command == "cancel":
        output(public_job(store.cancel(args.job_id)))
    elif args.command == "retry":
        output(
            public_job(
                store.retry(args.job_id, parse_time(args.publish_at) if args.publish_at else None)
            )
        )
    elif args.command == "worker":
        if args.poll_seconds < 1:
            raise ValueError("--poll-seconds must be at least 1")
        if args.dry_run:
            preview(store)
            return 0
        stop = threading.Event()
        for sig in (signal.SIGTERM, signal.SIGINT):
            signal.signal(sig, lambda *_: stop.set())
        with store.worker_lock():
            # Do not require OAuth for an empty --once invocation.
            if args.once and not store.due(utc_now()):
                print("No jobs due.")
                return 0
            with authorized_session(store.directory / "token.json") as session:
                youtube = YouTube(session)
                while not stop.is_set():
                    run_once(store, youtube)
                    if args.once:
                        return int(
                            any(
                                job["state"] in {"failed", "missed", "retry"}
                                for job in store.list()
                            )
                        )
                    stop.wait(args.poll_seconds)
    return 0


def main(argv=None) -> int:
    args = parser().parse_args(argv)
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
    store = None
    try:
        store = Store(args.data_dir)
        return execute(args, store)
    except (GoogleAuthError, requests.RequestException):
        print(
            "Error: Google authorization/network failure. Check connectivity and run auth again.",
            file=sys.stderr,
        )
        return 1
    except (ValueError, OSError, APIError) as exc:
        print(f"Error: {exc}", file=sys.stderr)
        return 1
    finally:
        if store is not None:
            store.close()


if __name__ == "__main__":
    sys.exit(main())
