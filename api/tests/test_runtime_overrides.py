"""Tests for the CLI / env-var config overrides in api.api.server."""

# pylint: disable=missing-function-docstring,redefined-outer-name
import argparse
import os

import pytest

REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
import sys  # noqa: E402

sys.path.insert(0, REPO_ROOT)
sys.path.insert(0, os.path.join(REPO_ROOT, "api"))

from api.api.server import _apply_runtime_overrides, _coerce_env_value  # noqa: E402


def _args(**kwargs):
    """Return a simple Namespace with None defaults for missing keys."""
    defaults = {"host": None, "port": None, "provider": None}
    defaults.update(kwargs)
    return argparse.Namespace(**defaults)


def test_no_overrides_returns_unchanged():
    cfg = {"host": "0.0.0.0", "port": 8765, "provider": "mock"}
    out = _apply_runtime_overrides(cfg, _args())
    assert out == cfg


def test_cli_flag_overrides_config():
    cfg = {"host": "0.0.0.0", "port": 8765, "provider": "mock"}
    out = _apply_runtime_overrides(cfg, _args(provider="kavita"))
    assert out["provider"] == "kavita"


def test_env_var_overrides_config(monkeypatch):
    cfg = {"host": "0.0.0.0", "port": 8765, "provider": "mock"}
    monkeypatch.setenv("PBEMU_PROVIDER", "kavita")
    out = _apply_runtime_overrides(cfg, _args())
    assert out["provider"] == "kavita"


def test_cli_flag_beats_env_var(monkeypatch):
    """The CLI flag is the highest-priority override."""
    cfg = {"provider": "mock"}
    monkeypatch.setenv("PBEMU_PROVIDER", "mock")
    out = _apply_runtime_overrides(cfg, _args(provider="kavita"))
    assert out["provider"] == "kavita"


def test_empty_env_var_does_not_override(monkeypatch):
    """Empty strings should be treated as 'not set'."""
    cfg = {"provider": "mock"}
    monkeypatch.setenv("PBEMU_PROVIDER", "")
    out = _apply_runtime_overrides(cfg, _args())
    assert out["provider"] == "mock"


def test_provider_env_overrides(monkeypatch):
    """PBEMU_<PROVIDER>_<FIELD> override the provider's own config block."""
    cfg = {
        "provider": "kavita",
        "providers": {
            "kavita": {
                "base_url": "https://kavita.example.com",
                "api_key": "old-key",
                "timeout": 30,
            }
        },
    }
    monkeypatch.setenv("PBEMU_KAVITA_BASE_URL", "https://kavita.other.example")
    monkeypatch.setenv("PBEMU_KAVITA_API_KEY", "new-key-from-env")
    monkeypatch.setenv("PBEMU_KAVITA_TIMEOUT", "120")
    out = _apply_runtime_overrides(cfg, _args())
    assert out["providers"]["kavita"]["base_url"] == "https://kavita.other.example"
    assert out["providers"]["kavita"]["api_key"] == "new-key-from-env"
    assert out["providers"]["kavita"]["timeout"] == 120  # coerced to int


def test_provider_env_overrides_create_block():
    """If the provider block doesn't exist, env vars create it."""
    cfg = {"provider": "kavita", "providers": {}}
    monkeypatch_ = pytest.MonkeyPatch()
    monkeypatch_.setenv("PBEMU_KAVITA_BASE_URL", "https://kavita.example.com")
    try:
        out = _apply_runtime_overrides(cfg, _args())
        assert out["providers"]["kavita"]["base_url"] == "https://kavita.example.com"
    finally:
        monkeypatch_.undo()


def test_provider_env_only_affects_active_provider(monkeypatch):
    """PBEMU_<PROVIDER>_* env vars are scoped to the active provider.

    With `provider: mock` selected, PBEMU_KAVITA_* must NOT mutate the
    kavita block — and vice versa.
    """
    cfg = {
        "provider": "mock",
        "providers": {
            "mock": {"books_dir": "/tmp/books"},
            "kavita": {"base_url": "https://kavita.example.com"},
        },
    }
    monkeypatch.setenv("PBEMU_KAVITA_BASE_URL", "https://env.example.com")
    out = _apply_runtime_overrides(cfg, _args())
    # kavita block is unchanged because the active provider is mock
    assert out["providers"]["mock"] == {"books_dir": "/tmp/books"}
    assert out["providers"]["kavita"]["base_url"] == "https://kavita.example.com"

    # Now switch the active provider to kavita; the env var should land.
    cfg2 = {
        "provider": "kavita",
        "providers": {
            "kavita": {"base_url": "https://kavita.example.com"},
        },
    }
    out2 = _apply_runtime_overrides(cfg2, _args())
    assert out2["providers"]["kavita"]["base_url"] == "https://env.example.com"


def test_coerce_env_value():
    assert _coerce_env_value("true") is True
    assert _coerce_env_value("True") is True
    assert _coerce_env_value("yes") is True
    assert _coerce_env_value("1") is True
    assert _coerce_env_value("false") is False
    assert _coerce_env_value("0") is False
    assert _coerce_env_value("no") is False
    assert _coerce_env_value("42") == 42
    assert _coerce_env_value("-7") == -7
    assert _coerce_env_value("hello") == "hello"
    assert (
        _coerce_env_value("https://kavita.example.com") == "https://kavita.example.com"
    )


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
