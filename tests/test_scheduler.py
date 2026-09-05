from pathlib import Path

import pytest

from video_marketing.models import parse_time
from video_marketing.store import Store
from video_marketing.worker import run_once
from video_marketing.youtube import API, UPLOAD_API, YouTube

from .conftest import NOW, SESSION


def run(store, remote, now=NOW):
    with store.worker_lock():
        return run_once(store, YouTube(remote), lambda: now)


def test_upload_time_then_private_upload_and_delayed_publish(store, job, remote):
    assert run(store, remote, NOW - 1) == 0
    assert remote.calls == []
    assert run(store, remote) == 1
    saved = store.get(job["id"])
    assert saved["state"] == "scheduled"
    assert saved["video_id"] == "video_123"
    assert bytes(remote.received) == Path(job["path"]).read_bytes()
    assert remote.status["privacyStatus"] == "private"
    assert parse_time(remote.status["publishAt"]) == NOW + 3600
    assert remote.status["containsSyntheticMedia"] is True
    assert remote.status["selfDeclaredMadeForKids"] is False
    assert run(store, remote, NOW + 10) == 0


def test_lost_final_upload_response_recovers_without_duplicate(store, job, remote):
    remote.lose_upload_response = True
    run(store, remote)
    saved = store.get(job["id"])
    assert saved["state"] == "retry"
    assert saved["video_id"] is None
    assert saved["session_url"] == SESSION
    assert "sensitive-session" not in saved["last_error"]
    # A genuinely new DB connection and client, reusing only durable state and the remote server.
    restarted = Store(store.directory)
    try:
        run(restarted, remote, saved["next_attempt"])
        assert restarted.get(job["id"])["state"] == "scheduled"
    finally:
        restarted.close()
    assert sum(method == "POST" for method, _, _ in remote.calls) == 1
    assert len(remote.received) == job["size"]


def test_lost_schedule_response_is_reconciled(store, job, remote):
    remote.lose_schedule_response = True
    run(store, remote)
    saved = store.get(job["id"])
    assert saved["state"] == "retry"
    assert saved["video_id"] == "video_123"
    run(store, remote, saved["next_attempt"])
    assert store.get(job["id"])["state"] == "scheduled"
    assert sum(method == "PUT" and url == API for method, url, _ in remote.calls) == 1


def test_schedule_retry_does_not_require_original_file(store, job, remote):
    remote.fail_schedule_once = True
    run(store, remote)
    saved = store.get(job["id"])
    Path(job["path"]).unlink()
    run(store, remote, saved["next_attempt"])
    assert store.get(job["id"])["state"] == "scheduled"
    assert sum(url == UPLOAD_API for _, url, _ in remote.calls) == 1


def test_interrupted_in_progress_job_is_recovered(store, job, remote):
    client = YouTube(remote)
    store.update(job["id"], state="uploading", session_url=client.start(job))
    remote.received.extend(Path(job["path"]).read_bytes()[:256])
    run(store, remote)
    assert store.get(job["id"])["state"] == "scheduled"
    actual = [
        kwargs
        for method, url, kwargs in remote.calls
        if method == "PUT" and url == SESSION and kwargs["data"]
    ]
    assert actual[0]["headers"]["Content-Range"].startswith("bytes 256-")


def test_deadline_missed_before_upload_makes_no_requests(store, job, remote):
    run(store, remote, NOW + 3600)
    assert store.get(job["id"])["state"] == "missed"
    assert not remote.calls


def test_deadline_missed_during_upload_leaves_video_private(store, job, remote):
    ticks = iter([NOW, NOW, NOW + 3600])
    with store.worker_lock():
        run_once(store, YouTube(remote), lambda: next(ticks))
    saved = store.get(job["id"])
    assert saved["state"] == "missed"
    assert saved["video_id"] == "video_123"
    assert remote.status["privacyStatus"] == "private"
    assert "publishAt" not in remote.status


def test_changed_file_fails_before_sending_bytes(store, job, remote):
    Path(job["path"]).write_bytes(b"changed")
    run(store, remote)
    assert store.get(job["id"])["state"] == "failed"
    assert not remote.calls


def test_second_worker_cannot_take_lock(store):
    other = Store(store.directory)
    try:
        with store.worker_lock(), pytest.raises(ValueError, match="already running"):
            with other.worker_lock():
                pytest.fail("Second worker acquired lock")
        with other.worker_lock():
            pass
    finally:
        other.close()


def test_cancelled_job_is_not_uploaded(store, job, remote):
    assert store.cancel(job["id"])["state"] == "cancelled"
    assert run(store, remote) == 0


def test_cannot_cancel_started_upload(store, job):
    store.update(job["id"], state="retry", session_url=SESSION)
    with pytest.raises(ValueError, match="no upload session"):
        store.cancel(job["id"])


def test_retry_retains_remote_identity_and_can_move_deadline(store, job):
    store.update(job["id"], state="failed", session_url=SESSION, video_id="video_123")
    saved = store.retry(job["id"], NOW + 7200)
    assert saved["session_url"] == SESSION
    assert saved["video_id"] == "video_123"
    assert saved["publish_at"] == NOW + 7200
    assert saved["attempts"] == 0


def test_retry_stops_after_maximum_attempts(store, job, remote):
    store.update(job["id"], attempts=7)
    remote.fail_schedule_once = True
    run(store, remote)
    saved = store.get(job["id"])
    assert saved["state"] == "failed"
    assert saved["attempts"] == 8


def test_one_failed_job_does_not_block_other_due_jobs(store, job, remote, tmp_path):
    second = tmp_path / "other.mp4"
    second.write_bytes(b"another video")
    next_job = store.add(
        path=second,
        title="Second",
        description="",
        tags=[],
        category="22",
        made_for_kids=True,
        synthetic_media=False,
        upload_at=NOW + 1,
        publish_at=NOW + 3600,
        now=NOW,
    )
    Path(job["path"]).unlink()
    assert run(store, remote, NOW + 1) == 2
    assert store.get(job["id"])["state"] == "failed"
    assert store.get(next_job["id"])["state"] == "scheduled"
