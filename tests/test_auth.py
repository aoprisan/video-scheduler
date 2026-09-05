import json
from unittest.mock import Mock

import pytest
from google.oauth2.credentials import Credentials

from video_marketing import auth


def test_login_requests_offline_access_and_persists_credentials(tmp_path, monkeypatch):
    client = tmp_path / "client.json"
    client.write_text(json.dumps({"installed": {"client_id": "client"}}))
    credentials = Credentials(
        token="token",
        refresh_token="refresh",
        client_id="client",
        client_secret="secret",
        token_uri="https://oauth2.googleapis.com/token",
        scopes=auth.SCOPES,
    )
    flow = Mock()
    flow.run_local_server.return_value = credentials
    factory = Mock(return_value=flow)
    monkeypatch.setattr(auth.InstalledAppFlow, "from_client_config", factory)
    token = tmp_path / "token.json"
    auth.login(client, token)
    assert factory.call_args.kwargs["autogenerate_code_verifier"] is True
    assert flow.run_local_server.call_args.kwargs["access_type"] == "offline"
    assert flow.run_local_server.call_args.kwargs["host"] == "localhost"
    assert json.loads(token.read_text())["refresh_token"] == "refresh"
    assert token.stat().st_mode & 0o777 == 0o600


def test_expired_credentials_refresh_and_are_saved(tmp_path, monkeypatch):
    token = tmp_path / "token.json"
    token.touch()
    credentials = Mock(valid=False)
    credentials.has_scopes.return_value = True
    credentials.to_json.return_value = '{"token": "refreshed"}'
    credentials.refresh.side_effect = lambda request: request("https://oauth2.googleapis.com/token")
    transport = Mock()
    monkeypatch.setattr(
        auth.Credentials, "from_authorized_user_file", Mock(return_value=credentials)
    )
    monkeypatch.setattr(auth, "Request", Mock(return_value=transport))
    session = Mock()
    monkeypatch.setattr(auth, "AuthorizedSession", session)
    auth.authorized_session(token)
    assert transport.call_args.kwargs["timeout"] == 60
    assert json.loads(token.read_text()) == {"token": "refreshed"}
    session.assert_called_once_with(credentials, refresh_timeout=60)


def test_upload_only_scope_rejected_before_worker_starts(tmp_path, monkeypatch):
    token = tmp_path / "token.json"
    token.touch()
    credentials = Mock()
    credentials.has_scopes.return_value = False
    monkeypatch.setattr(
        auth.Credentials, "from_authorized_user_file", Mock(return_value=credentials)
    )
    with pytest.raises(ValueError, match="Missing YouTube scope"):
        auth.authorized_session(token)
    credentials.refresh.assert_not_called()
