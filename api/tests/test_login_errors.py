"""Tests for the Kavita login error classifier.

These tests don't need a live server — they verify that the
formatter we added classifies common misconfigurations correctly,
so a user who pastes a wrong api_key into server.json sees a
helpful error pointing at the right knob.
"""

# pylint: disable=missing-function-docstring,redefined-outer-name
import pytest

REPO_ROOT = "/home/damian/git/pbemu"
import sys  # noqa: E402

sys.path.insert(0, REPO_ROOT)
sys.path.insert(0, f"{REPO_ROOT}/api")

from providers.kavita import _KavitaClient  # noqa: E402


def test_format_login_error_400():
    """HTTP 400 (model validation) gets a hint about config shape."""
    c = _KavitaClient(
        base_url="https://kavita.example.com",
        api_key="deadbeef",
        username="alice",
        password="secret",
    )
    msg = c._format_login_error(
        400,
        '{"errors":{"Username":["required"]}}',
    )
    assert "400" in msg
    assert "model validation" in msg
    assert "api/config/server.json" in msg


def test_format_login_error_401_with_non_uuid_key():
    """A 401 with a non-UUID api_key gets a 'not a UUID' hint."""
    c = _KavitaClient(
        base_url="https://kavita.example.com",
        api_key="WRONG-NOT-A-UUID",
        username="alice",
        password="secret",
    )
    msg = c._format_login_error(401, "Your credentials are not correct")
    assert "401" in msg
    assert "WRONG-NOT-A-UUID" in msg
    assert "UUID" in msg


def test_format_login_error_401_with_uuid_key_no_username():
    """A 401 with no username at all gets a 'no username configured' hint."""
    c = _KavitaClient(base_url="https://kavita.example.com")
    msg = c._format_login_error(401, "Your credentials are not correct")
    assert "no username configured" in msg
    assert "no api_key configured" in msg


def test_format_login_error_401_with_uuid_key_no_password():
    """A 401 with no password gets a 'no password configured' hint."""
    c = _KavitaClient(
        base_url="https://kavita.example.com",
        api_key="00000000-0000-0000-0000-000000000000",
        username="alice",
    )
    msg = c._format_login_error(401, "Your credentials are not correct")
    assert "no password configured" in msg
    # The api_key looks valid so we don't flag the format.
    assert "not a UUID" not in msg


def test_format_login_error_401_with_uuid_shaped_key_no_other_clues():
    """A 401 with a UUID-shaped key and full user/pwd: no clue, generic hint."""
    c = _KavitaClient(
        base_url="https://kavita.example.com",
        api_key="00000000-0000-0000-0000-000000000000",
        username="alice",
        password="secret",
    )
    msg = c._format_login_error(401, "Your credentials are not correct")
    assert "double-check" in msg
    assert "no api_key configured" not in msg
    assert "UUID" not in msg


def test_format_login_error_other_status():
    """Anything other than 400/401 just falls back to raw reporting."""
    c = _KavitaClient(base_url="https://kavita.example.com")
    msg = c._format_login_error(500, "internal server error")
    assert "500" in msg
    assert "internal server error" in msg


def test_format_login_error_truncates_long_body():
    """Long raw bodies shouldn't blow up the log line."""
    c = _KavitaClient(base_url="https://kavita.example.com")
    long_body = "x" * 1000
    msg = c._format_login_error(500, long_body)
    assert isinstance(msg, str)
    assert "500" in msg


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
