from unittest.mock import Mock

import pytest

from video_marketing.models import iso_time
from video_marketing.youtube import APIError, YouTube

from .conftest import NOW, SESSION, response


@pytest.mark.parametrize(
    "status,reason,retryable",
    [
        (400, "invalidTitle", False),
        (401, "unauthorized", False),
        (403, "quotaExceeded", True),
        (403, "forbidden", False),
        (429, "rateLimitExceeded", True),
        (503, "backendError", True),
    ],
)
def test_error_classification(status, reason, retryable):
    session = Mock()
    session.request.return_value = response(
        status, {"error": {"errors": [{"reason": reason}]}}, **{"Retry-After": "125"}
    )
    with pytest.raises(APIError) as caught:
        YouTube(session).request("GET", "https://www.googleapis.com/youtube/v3/videos")
    assert caught.value.retryable is retryable
    assert caught.value.retry_after == 125
    assert reason in str(caught.value)


def test_expired_session_is_not_recreated(job):
    session = Mock()
    session.request.return_value = response(404)
    with pytest.raises(APIError, match="not start a duplicate") as caught:
        YouTube(session).upload({**job, "session_url": SESSION})
    assert not caught.value.retryable
    assert session.request.call_count == 1


def test_multichunk_upload_uses_server_offset(job, remote, monkeypatch):
    monkeypatch.setattr("video_marketing.youtube.CHUNK_SIZE", 256)
    client = YouTube(remote)
    job["session_url"] = client.start(job)
    assert client.upload(job) == "video_123"
    chunks = [
        kwargs["data"]
        for method, _, kwargs in remote.calls
        if method == "PUT" and kwargs.get("data")
    ]
    assert len(chunks) == 3
    assert len(chunks[0]) == 256
    assert sum(map(len, chunks)) == job["size"]


def test_untrusted_session_url_is_rejected_before_credentials_sent(job):
    session = Mock()
    with pytest.raises(APIError, match="unexpected"):
        YouTube(session).upload({**job, "session_url": "https://evil.example/upload"})
    session.request.assert_not_called()


def test_scheduling_preserves_mutable_status(job):
    session = Mock()
    session.request.side_effect = [
        response(
            200,
            {
                "items": [
                    {
                        "status": {
                            "privacyStatus": "private",
                            "license": "creativeCommon",
                            "embeddable": False,
                        }
                    }
                ]
            },
        ),
        response(200, {"status": {"publishAt": iso_time(job["publish_at"])}}),
    ]
    assert YouTube(session).schedule({**job, "video_id": "video_123"}, lambda: NOW)
    body = session.request.call_args.kwargs["json"]
    assert body["status"]["license"] == "creativeCommon"
    assert body["status"]["embeddable"] is False
    assert body["status"]["privacyStatus"] == "private"


def test_schedule_clock_checked_after_remote_lookup(job):
    session = Mock()
    session.request.return_value = response(
        200,
        {
            "items": [
                {
                    "status": {
                        "privacyStatus": "private",
                    }
                }
            ]
        },
    )
    assert not YouTube(session).schedule({**job, "video_id": "video_123"}, lambda: NOW + 3600)
    assert session.request.call_count == 1


def test_rejected_video_is_not_scheduled(job):
    session = Mock()
    session.request.return_value = response(
        200,
        {
            "items": [
                {
                    "status": {
                        "privacyStatus": "private",
                        "uploadStatus": "rejected",
                    }
                }
            ]
        },
    )
    with pytest.raises(APIError, match="rejected"):
        YouTube(session).schedule({**job, "video_id": "video_123"}, lambda: NOW)
    assert session.request.call_count == 1


def test_no_progress_returns_retry_instead_of_looping(job):
    session = Mock()
    session.request.return_value = response(308)
    with pytest.raises(APIError, match="no upload progress") as caught:
        YouTube(session).upload({**job, "session_url": SESSION})
    assert caught.value.retryable
    assert session.request.call_count == 2
