import hashlib
import os
from datetime import UTC, datetime
from pathlib import Path


def utc_now() -> float:
    return datetime.now(UTC).timestamp()


def parse_time(value: str) -> float:
    try:
        date = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as exc:
        raise ValueError("Use an ISO 8601 date, e.g. 2026-10-01T18:00:00+02:00") from exc
    if date.tzinfo is None or date.utcoffset() is None:
        raise ValueError("Dates must include a timezone offset or Z")
    return date.timestamp()


def iso_time(timestamp: float) -> str:
    return datetime.fromtimestamp(timestamp, UTC).isoformat().replace("+00:00", "Z")


def fingerprint(path: Path) -> tuple[int, str]:
    with path.open("rb") as file:
        before = os.fstat(file.fileno())
        digest = hashlib.file_digest(file, "sha256").hexdigest()
        after = os.fstat(file.fileno())
    if (before.st_size, before.st_mtime_ns) != (after.st_size, after.st_mtime_ns):
        raise ValueError("Video file changed while reading it")
    size = after.st_size
    if not size:
        raise ValueError("Video file must not be empty")
    return size, digest


def validate_metadata(title: str, description: str, tags: list[str], category: str) -> None:
    if not title.strip() or len(title) > 100 or any(c in title for c in "<>"):
        raise ValueError("Title must be 1–100 characters and cannot contain < or >")
    if len(description.encode("utf-8")) > 5000 or any(c in description for c in "<>"):
        raise ValueError("Description must be at most 5000 UTF-8 bytes without < or >")
    # YouTube counts separators and implied quotation marks around tags with spaces.
    tag_length = sum(len(tag) + (2 if " " in tag else 0) for tag in tags)
    if any(not tag.strip() for tag in tags) or tag_length + max(0, len(tags) - 1) > 500:
        raise ValueError("Tags must be nonempty and total at most 500 characters")
    if not category.isdigit() or int(category) < 1:
        raise ValueError("Category must be a positive YouTube category ID")


def upload_body(job: dict) -> dict:
    return {
        "snippet": {
            "title": job["title"],
            "description": job["description"],
            "tags": job["tags"],
            "categoryId": job["category"],
        },
        "status": {
            "privacyStatus": "private",
            "selfDeclaredMadeForKids": bool(job["made_for_kids"]),
            "containsSyntheticMedia": bool(job["synthetic_media"]),
        },
    }
