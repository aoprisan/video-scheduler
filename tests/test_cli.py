import json
import subprocess
import sys
from pathlib import Path

import pytest

from video_marketing.cli import main
from video_marketing.models import parse_time, validate_metadata


def test_cli_schedule_preview_list_cancel_in_separate_processes(tmp_path):
    video = tmp_path / "video.mp4"
    video.write_bytes(b"video")
    command = [sys.executable, "-m", "video_marketing.cli", "--data-dir", str(tmp_path / "data")]

    def run(*args):
        result = subprocess.run([*command, *args], capture_output=True, text=True, check=True)
        return result.stdout

    saved = json.loads(
        run(
            "schedule",
            str(video),
            "--title",
            "Demo",
            "--publish-at",
            "2030-01-02T18:00:00+02:00",
            "--not-made-for-kids",
        )
    )
    assert saved["state"] == "queued"
    assert saved["publish_at"] == "2030-01-02T16:00:00Z"
    before = json.loads(run("show", saved["id"]))
    preview = json.loads(run("worker", "--dry-run"))
    assert preview[0]["upload_body"]["status"]["privacyStatus"] == "private"
    assert json.loads(run("show", saved["id"])) == before
    assert "session_url" not in before
    assert json.loads(run("list", "--json"))[0]["id"] == saved["id"]
    assert json.loads(run("cancel", saved["id"]))["state"] == "cancelled"
    assert "No jobs due" in run("worker", "--once")


def test_worker_with_due_job_needs_auth_without_changing_state(store, job, capsys):
    store.update(job["id"], next_attempt=0)
    assert main(["--data-dir", str(store.directory), "worker", "--once"]) == 1
    assert "No YouTube credentials" in capsys.readouterr().err
    assert store.get(job["id"])["state"] == "queued"


@pytest.mark.parametrize("value", ["2030-01-01", "2030-01-01T10:00:00", "nonsense"])
def test_naive_or_invalid_dates_rejected(value):
    with pytest.raises(ValueError):
        parse_time(value)


@pytest.mark.parametrize(
    "title,description,tags",
    [
        ("", "", []),
        ("x" * 101, "", []),
        ("<invalid>", "", []),
        ("ok", "é" * 2501, []),
        ("ok", "", [""]),
        ("ok", "", ["x" * 501]),
    ],
)
def test_invalid_metadata_rejected(title, description, tags):
    with pytest.raises(ValueError):
        validate_metadata(title, description, tags, "22")


def test_queue_and_token_permissions(store, tmp_path):
    from google.oauth2.credentials import Credentials

    from video_marketing.auth import save_credentials

    path = tmp_path / "token.json"
    save_credentials(
        path,
        Credentials(
            token="test", refresh_token="refresh", client_id="client", client_secret="secret"
        ),
    )
    assert path.stat().st_mode & 0o777 == 0o600
    assert (store.directory / "queue.sqlite3").stat().st_mode & 0o777 == 0o600
    assert not list(tmp_path.glob(".oauth-*"))


def test_empty_file_and_invalid_schedule_rejected(store, job, tmp_path):
    base = {
        key: job[key]
        for key in (
            "title",
            "description",
            "tags",
            "category",
            "made_for_kids",
            "synthetic_media",
            "upload_at",
            "publish_at",
        )
    }
    empty = tmp_path / "empty.mp4"
    empty.touch()
    with pytest.raises(ValueError, match="empty"):
        store.add(path=empty, **base)
    with pytest.raises(ValueError, match="60 seconds"):
        store.add(path=Path(job["path"]), **{**base, "publish_at": job["upload_at"]})
