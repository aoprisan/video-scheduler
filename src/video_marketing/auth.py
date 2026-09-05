import json
import os
import tempfile
from pathlib import Path

from google.auth.transport.requests import AuthorizedSession, Request
from google.oauth2.credentials import Credentials
from google_auth_oauthlib.flow import InstalledAppFlow

# videos.update requires this scope; youtube.upload alone cannot schedule after upload.
SCOPES = ["https://www.googleapis.com/auth/youtube.force-ssl"]


def save_credentials(path: Path, credentials: Credentials):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    descriptor, temporary = tempfile.mkstemp(dir=path.parent, prefix=".oauth-")
    try:
        with os.fdopen(descriptor, "w") as handle:
            handle.write(credentials.to_json())
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def login(client_secrets: Path, token_path: Path):
    config = json.loads(client_secrets.read_text())
    if "installed" not in config:
        raise ValueError("Create a Desktop app OAuth client and download its client JSON")
    flow = InstalledAppFlow.from_client_config(config, SCOPES, autogenerate_code_verifier=True)
    credentials = flow.run_local_server(
        host="localhost", port=0, access_type="offline", prompt="consent", timeout_seconds=300
    )
    if not credentials.refresh_token:
        raise ValueError("Google did not grant offline access; authorize again")
    save_credentials(token_path, credentials)


def authorized_session(token_path: Path) -> AuthorizedSession:
    if not token_path.exists():
        raise ValueError("No YouTube credentials. Run auth --client-secrets /path/to/client.json")
    credentials = Credentials.from_authorized_user_file(str(token_path))
    if not credentials.has_scopes(SCOPES):
        raise ValueError("Missing YouTube scope. Run auth again")
    if not credentials.valid:
        transport = Request()

        def bounded_request(*args, **kwargs):
            kwargs.setdefault("timeout", 60)
            return transport(*args, **kwargs)

        credentials.refresh(bounded_request)
        save_credentials(token_path, credentials)
    return AuthorizedSession(credentials, refresh_timeout=60)
