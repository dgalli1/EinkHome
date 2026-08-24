"""Tests for the CLI / env-var config overrides in api.api.server."""

# pylint: disable=missing-function-docstring,redefined-outer-name
import argparse
import os
import tempfile

import pytest

REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
import sys  # noqa: E402

sys.path.insert(0, REPO_ROOT)
sys.path.insert(0, os.path.join(REPO_ROOT, "api"))

from api.api.server import (  # noqa: E402
    _apply_runtime_overrides,
    _coerce_env_value,
    build_default_app,
)


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


# --- build_default_app: volatile-ledger relocation ----------------------


def _bootstrap_cfg(tmp_path, **extra):
    cfg = {
        "host": "127.0.0.1",
        "port": 0,
        "provider": "mock",
        "providers": {"mock": {"kind": "mock", "books_dir": str(tmp_path)}},
        "cover_cache_dir": str(tmp_path / ".cover-cache"),
        "suggestions": False,
    }
    cfg.update(extra)
    return cfg


def test_default_ledger_in_tmpdir_relocates_next_to_config(tmp_path, capsys):
    """A default ledger path landing under the system tempdir is
    relocated next to the config file so sync history survives reboots.
    (When the config itself is also volatile a second warning fires but
    the relocation still happens.)"""
    config_path = str(tmp_path / "server.json")
    app = build_default_app(_bootstrap_cfg(tmp_path), config_path=config_path)
    try:
        relocated = tmp_path / "server.json-ledger.db"
        assert app.ledger is not None
        assert relocated.exists()
        err = capsys.readouterr().err
        assert f"relocating to {relocated}" in err
        # The relocated path is still inside volatile storage here —
        # the code says so rather than staying silent.
        assert "final path" in err and "volatile" in err
    finally:
        if app.ledger is not None:
            app.ledger.close()


def test_explicit_volatile_ledger_kept_with_warning(tmp_path, capsys):
    """An explicitly configured volatile path is respected (not moved)
    but warned about."""
    ledger_path = tmp_path / "sync-ledger.db"
    app = build_default_app(_bootstrap_cfg(tmp_path, ledger={"path": str(ledger_path)}))
    try:
        assert app.ledger is not None
        assert ledger_path.exists()
        err = capsys.readouterr().err
        assert f"configured path {ledger_path} is volatile" in err
        assert "relocating" not in err
    finally:
        if app.ledger is not None:
            app.ledger.close()


def test_explicit_nonvolatile_ledger_untouched(tmp_path, monkeypatch, capsys):
    """An explicit path outside the tempdir gets no warning and no
    relocation; only the tempdir prefix counts as volatile."""
    fake_tmp = tmp_path / "volatile"
    fake_tmp.mkdir()
    monkeypatch.setattr(tempfile, "gettempdir", lambda: str(fake_tmp))
    ledger_path = tmp_path / "durable" / "ledger.db"
    app = build_default_app(_bootstrap_cfg(tmp_path, ledger={"path": str(ledger_path)}))
    try:
        assert app.ledger is not None
        assert ledger_path.exists()
        assert "ledger:" not in capsys.readouterr().err
    finally:
        if app.ledger is not None:
            app.ledger.close()


def test_corrupt_ledger_degrades_to_none(tmp_path, capsys):
    """A garbage ledger DB fails to open with sqlite3.Error; the app
    boots anyway with ledger=None (sync/delta then serves 503)."""
    ledger_path = tmp_path / "sync-ledger.db"
    ledger_path.write_bytes(b"this is definitely not sqlite" * 8)
    app = build_default_app(_bootstrap_cfg(tmp_path, ledger={"path": str(ledger_path)}))
    assert app.ledger is None
    assert f"cannot open {ledger_path}" in capsys.readouterr().err


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
