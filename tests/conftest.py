import json

import pytest
import requests

from video_marketing.models import parse_time
from video_marketing.store import Store
from video_marketing.youtube import API, UPLOAD_API

NOW = parse_time("2030-01-01T00:00:00Z")
SESSION = "https://www.googleapis.com/upload/youtube/v3/videos?upload_id=sensitive-session"


def response(code, body=None, **headers):
    result = requests.Response()
    result.status_code = code
    result._content = json.dumps(body or {}).encode()
    result.headers.update(headers)
    return result


class FakeYouTubeHTTP:
    """Stateful HTTP peer: owns upload offsets and remote video state across worker restarts."""

    def __init__(self):
        self.calls = []
        self.received = bytearray()
        self.total = None
        self.status = None
        self.lose_upload_response = False
        self.lose_schedule_response = False
        self.fail_schedule_once = False

    def request(self, method, url, **kwargs):
        self.calls.append((method, url, kwargs))
        if method == "POST" and url == UPLOAD_API:
            self.total = int(kwargs["headers"]["X-Upload-Content-Length"])
            self.status = dict(kwargs["json"]["status"])
            assert self.status["privacyStatus"] == "private"
            assert "publishAt" not in self.status
            return response(200, Location=SESSION)
        if method == "PUT" and url == SESSION:
            data = kwargs["data"]
            if data:
                start = int(kwargs["headers"]["Content-Range"].split()[1].split("-")[0])
                assert start == len(self.received)
                self.received.extend(data)
            if len(self.received) == self.total:
                self.status["uploadStatus"] = "uploaded"
                if data and self.lose_upload_response:
                    self.lose_upload_response = False
                    raise requests.ConnectionError("sensitive-session URL must not be logged")
                return response(200, {"id": "video_123", "status": self.status})
            headers = {"Range": f"bytes=0-{len(self.received) - 1}"} if self.received else {}
            return response(308, **headers)
        if method == "GET" and url == API:
            return response(200, {"items": [{"id": "video_123", "status": self.status}]})
        if method == "PUT" and url == API:
            if self.fail_schedule_once:
                self.fail_schedule_once = False
                return response(503)
            self.status.update(kwargs["json"]["status"])
            if self.lose_schedule_response:
                self.lose_schedule_response = False
                raise requests.Timeout("lost schedule response")
            return response(200, {"id": "video_123", "status": self.status})
        raise AssertionError(f"Unexpected request: {method} {url}")


@pytest.fixture
def store(tmp_path):
    value = Store(tmp_path / "data")
    yield value
    value.close()


@pytest.fixture
def job(store, tmp_path):
    video = tmp_path / "sample.mp4"
    video.write_bytes(b"sample video bytes" * 40)
    return store.add(
        path=video,
        title="A demo",
        description="Description",
        tags=["demo"],
        category="22",
        made_for_kids=False,
        synthetic_media=True,
        upload_at=NOW,
        publish_at=NOW + 3600,
        now=NOW,
    )


@pytest.fixture
def remote():
    return FakeYouTubeHTTP()
