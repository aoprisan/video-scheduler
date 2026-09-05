import re
from datetime import UTC, datetime
from email.utils import parsedate_to_datetime
from pathlib import Path
from urllib.parse import urlparse

import requests
from google.auth.exceptions import GoogleAuthError

from .models import iso_time, parse_time, upload_body, utc_now

API = "https://www.googleapis.com/youtube/v3/videos"
UPLOAD_API = "https://www.googleapis.com/upload/youtube/v3/videos"
CHUNK_SIZE = 8 * 1024 * 1024  # A multiple of YouTube's required 256 KiB.
TIMEOUT = 60


class APIError(Exception):
    def __init__(self, message: str, *, retryable: bool = False, retry_after: float = 0):
        super().__init__(message)
        self.retryable = retryable
        self.retry_after = retry_after


def validate_session_url(url: str) -> str:
    parsed = urlparse(url)
    if (
        parsed.scheme != "https"
        or parsed.hostname != "www.googleapis.com"
        or parsed.port not in (None, 443)
        or parsed.username
        or parsed.password
        or not parsed.path.startswith("/upload/youtube/")
    ):
        raise APIError("YouTube returned an unexpected resumable upload URL")
    return url


def retry_delay(value: str) -> float:
    try:
        return max(0, float(value))
    except ValueError:
        try:
            return max(0, (parsedate_to_datetime(value) - datetime.now(UTC)).total_seconds())
        except (ValueError, TypeError):
            return 0


class YouTube:
    def __init__(self, session):
        self.session = session

    def request(self, method: str, url: str, **kwargs):
        try:
            response = self.session.request(
                method, url, timeout=TIMEOUT, allow_redirects=False, **kwargs
            )
        except requests.RequestException as exc:
            # requests exceptions can contain the sensitive resumable session URL.
            raise APIError(
                "Network request failed; the saved session will be reconciled", retryable=True
            ) from exc
        except GoogleAuthError as exc:
            raise APIError("Google authorization failed; run auth again") from exc
        if response.status_code in (200, 201, 308):
            return response
        reason = "unknown"
        try:
            reason = response.json()["error"]["errors"][0]["reason"]
        except (ValueError, KeyError, IndexError, TypeError):
            pass
        # Only record a short machine-readable reason, never raw HTTP bodies/URLs.
        if not isinstance(reason, str) or not re.fullmatch(r"[A-Za-z0-9_]{1,80}", reason):
            reason = "unknown"
        retryable = response.status_code in (408, 429, 500, 502, 503, 504) or reason in {
            "rateLimitExceeded",
            "userRateLimitExceeded",
            "backendError",
            "quotaExceeded",
        }
        if response.status_code in (404, 410) and url != API:
            raise APIError(
                "Upload session expired or was lost. Check YouTube Studio before "
                "creating another job; this job will not start a duplicate upload"
            )
        raise APIError(
            f"YouTube HTTP {response.status_code}: {reason}",
            retryable=retryable,
            retry_after=retry_delay(response.headers.get("Retry-After", "0")),
        )

    @staticmethod
    def body(response) -> dict:
        try:
            body = response.json()
        except ValueError as exc:
            raise APIError("YouTube returned invalid JSON", retryable=True) from exc
        if not isinstance(body, dict):
            raise APIError("YouTube returned an invalid resource", retryable=True)
        return body

    @classmethod
    def video_id(cls, response) -> str:
        video_id = cls.body(response).get("id")
        if not isinstance(video_id, str) or not re.fullmatch(r"[A-Za-z0-9_-]{1,128}", video_id):
            raise APIError("YouTube completion response has no video ID", retryable=True)
        return video_id

    def start(self, job: dict) -> str:
        response = self.request(
            "POST",
            UPLOAD_API,
            params={"uploadType": "resumable", "part": "snippet,status"},
            json=upload_body(job),
            headers={
                "X-Upload-Content-Length": str(job["size"]),
                "X-Upload-Content-Type": "application/octet-stream",
            },
        )
        url = response.headers.get("Location")
        if not url:
            raise APIError("YouTube did not return a resumable upload session", retryable=True)
        return validate_session_url(url)

    def upload(self, job: dict) -> str:
        url = validate_session_url(job["session_url"])
        total = job["size"]
        # Always ask YouTube for its offset. This also recovers a lost final response.
        response = self.request(
            "PUT",
            url,
            data=b"",
            headers={
                "Content-Length": "0",
                "Content-Range": f"bytes */{total}",
            },
        )
        with Path(job["path"]).open("rb") as file:
            while response.status_code == 308:
                delay = retry_delay(response.headers.get("Retry-After", "0"))
                if delay:
                    raise APIError(
                        "YouTube requested an upload pause", retryable=True, retry_after=delay
                    )
                received = response.headers.get("Range")
                match = re.fullmatch(r"bytes=0-(\d+)", received) if received else None
                if received and not match:
                    raise APIError("YouTube returned an invalid upload offset")
                offset = int(match.group(1)) + 1 if match else 0
                if not 0 <= offset < total:
                    raise APIError(
                        "YouTube returned an incomplete upload with invalid offset", retryable=True
                    )
                file.seek(offset)
                chunk = file.read(min(CHUNK_SIZE, total - offset))
                if not chunk:
                    raise APIError("Video file became shorter during upload")
                response = self.request(
                    "PUT",
                    url,
                    data=chunk,
                    headers={
                        "Content-Type": "application/octet-stream",
                        "Content-Length": str(len(chunk)),
                        "Content-Range": f"bytes {offset}-{offset + len(chunk) - 1}/{total}",
                    },
                )
                if response.status_code == 308:
                    new_range = response.headers.get("Range")
                    if new_range == received:
                        raise APIError("YouTube made no upload progress", retryable=True)
        return self.video_id(response)

    def get(self, video_id: str) -> dict:
        response = self.request("GET", API, params={"part": "status", "id": video_id})
        items = self.body(response).get("items", [])
        if not items:
            raise APIError("Uploaded video is not visible to this account; check YouTube Studio")
        return items[0]

    def schedule(self, job: dict, clock=utc_now) -> bool:
        """Return False for a missed deadline. Reconcile before any repeated update."""
        video = self.get(job["video_id"])
        status = video.get("status", {})
        if status.get("uploadStatus") in {"failed", "rejected", "deleted"}:
            raise APIError("YouTube rejected or failed to process the video; check YouTube Studio")
        scheduled = status.get("publishAt")
        if scheduled and parse_time(scheduled) == job["publish_at"]:
            return True
        # A previous scheduling response may have been lost and publication has since occurred.
        if status.get("privacyStatus") == "public":
            return True
        if status.get("privacyStatus") != "private":
            raise APIError("Video privacy changed outside this app; check YouTube Studio")
        if job["publish_at"] <= clock() + 60:
            return False
        mutable = {
            key: status[key]
            for key in (
                "embeddable",
                "license",
                "publicStatsViewable",
                "selfDeclaredMadeForKids",
                "containsSyntheticMedia",
            )
            if key in status
        }
        mutable.update(privacyStatus="private", publishAt=iso_time(job["publish_at"]))
        response = self.request(
            "PUT", API, params={"part": "status"}, json={"id": job["video_id"], "status": mutable}
        )
        returned = self.body(response).get("status", {})
        if returned.get("publishAt") and parse_time(returned["publishAt"]) == job["publish_at"]:
            return True
        raise APIError(
            "YouTube did not confirm the publication time; check YouTube Studio", retryable=True
        )
